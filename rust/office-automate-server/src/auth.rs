use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use base64::{Engine, engine::general_purpose};
use chrono::{TimeDelta, Utc};
use ipnet::IpNet;
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
    jwk::JwkSet,
};
use rand::{RngCore, rngs::OsRng};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{GoogleOAuthConfig, OrchestratorConfig};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";
const GOOGLE_TOKENINFO_URL: &str = "https://oauth2.googleapis.com/tokeninfo";
const OAUTH_SCOPE: &str = "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
const BASIC_WS_COOKIE: &str = "office_basic_ws";
const BASIC_WS_COOKIE_MAX_AGE_SECONDS: i64 = 600;
pub const OAUTH_SESSION_COOKIE: &str = "office_auth";
pub const OAUTH_CSRF_COOKIE: &str = "office_csrf";
pub const OAUTH_CSRF_HEADER: &str = "x-csrf-token";

#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    oauth: Option<OAuthRuntime>,
    basic: Option<BasicCredentials>,
    admin_emails: HashSet<String>,
    pending_oauth: RwLock<HashMap<String, PendingOAuthState>>,
    device_flows: RwLock<HashMap<String, DeviceFlowState>>,
    invalidated_tokens: RwLock<HashSet<String>>,
    basic_ws_tokens: RwLock<HashMap<String, chrono::DateTime<Utc>>>,
    client: Client,
}

#[derive(Clone)]
struct OAuthRuntime {
    client_id: String,
    client_secret: String,
    allowed_emails: HashSet<String>,
    token_expiry_days: i64,
    device_flow_enabled: bool,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    trusted_networks: Vec<IpNet>,
}

