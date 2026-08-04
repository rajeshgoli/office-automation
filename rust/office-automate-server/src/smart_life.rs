//! Shared Smart Life (Tuya sharing API) client.
//!
//! The auth cache, token refresh, signing, and AES-GCM payload crypto are all
//! device-agnostic, so blinds and the ERV share this module. Token refresh
//! *rotates the refresh token*, which means two devices refreshing concurrently
//! against the same cache file can clobber each other and lose cloud auth for
//! both. Every load/refresh/save cycle therefore runs under `AUTH_CACHE_LOCK`.

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use aes_gcm::{
    Aes128Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// Shared Home Assistant Tuya integration identifier. Not a secret, but a
/// third-party constant that has changed before, so it lives in config with
/// this value as the default.
pub const DEFAULT_CLIENT_ID: &str = "HA_3y9q4ak7g4ephrvke";

const DEFAULT_AUTH_FILE: &str = ".office-automate/tuya-sharing-auth.json";
const SCENE_TRIGGER_PATH: &str = "/v1.0/m/scene/ha/trigger";
const SCENE_LIST_PATH: &str = "/v1.0/m/scene/ha/home/scenes";
const DEVICE_DETAIL_PATH: &str = "/v1.0/m/life/ha/devices/detail";

/// Serializes the whole load -> refresh -> save cycle across every caller in
/// the process. See the module docs for why this is not optional.
static AUTH_CACHE_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

type HmacSha256 = Hmac<Sha256>;

pub fn default_auth_file() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_AUTH_FILE)
}

/// Resolve a configured auth-file override against the shared default path.
pub fn auth_file_or_default(configured: Option<&PathBuf>) -> PathBuf {
    configured.cloned().unwrap_or_else(default_auth_file)
}

pub struct SmartLifeClient {
    http: Client,
    client_id: String,
    auth_file: PathBuf,
}

impl SmartLifeClient {
    pub fn new(client_id: impl Into<String>, auth_file: PathBuf) -> Self {
        Self {
            http: Client::new(),
            client_id: client_id.into(),
            auth_file,
        }
    }

    pub async fn get(&self, path: &str, params: Option<Value>) -> Result<Value> {
        self.call("GET", path, params, None).await
    }

    pub async fn post(
        &self,
        path: &str,
        params: Option<Value>,
        body: Option<Value>,
    ) -> Result<Value> {
        self.call("POST", path, params, body).await
    }

    /// Run a tap-to-run scene. The trigger endpoint only names the scene; what
    /// the scene does is stored server-side and is not constrained by what the
    /// (lossy) scene read view can display.
    pub async fn trigger_scene(&self, home_id: &str, scene_id: &str) -> Result<()> {
        self.post(
            SCENE_TRIGGER_PATH,
            None,
            Some(json!({"homeId": home_id, "sceneId": scene_id})),
        )
        .await
        .map(|_| ())
    }

    /// List the scene ids in a home.
    ///
    /// Existence only. The rendered *actions* of a scene are lossy -- the
    /// sharing API hides the private speed data points there exactly as it
    /// hides them in device status -- so nothing may be concluded from them
    /// about what a scene does. An id being present is still worth knowing:
    /// a deleted or mistyped one fails at the first ventilation request.
    pub async fn list_scene_ids(&self, home_id: &str) -> Result<Vec<String>> {
        let result = self
            .get(SCENE_LIST_PATH, Some(json!({"homeId": home_id})))
            .await?;
        let scenes = result
            .as_array()
            .ok_or_else(|| anyhow!("Smart Life scene list is not an array"))?;
        Ok(scenes
            .iter()
            .filter_map(|scene| {
                ["scene_id", "sceneId", "id"]
                    .iter()
                    .find_map(|key| scene.get(*key).and_then(Value::as_str))
                    .map(str::to_string)
            })
            .collect())
    }

    /// Read a device's cloud status codes. The sharing API exposes only the
    /// standard codes (`switch`, `mode`, air-quality values); private data
    /// points such as the ERV speed DPs are never returned.
    pub async fn device_status(&self, device_id: &str) -> Result<Value> {
        let result = self
            .get(DEVICE_DETAIL_PATH, Some(json!({"devIds": device_id})))
            .await?;
        device_status_codes(&result, device_id)
            .ok_or_else(|| anyhow!("Smart Life returned no status for device {device_id}"))
    }

