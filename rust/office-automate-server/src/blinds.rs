use std::{env, fs, path::PathBuf, str::FromStr, time::Duration};

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
use uuid::Uuid;

use crate::config::BlindsConfig;

const SMART_LIFE_CLIENT_ID: &str = "HA_3y9q4ak7g4ephrvke";
const DEFAULT_SMART_LIFE_AUTH_FILE: &str = ".office-automate/tuya-sharing-auth.json";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlindsCommand {
    Open,
    Close,
}

impl BlindsCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            _ => None,
        }
    }

    fn tuya_value<'a>(self, config: &'a BlindsConfig) -> &'a str {
        match self {
            Self::Open => &config.open_value,
            Self::Close => &config.close_value,
        }
    }

    fn scene_id<'a>(self, config: &'a BlindsConfig) -> Option<&'a str> {
        match self {
            Self::Open => config.open_scene_id.as_deref(),
            Self::Close => config.close_scene_id.as_deref(),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }
}

pub async fn set_blinds(config: &BlindsConfig, command: BlindsCommand) -> Result<()> {
    if !config.active_control_enabled {
        bail!("Blinds active control is disabled");
    }
    if !config.is_configured() {
        bail!("Blinds config is incomplete");
    }

    if config.smart_life_scene_configured() {
        trigger_smart_life_scene(config, command).await
    } else {
        set_local_tuya_blinds(config, command).await
    }
}

async fn set_local_tuya_blinds(config: &BlindsConfig, command: BlindsCommand) -> Result<()> {
    if !config.local_tuya_configured() {
        bail!("Blinds local Tuya config is incomplete");
    }

    let device = build_rustuya_device(config)?;
    let result = device
        .set_value(&config.control_dp, command.tuya_value(config))
        .await
        .with_context(|| format!("failed to send blinds {} command", command.as_str()));
    device.close().await;

    let payload = result?;
    ensure_tuya_command_ok("Local blinds command failed", payload.as_deref())
}

async fn trigger_smart_life_scene(config: &BlindsConfig, command: BlindsCommand) -> Result<()> {
    let home_id = config
        .smart_life_home_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Blinds Smart Life home id is missing"))?;
    let scene_id = command
        .scene_id(config)
        .ok_or_else(|| anyhow!("Blinds Smart Life {} scene id is missing", command.as_str()))?;

    let auth_file = smart_life_auth_file(config);
    let mut auth_cache = SmartLifeAuthCache::load(&auth_file)?;
    let client = SmartLifeClient::new();
    client.refresh_auth_if_needed(&mut auth_cache).await?;
    auth_cache.save(&auth_file)?;
    client
        .post(
            &mut auth_cache,
            "/v1.0/m/scene/ha/trigger",
            None,
            Some(json!({"homeId": home_id, "sceneId": scene_id})),
        )
        .await
        .with_context(|| {
            format!(
                "failed to trigger Smart Life {} blinds scene",
                command.as_str()
            )
        })?;
    auth_cache.save(&auth_file)?;
    Ok(())
}

fn smart_life_auth_file(config: &BlindsConfig) -> PathBuf {
    config.smart_life_auth_file.clone().unwrap_or_else(|| {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(DEFAULT_SMART_LIFE_AUTH_FILE)
    })
}

fn build_rustuya_device(config: &BlindsConfig) -> Result<rustuya::Device> {
    let version = rustuya::Version::from_str(&config.version)
        .map_err(|error| anyhow!("invalid blinds Tuya protocol version: {error}"))?;
    Ok(rustuya::Device::builder(
        config.device_id.clone(),
        config.local_key.as_bytes().to_vec(),
    )
    .address(config.ip.clone())
    .version(version)
    .port(config.port)
    .persist(false)
    .timeout(Duration::from_secs(config.status_timeout_seconds.max(1)))
    .build())
}

fn ensure_tuya_command_ok(context: &str, payload: Option<&str>) -> Result<()> {
    match payload {
        Some(raw) if raw.contains("\"Err\"") || raw.contains("\"Error\"") => {
            bail!("{context}: {raw}")
        }
        _ => Ok(()),
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
    fn load(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read Smart Life auth cache {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed to parse Smart Life auth cache {}", path.display()))
    }

    fn save(&self, path: &PathBuf) -> Result<()> {
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

struct SmartLifeClient {
    http: Client,
}

impl SmartLifeClient {
    fn new() -> Self {
        Self {
            http: Client::new(),
        }
    }

    async fn refresh_auth_if_needed(&self, auth: &mut SmartLifeAuthCache) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expires_at = auth.token_info.t + auth.token_info.expire_time * 1_000;
        if expires_at - 60_000 > now_ms {
            return Ok(());
        }

        let path = format!("/v1.0/m/token/{}", auth.token_info.refresh_token);
        let result = self
            .get(auth, &path, None)
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

    async fn get(
        &self,
        auth: &mut SmartLifeAuthCache,
        path: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        self.request(auth, "GET", path, params, None).await
    }

    async fn post(
        &self,
        auth: &mut SmartLifeAuthCache,
        path: &str,
        params: Option<Value>,
        body: Option<Value>,
    ) -> Result<Value> {
        self.request(auth, "POST", path, params, body).await
    }

    async fn request(
        &self,
        auth: &mut SmartLifeAuthCache,
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
        .header("X-appKey", SMART_LIFE_CLIENT_ID)
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
    hash_key: &str,
    query_encdata: &str,
    body_encdata: &str,
    access_token: &str,
    request_id: &str,
    timestamp_ms: &str,
) -> Result<String> {
    let mut sign = format!(
        "X-appKey={SMART_LIFE_CLIENT_ID}||X-requestId={request_id}||X-time={timestamp_ms}||X-token={access_token}"
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
    fn parses_supported_commands() {
        assert_eq!(BlindsCommand::parse("open"), Some(BlindsCommand::Open));
        assert_eq!(BlindsCommand::parse("close"), Some(BlindsCommand::Close));
        assert_eq!(BlindsCommand::parse("stop"), None);
    }

    #[test]
    fn config_accepts_scene_or_local_tuya_identity() {
        let mut config = BlindsConfig::default();
        assert!(!config.is_configured());

        config.smart_life_home_id = Some("home-id".to_string());
        config.open_scene_id = Some("open-scene".to_string());
        config.close_scene_id = Some("close-scene".to_string());
        assert!(config.is_configured());
        assert!(config.smart_life_scene_configured());
        assert!(!config.local_tuya_configured());

        let mut config = BlindsConfig::default();
        config.ip = "192.168.1.55".to_string();
        config.device_id = "device-id".to_string();
        config.local_key = "local-key".to_string();
        assert!(config.is_configured());
        assert!(!config.smart_life_scene_configured());
        assert!(config.local_tuya_configured());
    }

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
}