#[derive(Debug, Clone)]
struct BasicCredentials {
    username: String,
    password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOAuthState {
    pub code_verifier: String,
    pub redirect_uri: String,
    pub platform: Option<String>,
    pub return_to: Option<String>,
}

#[derive(Debug, Clone)]
struct DeviceFlowState {
    expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpAuthMode {
    Open,
    OAuth,
    Basic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketAuth {
    TrustedNetwork,
    UpgradeBearer,
    UpgradeBasic,
    FirstMessage,
    Reject,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub email: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthCredentialSource {
    Bearer,
    SessionCookie,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOAuthRequest {
    pub user: AuthenticatedUser,
    pub source: OAuthCredentialSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    email: String,
    exp: usize,
    iat: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudflareAccessClaims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
    pub iss: String,
    #[serde(default)]
    pub common_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GoogleTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GoogleTokenInfo {
    email: Option<String>,
    aud: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceFlowStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<i64>,
}

impl AuthManager {
    pub fn new(config: &OrchestratorConfig) -> Result<Self> {
        let oauth = config
            .google_oauth
            .as_ref()
            .map(OAuthRuntime::from_config)
            .transpose()?;

        let basic = match (&config.auth_username, &config.auth_password) {
            (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
                Some(BasicCredentials {
                    username: username.clone(),
                    password: password.clone(),
                })
            }
            _ => None,
        };

        Ok(Self {
            inner: Arc::new(AuthInner {
                oauth,
                basic,
                admin_emails: config
                    .admin_emails
                    .iter()
                    .map(|email| email.to_ascii_lowercase())
                    .collect(),
                pending_oauth: RwLock::new(HashMap::new()),
                device_flows: RwLock::new(HashMap::new()),
                invalidated_tokens: RwLock::new(HashSet::new()),
                basic_ws_tokens: RwLock::new(HashMap::new()),
                client: Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .context("failed to build OAuth HTTP client")?,
            }),
        })
    }

    pub fn mode(&self) -> HttpAuthMode {
        if self.inner.oauth.is_some() {
            HttpAuthMode::OAuth
        } else if self.inner.basic.is_some() {
            HttpAuthMode::Basic
        } else {
            HttpAuthMode::Open
        }
    }

    pub fn oauth_enabled(&self) -> bool {
        self.inner.oauth.is_some()
    }

    pub fn basic_enabled(&self) -> bool {
        self.inner.basic.is_some()
    }

    pub fn is_admin_user(&self, email: &str) -> bool {
        self.inner
            .admin_emails
            .contains(&email.to_ascii_lowercase())
    }

    pub fn is_trusted_request(&self, headers: &HeaderMap) -> bool {
        let Some(oauth) = &self.inner.oauth else {
            return false;
        };

        let Some(client_ip) = forwarded_client_ip(headers) else {
            return false;
        };

        let Ok(address) = client_ip.parse::<IpAddr>() else {
            return false;
        };

        oauth
            .trusted_networks
            .iter()
            .any(|network| network.contains(&address))
    }

    pub fn verify_bearer_header(&self, headers: &HeaderMap) -> Option<AuthenticatedUser> {
        let token = bearer_token(headers)?;
        self.verify_jwt(token)
            .map(|email| AuthenticatedUser { email })
    }

    pub fn verify_oauth_request(&self, headers: &HeaderMap) -> Option<VerifiedOAuthRequest> {
        if let Some(user) = self.verify_bearer_header(headers) {
            return Some(VerifiedOAuthRequest {
                user,
                source: OAuthCredentialSource::Bearer,
            });
        }

        let token = oauth_session_cookie(headers)?;
        self.verify_jwt(token).map(|email| VerifiedOAuthRequest {
            user: AuthenticatedUser { email },
            source: OAuthCredentialSource::SessionCookie,
        })
    }

    pub fn bearer_or_session_token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        bearer_token(headers).or_else(|| oauth_session_cookie(headers))
    }

    pub fn verify_oauth_csrf(&self, headers: &HeaderMap) -> bool {
        let Some(header_token) = headers.get(OAUTH_CSRF_HEADER).and_then(header_to_str) else {
            return false;
        };
        let Some(cookie_token) = cookie_value(headers, OAUTH_CSRF_COOKIE) else {
            return false;
        };
        !header_token.is_empty() && header_token == cookie_token
    }

    pub async fn verify_cloudflare_access_assertion(
        &self,
        token: &str,
    ) -> Option<CloudflareAccessClaims> {
        let header = decode_header(token).ok()?;
        let kid = header.kid.as_deref()?;
        let untrusted_claims = Self::decode_cloudflare_access_assertion_unverified(token)?;
        let issuer = cloudflare_access_issuer_url(&untrusted_claims.iss)?;
        let jwks_url = issuer.join("/cdn-cgi/access/certs").ok()?;
        let jwks = self
            .inner
            .client
            .get(jwks_url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<JwkSet>()
            .await
            .ok()?;
        let key = DecodingKey::from_jwk(jwks.find(kid)?).ok()?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_aud = false;
        validation.set_issuer(&[issuer.as_str().trim_end_matches('/')]);
        decode::<CloudflareAccessClaims>(token, &key, &validation)
            .ok()
            .map(|decoded| decoded.claims)
    }

    fn decode_cloudflare_access_assertion_unverified(
        token: &str,
    ) -> Option<CloudflareAccessClaims> {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.insecure_disable_signature_validation();
        validation.validate_aud = false;
        validation.validate_exp = false;
        decode::<CloudflareAccessClaims>(token, &DecodingKey::from_secret(&[]), &validation)
            .ok()
            .map(|decoded| decoded.claims)
    }

    pub fn verify_basic_header(&self, headers: &HeaderMap) -> bool {
        self.verify_basic_header_user(headers).is_some()
    }

    pub fn verify_basic_header_user(&self, headers: &HeaderMap) -> Option<AuthenticatedUser> {
        let Some(credentials) = &self.inner.basic else {
            return None;
        };
        let Some(header_value) = headers.get(header::AUTHORIZATION).and_then(header_to_str) else {
            return None;
        };
        let Some(encoded) = header_value.strip_prefix("Basic ") else {
            return None;
        };
        let Ok(decoded) = general_purpose::STANDARD.decode(encoded) else {
            return None;
        };
        let Ok(decoded) = String::from_utf8(decoded) else {
            return None;
        };
        let Some((username, password)) = decoded.split_once(':') else {
            return None;
        };

        if username == credentials.username && password == credentials.password {
            Some(AuthenticatedUser {
                email: credentials.username.clone(),
            })
        } else {
            None
        }
    }

    pub fn issue_basic_websocket_cookie(&self) -> Option<String> {
        self.inner.basic.as_ref()?;
        let token = random_url_token(32);
        let expires_at = Utc::now() + TimeDelta::seconds(BASIC_WS_COOKIE_MAX_AGE_SECONDS);
        let mut tokens = self
            .inner
            .basic_ws_tokens
            .write()
            .expect("basic websocket token lock");
        prune_expired_tokens(&mut tokens);
        tokens.insert(token.clone(), expires_at);
        Some(format!(
            "{BASIC_WS_COOKIE}={token}; Max-Age={BASIC_WS_COOKIE_MAX_AGE_SECONDS}; Path=/ws; HttpOnly; SameSite=Lax"
        ))
    }

    pub fn issue_oauth_session_cookies(&self, token: &str, secure: bool) -> Option<Vec<String>> {
        let oauth = self.inner.oauth.as_ref()?;
        let max_age = oauth.token_expiry_days.max(1) * 24 * 60 * 60;
        let csrf_token = random_url_token(32);
        let secure_attribute = secure.then_some("; Secure").unwrap_or("");
        Some(vec![
            format!(
                "{OAUTH_SESSION_COOKIE}={token}; Max-Age={max_age}; Path=/; HttpOnly{secure_attribute}; SameSite=Lax"
            ),
            format!(
                "{OAUTH_CSRF_COOKIE}={csrf_token}; Max-Age={max_age}; Path=/{secure_attribute}; SameSite=Lax"
            ),
        ])
    }

    pub fn clear_oauth_session_cookies(secure: bool) -> [String; 2] {
        let secure_attribute = secure.then_some("; Secure").unwrap_or("");
        [
            format!("office_auth=; Max-Age=0; Path=/; HttpOnly{secure_attribute}; SameSite=Lax"),
            format!("office_csrf=; Max-Age=0; Path=/{secure_attribute}; SameSite=Lax"),
        ]
    }

    pub fn verify_basic_websocket_auth(&self, headers: &HeaderMap) -> bool {
        self.verify_basic_header(headers) || self.verify_basic_websocket_cookie(headers)
    }

    fn verify_basic_websocket_cookie(&self, headers: &HeaderMap) -> bool {
        if self.inner.basic.is_none() {
            return false;
        }
        let Some(token) = cookie_value(headers, BASIC_WS_COOKIE) else {
            return false;
        };
        let now = Utc::now();
        let mut tokens = self
            .inner
            .basic_ws_tokens
            .write()
            .expect("basic websocket token lock");
        prune_expired_tokens_at(&mut tokens, now);
        tokens
            .get(token)
            .is_some_and(|expires_at| *expires_at > now)
    }

    pub fn verify_jwt(&self, token: &str) -> Option<String> {
        if self
            .inner
            .invalidated_tokens
            .read()
            .expect("invalidated token lock")
            .contains(token)
        {
            return None;
        }

        let oauth = self.inner.oauth.as_ref()?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_aud = false;
        let claims = decode::<Claims>(token, &oauth.decoding_key, &validation)
            .ok()?
            .claims;
        let email = claims.email.to_ascii_lowercase();
        oauth.allowed_emails.contains(&email).then_some(email)
    }

    pub fn generate_jwt(&self, email: &str) -> Result<String> {
        let oauth = self
            .inner
            .oauth
            .as_ref()
            .ok_or_else(|| anyhow!("OAuth not configured"))?;
        let now = Utc::now();
        let expiry = now + TimeDelta::days(oauth.token_expiry_days);
        let claims = Claims {
            email: email.to_ascii_lowercase(),
            iat: now.timestamp() as usize,
            exp: expiry.timestamp() as usize,
        };
        encode(&Header::new(Algorithm::HS256), &claims, &oauth.encoding_key)
            .context("failed to generate JWT")
    }

    pub fn invalidate_token(&self, token: &str) {
        self.inner
            .invalidated_tokens
            .write()
            .expect("invalidated token lock")
            .insert(token.to_string());
    }

    pub fn websocket_auth(&self, headers: &HeaderMap) -> WebSocketAuth {
        if self.oauth_enabled() {
            if self.is_trusted_request(headers) {
                return WebSocketAuth::TrustedNetwork;
            }
            if self.verify_oauth_request(headers).is_some() {
                return WebSocketAuth::UpgradeBearer;
            }
            WebSocketAuth::FirstMessage
        } else if self.basic_enabled() {
            if self.verify_basic_websocket_auth(headers) {
                WebSocketAuth::UpgradeBasic
            } else {
                WebSocketAuth::Reject
            }
        } else {
            WebSocketAuth::Open
        }
    }

    pub fn begin_login(
        &self,
        host: &str,
        forwarded_proto: Option<&str>,
        platform: Option<String>,
        return_to: Option<String>,
    ) -> Result<Value> {
        let oauth = self
            .inner
            .oauth
            .as_ref()
            .ok_or_else(|| anyhow!("OAuth not configured"))?;
        let redirect_uri = format!(
            "{}://{host}/auth/callback",
            resolve_redirect_scheme(host, forwarded_proto)
        );
        let (code_verifier, code_challenge) = generate_pkce_pair();
        let state = random_url_token(32);

        self.inner
            .pending_oauth
            .write()
            .expect("pending OAuth lock")
            .insert(
                state.clone(),
                PendingOAuthState {
                    code_verifier,
                    redirect_uri: redirect_uri.clone(),
                    platform,
                    return_to,
                },
            );

        let authorization_url = format!(
            "{GOOGLE_AUTH_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}&access_type=offline&include_granted_scopes=true&state={}&code_challenge={}&code_challenge_method=S256&prompt=consent",
            urlencoding::encode(&oauth.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(OAUTH_SCOPE),
            urlencoding::encode(&state),
            urlencoding::encode(&code_challenge),
        );

        Ok(json!({
            "authorization_url": authorization_url,
            "state": state,
        }))
    }

    pub fn pending_state(&self, state: &str) -> Option<PendingOAuthState> {
        self.inner
            .pending_oauth
            .read()
            .expect("pending OAuth lock")
            .get(state)
            .cloned()
    }

    pub async fn finish_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<Option<FinishedOAuthLogin>> {
        let oauth = self
            .inner
            .oauth
            .as_ref()
            .ok_or_else(|| anyhow!("OAuth not configured"))?;
        let pending = self
            .inner
            .pending_oauth
            .write()
            .expect("pending OAuth lock")
            .remove(state)
            .ok_or_else(|| anyhow!("Invalid state"))?;

        let token_response: GoogleTokenResponse = self
            .inner
            .client
            .post(GOOGLE_TOKEN_URL)
            .form(&[
                ("client_id", oauth.client_id.as_str()),
                ("client_secret", oauth.client_secret.as_str()),
                ("code", code),
                ("code_verifier", pending.code_verifier.as_str()),
                ("redirect_uri", pending.redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .context("OAuth token exchange failed")?
            .error_for_status()
            .context("OAuth token exchange returned an error")?
            .json()
            .await
            .context("OAuth token response was not JSON")?;

        let Some(id_token) = token_response.id_token.as_deref() else {
            return Ok(None);
        };
        let Some(email) = self
            .verify_google_id_token(oauth, id_token, &oauth.client_id)
            .await?
        else {
            return Ok(None);
        };
        let jwt = self.generate_jwt(&email)?;

        Ok(Some(FinishedOAuthLogin {
            email,
            jwt,
            platform: pending.platform,
            return_to: pending.return_to,
            secure_cookie: pending.redirect_uri.starts_with("https://"),
            refresh_token: token_response.refresh_token,
            access_token: token_response.access_token,
        }))
    }

    pub async fn start_device_flow(&self) -> Result<DeviceFlowStartResponse> {
        let oauth = self
            .inner
            .oauth
            .as_ref()
            .ok_or_else(|| anyhow!("OAuth not configured"))?;
        if !oauth.device_flow_enabled {
            return Err(anyhow!("Device flow not enabled"));
        }

        let response: DeviceFlowStartResponse = self
            .inner
            .client
            .post(GOOGLE_DEVICE_CODE_URL)
            .form(&[
                ("client_id", oauth.client_id.as_str()),
                ("scope", "openid email profile"),
            ])
            .send()
            .await
            .context("device flow initiation failed")?
            .error_for_status()
            .context("device flow initiation returned an error")?
            .json()
            .await
            .context("device flow response was not JSON")?;

        self.inner
            .device_flows
            .write()
            .expect("device flow lock")
            .insert(
                response.device_code.clone(),
                DeviceFlowState {
                    expires_at: Utc::now() + TimeDelta::seconds(response.expires_in),
                },
            );

        Ok(response)
    }

    pub async fn poll_device_flow(&self, device_code: &str) -> Result<Value> {
        let oauth = self
            .inner
            .oauth
            .as_ref()
            .ok_or_else(|| anyhow!("OAuth not configured"))?;
        if !oauth.device_flow_enabled {
            return Ok(json!({"status": "error", "message": "Device flow not enabled"}));
        }

        {
            let mut device_flows = self.inner.device_flows.write().expect("device flow lock");
            let Some(state) = device_flows.get(device_code) else {
                return Ok(json!({"status": "invalid", "message": "Unknown device code"}));
            };
            if Utc::now() >= state.expires_at {
                device_flows.remove(device_code);
                return Ok(json!({"status": "expired", "message": "Device code expired"}));
            }
        }

        let response = self
            .inner
            .client
            .post(GOOGLE_TOKEN_URL)
            .form(&[
                ("client_id", oauth.client_id.as_str()),
                ("client_secret", oauth.client_secret.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .context("device flow polling failed")?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_else(|_| json!({}));

        if status == StatusCode::OK {
            let Some(id_token) = body.get("id_token").and_then(Value::as_str) else {
                return Ok(json!({"status": "error", "message": "Missing id_token"}));
            };
            let Some(email) = self
                .verify_google_id_token(oauth, id_token, &oauth.client_id)
                .await?
            else {
                return Ok(json!({"status": "forbidden", "message": "Email not allowed"}));
            };
            let jwt = self.generate_jwt(&email)?;
            self.inner
                .device_flows
                .write()
                .expect("device flow lock")
                .remove(device_code);
            return Ok(json!({
                "status": "success",
                "email": email,
                "access_token": jwt,
                "refresh_token": body.get("refresh_token").cloned().unwrap_or(Value::Null),
                "expires_in": oauth.token_expiry_days * 24 * 60 * 60,
            }));
        }

        if status == StatusCode::PRECONDITION_REQUIRED {
            return Ok(json!({"status": "pending", "message": "User has not authorized yet"}));
        }

        if status == StatusCode::BAD_REQUEST || status == StatusCode::FORBIDDEN {
            let error = body.get("error").and_then(Value::as_str).unwrap_or("error");
            return Ok(match error {
                "authorization_pending" => {
                    json!({"status": "pending", "message": "Waiting for user authorization"})
                }
                "slow_down" => json!({"status": "slow_down", "message": "Polling too fast"}),
                "access_denied" => json!({"status": "forbidden", "message": "Access denied"}),
                _ => json!({"status": "error", "message": error}),
            });
        }

        Ok(json!({"status": "error", "message": format!("HTTP {}", status.as_u16())}))
    }

    async fn verify_google_id_token(
        &self,
        oauth: &OAuthRuntime,
        id_token: &str,
        expected_audience: &str,
    ) -> Result<Option<String>> {
        let info: GoogleTokenInfo = self
            .inner
            .client
            .get(GOOGLE_TOKENINFO_URL)
            .query(&[("id_token", id_token)])
            .send()
            .await
            .context("Google id_token verification failed")?
            .error_for_status()
            .context("Google rejected id_token")?
            .json()
            .await
            .context("Google tokeninfo response was not JSON")?;

        if info.aud.as_deref() != Some(expected_audience) {
            return Ok(None);
        }

        let Some(email) = info.email.map(|email| email.to_ascii_lowercase()) else {
            return Ok(None);
        };

        Ok(oauth.allowed_emails.contains(&email).then_some(email))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedOAuthLogin {
    pub email: String,
    pub jwt: String,
    pub platform: Option<String>,
    pub return_to: Option<String>,
    pub secure_cookie: bool,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
}

impl OAuthRuntime {
    fn from_config(config: &GoogleOAuthConfig) -> Result<Self> {
        let jwt_secret = config
            .jwt_secret
            .clone()
            .unwrap_or_else(|| random_url_token(32));
        let trusted_networks = config
            .trusted_networks
            .iter()
            .map(|network| {
                network
                    .parse()
                    .with_context(|| format!("invalid trusted network {network:?}"))
            })
            .collect::<Result<Vec<IpNet>>>()?;

        Ok(Self {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            allowed_emails: config
                .allowed_emails
                .iter()
                .map(|email| email.to_ascii_lowercase())
                .collect(),
            token_expiry_days: config.token_expiry_days,
            device_flow_enabled: config.device_flow_enabled,
            encoding_key: EncodingKey::from_secret(jwt_secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(jwt_secret.as_bytes()),
            trusted_networks,
        })
    }
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(header_to_str)?
        .strip_prefix("Bearer ")
}

fn oauth_session_cookie(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, OAUTH_SESSION_COOKIE)
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-forwarded-for")
        .and_then(header_to_str)
        .and_then(|value| value.split(',').next().map(str::trim))
        .filter(|value| !value.is_empty())
}

fn header_to_str(value: &HeaderValue) -> Option<&str> {
    value.to_str().ok()
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)
        .and_then(header_to_str)?
        .split(';')
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(cookie_name, value)| (cookie_name == name).then_some(value))
}

fn prune_expired_tokens(tokens: &mut HashMap<String, chrono::DateTime<Utc>>) {
    prune_expired_tokens_at(tokens, Utc::now());
}

fn prune_expired_tokens_at(
    tokens: &mut HashMap<String, chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) {
    tokens.retain(|_, expires_at| *expires_at > now);
}

fn cloudflare_access_issuer_url(issuer: &str) -> Option<Url> {
    let url = Url::parse(issuer).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host == "cloudflareaccess.com" || host.ends_with(".cloudflareaccess.com") {
        Some(url)
    } else {
        None
    }
}

fn generate_pkce_pair() -> (String, String) {
    let code_verifier = random_url_token(32);
    let challenge = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = general_purpose::URL_SAFE_NO_PAD.encode(challenge);
    (code_verifier, code_challenge)
}

fn random_url_token(byte_len: usize) -> String {
    let mut bytes = vec![0_u8; byte_len];
    OsRng.fill_bytes(&mut bytes);
    general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn resolve_redirect_scheme(host: &str, forwarded_proto: Option<&str>) -> &'static str {
    if let Some(proto) = forwarded_proto.and_then(|value| value.split(',').next()) {
        let proto = proto.trim();
        if proto.eq_ignore_ascii_case("http") {
            return "http";
        }
        if proto.eq_ignore_ascii_case("https") {
            return "https";
        }
    }

    let host_without_port = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    if host_without_port == "localhost"
        || host_without_port == "127.0.0.1"
        || host_without_port.ends_with(".local")
    {
        "http"
    } else {
        "https"
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn oauth_config() -> OrchestratorConfig {
        OrchestratorConfig {
            google_oauth: Some(GoogleOAuthConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                allowed_emails: vec!["Engineer@RajesHGo.li".to_string()],
                jwt_secret: Some("test-secret".to_string()),
                trusted_networks: vec!["192.168.0.0/16".to_string()],
                ..GoogleOAuthConfig::default()
            }),
            ..OrchestratorConfig::default()
        }
    }

    #[test]
    fn jwt_generation_and_validation_honors_allowlist() {
        let manager = AuthManager::new(&oauth_config()).expect("auth");
        let token = manager.generate_jwt("engineer@rajeshgo.li").expect("token");

        assert_eq!(
            manager.verify_jwt(&token),
            Some("engineer@rajeshgo.li".to_string())
        );

        manager.invalidate_token(&token);
        assert_eq!(manager.verify_jwt(&token), None);
    }

    #[test]
    fn trusted_network_uses_forwarded_for() {
        let manager = AuthManager::new(&oauth_config()).expect("auth");
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("192.168.1.20"));

        assert!(manager.is_trusted_request(&headers));
    }

    #[test]
    fn login_uses_forwarded_proto_and_host() {
        let manager = AuthManager::new(&oauth_config()).expect("auth");
        let payload = manager
            .begin_login(
                "office.example.com",
                Some("https"),
                Some("android".to_string()),
                Some("/apps/office-climate/latest.apk".to_string()),
            )
            .expect("login");
        let url = payload["authorization_url"].as_str().expect("url");
        let state = payload["state"].as_str().expect("state");

        assert!(url.contains("redirect_uri=https%3A%2F%2Foffice.example.com%2Fauth%2Fcallback"));
        assert_eq!(
            manager.pending_state(state).expect("stored").platform,
            Some("android".to_string())
        );
        assert_eq!(
            manager.pending_state(state).expect("stored").return_to,
            Some("/apps/office-climate/latest.apk".to_string())
        );
    }

    #[test]
    fn basic_auth_validates_credentials() {
        let manager = AuthManager::new(&OrchestratorConfig {
            auth_username: Some("admin".to_string()),
            auth_password: Some("secret".to_string()),
            ..OrchestratorConfig::default()
        })
        .expect("auth");

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic YWRtaW46c2VjcmV0"),
        );

        assert!(manager.verify_basic_header(&headers));
    }

    #[test]
    fn cloudflare_access_issuer_guard_rejects_non_cloudflare_hosts() {
        assert!(cloudflare_access_issuer_url("https://team.cloudflareaccess.com").is_some());
        assert!(cloudflare_access_issuer_url("https://rajeshgoli.cloudflareaccess.com").is_some());
        assert!(cloudflare_access_issuer_url("http://rajeshgoli.cloudflareaccess.com").is_none());
        assert!(cloudflare_access_issuer_url("https://cloudflareaccess.com.evil.test").is_none());
        assert!(cloudflare_access_issuer_url("https://example.test").is_none());
    }

    #[test]
    fn basic_websocket_cookie_is_issued_and_verified() {
        let manager = AuthManager::new(&OrchestratorConfig {
            auth_username: Some("admin".to_string()),
            auth_password: Some("secret".to_string()),
            ..OrchestratorConfig::default()
        })
        .expect("auth");

        let cookie = manager
            .issue_basic_websocket_cookie()
            .expect("basic websocket cookie");
        let cookie_pair = cookie.split(';').next().expect("cookie pair");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(cookie_pair).expect("cookie"),
        );

        assert!(manager.verify_basic_websocket_auth(&headers));
    }
}