    /// Read-only credential check: the cache is present, parseable, and its
    /// refresh token is usable. Issues no device command.
    ///
    /// Unconditionally refreshes rather than deferring to
    /// `refresh_auth_if_needed`'s expiry check. A cached access token with
    /// time left on it would otherwise let this pass while the refresh token
    /// behind it is dead -- exactly the "credentials are fine until the day
    /// they aren't" gap this check exists to close.
    pub async fn check_credentials(&self) -> Result<String> {
        let _auth_guard = AUTH_CACHE_LOCK.lock().await;
        let mut auth = SmartLifeAuthCache::load(&self.auth_file)?;
        self.refresh_auth(&mut auth)
            .await
            .context("Smart Life refresh token is not usable")?;
        auth.save(&self.auth_file)?;
        Ok(auth.endpoint.clone())
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        params: Option<Value>,
        body: Option<Value>,
    ) -> Result<Value> {
        let _auth_guard = AUTH_CACHE_LOCK.lock().await;
        let mut auth = SmartLifeAuthCache::load(&self.auth_file)?;
        if self.refresh_auth_if_needed(&mut auth).await? {
            auth.save(&self.auth_file)?;
        }
        self.request(&auth, method, path, params, body).await
    }

    /// Returns true when the cache was rotated and needs persisting.
    async fn refresh_auth_if_needed(&self, auth: &mut SmartLifeAuthCache) -> Result<bool> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expires_at = auth.token_info.t + auth.token_info.expire_time * 1_000;
        if expires_at - 60_000 > now_ms {
            return Ok(false);
        }
        self.refresh_auth(auth).await?;
        Ok(true)
    }

    /// Unconditionally rotate the access and refresh tokens. Callers that
    /// only need to avoid an unnecessary round trip should go through
    /// `refresh_auth_if_needed`; this is for the cases -- like
    /// `check_credentials` -- where skipping the call because the cached
    /// access token isn't expired yet would defeat the point.
    async fn refresh_auth(&self, auth: &mut SmartLifeAuthCache) -> Result<()> {
        let path = format!("/v1.0/m/token/{}", auth.token_info.refresh_token);
        let result = self
            .request(auth, "GET", &path, None, None)
            .await
            .context("failed to refresh Smart Life access token")?;
        auth.token_info = SmartLifeTokenInfo {
            t: chrono::Utc::now().timestamp_millis(),
            expire_time: result
                .get("expireTime")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("Smart Life token refresh missing expireTime"))?,
            uid: result
                .get("uid")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Smart Life token refresh missing uid"))?
                .to_string(),
            access_token: result
                .get("accessToken")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Smart Life token refresh missing accessToken"))?
                .to_string(),
            refresh_token: result
                .get("refreshToken")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Smart Life token refresh missing refreshToken"))?
                .to_string(),
        };
        Ok(())
    }

    async fn request(
        &self,
        auth: &SmartLifeAuthCache,
        method: &str,
        path: &str,
        params: Option<Value>,
        body: Option<Value>,
    ) -> Result<Value> {
        let request_id = Uuid::new_v4().to_string();
        let hash_key = format!(
            "{:x}",
            md5::compute(format!("{}{}", request_id, auth.token_info.refresh_token))
        );
        let secret = smart_life_secret(&request_id, "", &hash_key)?;

        let query_encdata = match params {
            Some(params) => Some(encrypt_json(&params, &secret)?),
            None => None,
        };
        let body_encdata = match body {
            Some(body) => Some(encrypt_json(&body, &secret)?),
            None => None,
        };
        let now_ms = chrono::Utc::now().timestamp_millis().to_string();
        let sign = smart_life_sign(
            &self.client_id,
            &hash_key,
            query_encdata.as_deref().unwrap_or_default(),
            body_encdata.as_deref().unwrap_or_default(),
            &auth.token_info.access_token,
            &request_id,
            &now_ms,
        )?;

        let url = format!("{}{}", auth.endpoint, path);
        let mut request = match method {
            "GET" => self.http.get(url),
            "POST" => self.http.post(url),
            _ => bail!("unsupported Smart Life method {method}"),
        }
        .header("X-appKey", &self.client_id)
        .header("X-requestId", request_id)
        .header("X-sid", "")
        .header("X-time", now_ms)
        .header("X-token", &auth.token_info.access_token)
        .header("X-sign", sign);

        if let Some(query_encdata) = query_encdata {
            request = request.query(&[("encdata", query_encdata)]);
        }
        if let Some(body_encdata) = body_encdata {
            request = request.json(&json!({ "encdata": body_encdata }));
        }

        let response = request
            .send()
            .await
            .context("Smart Life request failed")?
            .error_for_status()
            .context("Smart Life HTTP error")?;
        let envelope: SmartLifeApiEnvelope = response
            .json()
            .await
            .context("failed to parse Smart Life response")?;
        if !envelope.success {
            bail!(
                "Smart Life API error code={} msg={}",
                envelope
                    .code
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| "unknown".to_string()),
                envelope
                    .msg
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_else(|| "unknown".to_string())
            );
        }

        match envelope.result {
            Some(Value::String(encrypted)) => decrypt_result(&encrypted, &secret),
            Some(value) => Ok(value),
            None => Ok(Value::Null),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SmartLifeAuthCache {
    user_code: String,
    terminal_id: String,
    endpoint: String,
    token_info: SmartLifeTokenInfo,
}

impl SmartLifeAuthCache {
    fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read Smart Life auth cache {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse Smart Life auth cache {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)
            .context("failed to serialize Smart Life auth cache")?;
        fs::write(path, format!("{content}\n"))
            .with_context(|| format!("failed to write Smart Life auth cache {}", path.display()))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SmartLifeTokenInfo {
    t: i64,
    expire_time: i64,
    uid: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct SmartLifeApiEnvelope {
    success: bool,
    result: Option<Value>,
    code: Option<Value>,
    msg: Option<Value>,
}

/// Pull the status codes for one device out of a `devices/detail` result. The
/// endpoint answers with a bare list, or a wrapper object, depending on how
/// many devices were asked for, so both shapes are accepted.
fn device_status_codes(result: &Value, device_id: &str) -> Option<Value> {
    let devices = match result {
        Value::Array(devices) => devices.clone(),
        Value::Object(object) => ["devices", "list", "data"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_array).cloned())
            .unwrap_or_else(|| vec![result.clone()]),
        _ => return None,
    };

    devices
        .into_iter()
        .find(|device| {
            ["id", "devId", "device_id"]
                .iter()
                .filter_map(|key| device.get(*key).and_then(Value::as_str))
                .any(|value| value == device_id)
        })
        .and_then(|device| device.get("status").cloned())
}

/// Read one status code out of the `status` payload. It arrives either as a
/// `{code, value}` list or as a flat code -> value map.
pub fn status_code_value<'a>(status: &'a Value, code: &str) -> Option<&'a Value> {
    if let Some(entries) = status.as_array() {
        return entries
            .iter()
            .find(|entry| entry.get("code").and_then(Value::as_str) == Some(code))
            .and_then(|entry| entry.get("value"));
    }
    status.get(code)
}

fn encrypt_json(value: &Value, secret: &str) -> Result<String> {
    let raw = serde_json::to_string(value).context("failed to encode Smart Life payload")?;
    let nonce = random_nonce();
    let cipher = Aes128Gcm::new_from_slice(secret.as_bytes())
        .map_err(|error| anyhow!("invalid Smart Life AES key: {error}"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(nonce.as_bytes()), raw.as_bytes())
        .map_err(|error| anyhow!("failed to encrypt Smart Life payload: {error}"))?;
    Ok(format!(
        "{}{}",
        BASE64.encode(nonce.as_bytes()),
        BASE64.encode(ciphertext)
    ))
}

fn decrypt_result(encrypted: &str, secret: &str) -> Result<Value> {
    let bytes = BASE64
        .decode(encrypted)
        .context("failed to decode Smart Life response")?;
    if bytes.len() < 12 {
        bail!("Smart Life response is too short");
    }
    let (nonce, ciphertext) = bytes.split_at(12);
    let cipher = Aes128Gcm::new_from_slice(secret.as_bytes())
        .map_err(|error| anyhow!("invalid Smart Life AES key: {error}"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|error| anyhow!("failed to decrypt Smart Life response: {error}"))?;
    let text = String::from_utf8(plaintext).context("Smart Life response is not valid UTF-8")?;
    serde_json::from_str(&text).or_else(|_| Ok(Value::String(text)))
}

fn random_nonce() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTWXYZabcdefhijkmnprstwxyz2345678";
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn smart_life_secret(request_id: &str, sid: &str, hash_key: &str) -> Result<String> {
    let mut message = hash_key.to_string();
    if !sid.is_empty() {
        let mut ecode = String::new();
        for character in sid.chars().take(16) {
            let index = character as usize % 16;
            let selected = sid
                .chars()
                .nth(index)
                .ok_or_else(|| anyhow!("invalid Smart Life sid"))?;
            ecode.push(selected);
        }
        message.push('_');
        message.push_str(&ecode);
    }

    let mut mac = <HmacSha256 as Mac>::new_from_slice(request_id.as_bytes())
        .map_err(|error| anyhow!("invalid Smart Life HMAC key: {error}"))?;
    mac.update(message.as_bytes());
    let digest = mac.finalize().into_bytes();
    Ok(to_lower_hex(&digest)[..16].to_string())
}

fn smart_life_sign(
    client_id: &str,
    hash_key: &str,
    query_encdata: &str,
    body_encdata: &str,
    access_token: &str,
    request_id: &str,
    timestamp_ms: &str,
) -> Result<String> {
    let mut sign = format!(
        "X-appKey={client_id}||X-requestId={request_id}||X-time={timestamp_ms}||X-token={access_token}"
    );
    sign.push_str(query_encdata);
    sign.push_str(body_encdata);

    let mut mac = <HmacSha256 as Mac>::new_from_slice(hash_key.as_bytes())
        .map_err(|error| anyhow!("invalid Smart Life sign key: {error}"))?;
    mac.update(sign.as_bytes());
    Ok(to_lower_hex(&mac.finalize().into_bytes()))
}

fn to_lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_life_crypto_matches_sdk_vectors() {
        let secret = smart_life_secret(
            "7aeb69e7-0132-4553-9eaf-8df515d7e6de",
            "",
            "4b7d14dca2d3df16b3a12219a47a7e56",
        )
        .expect("secret");
        assert_eq!(secret, "60437f64b09370c5");

        let sign = smart_life_sign(
            DEFAULT_CLIENT_ID,
            "4b7d14dca2d3df16b3a12219a47a7e56",
            "",
            "body-encdata",
            "access-token",
            "7aeb69e7-0132-4553-9eaf-8df515d7e6de",
            "1783144312258",
        )
        .expect("sign");
        assert_eq!(
            sign,
            "0b01f17ddcba9cd54e0305cdd53607ae3b877d88f84690fc406ee719fae94871"
        );
    }

    #[test]
    fn signs_with_the_configured_client_id() {
        let default_sign = smart_life_sign(
            DEFAULT_CLIENT_ID,
            "hash-key",
            "",
            "",
            "access-token",
            "request-id",
            "1783144312258",
        )
        .expect("sign");
        let custom_sign = smart_life_sign(
            "HA_other_client",
            "hash-key",
            "",
            "",
            "access-token",
            "request-id",
            "1783144312258",
        )
        .expect("sign");

        assert_ne!(default_sign, custom_sign);
    }

    #[test]
    fn reads_device_status_from_both_response_shapes() {
        let listed = json!([
            {"id": "other-device", "status": [{"code": "switch", "value": false}]},
            {"id": "erv-device", "status": [{"code": "switch", "value": true}]},
        ]);
        let status = device_status_codes(&listed, "erv-device").expect("status");
        assert_eq!(status_code_value(&status, "switch"), Some(&json!(true)));

        let wrapped = json!({"devices": [{"devId": "erv-device", "status": {"switch": false}}]});
        let status = device_status_codes(&wrapped, "erv-device").expect("status");
        assert_eq!(status_code_value(&status, "switch"), Some(&json!(false)));

        assert!(device_status_codes(&listed, "missing-device").is_none());
    }

    /// Token refresh rotates the refresh token. Two devices sharing the cache
    /// file must not interleave load/save, or one rotation is lost and cloud
    /// auth dies for both.
    #[tokio::test]
    async fn auth_cache_lock_serializes_concurrent_rotations() {
        async fn rotate(path: PathBuf) {
            let _auth_guard = AUTH_CACHE_LOCK.lock().await;
            let mut auth = SmartLifeAuthCache::load(&path).expect("load");
            // Stand in for the round trip a real refresh makes between reading
            // the cache and writing the rotated token back.
            tokio::task::yield_now().await;
            auth.token_info.refresh_token.push('+');
            auth.save(&path).expect("save");
        }

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join("tuya-sharing-auth.json");
        let auth = SmartLifeAuthCache {
            user_code: "user".to_string(),
            terminal_id: "terminal".to_string(),
            endpoint: "https://apigw.example".to_string(),
            token_info: SmartLifeTokenInfo {
                t: 0,
                expire_time: 7200,
                uid: "uid".to_string(),
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
            },
        };
        auth.save(&path).expect("seed");

        let rotations = 8;
        let handles = (0..rotations)
            .map(|_| tokio::spawn(rotate(path.clone())))
            .collect::<Vec<_>>();
        for handle in handles {
            handle.await.expect("rotation task");
        }

        let final_auth = SmartLifeAuthCache::load(&path).expect("load");
        assert_eq!(
            final_auth.token_info.refresh_token,
            format!("refresh{}", "+".repeat(rotations)),
            "a concurrent refresh clobbered another rotation"
        );
    }

    #[test]
    fn auth_file_falls_back_to_the_shared_default() {
        let configured = PathBuf::from("/tmp/office/auth.json");
        assert_eq!(
            auth_file_or_default(Some(&configured)),
            PathBuf::from("/tmp/office/auth.json")
        );
        assert_eq!(auth_file_or_default(None), default_auth_file());
    }
}
