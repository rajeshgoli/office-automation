use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    cloudflare::{self, DevicePolicyAction},
    config::CloudflareAccessConfig,
    db::{self, DeviceRegistration},
};

const DEFAULT_DEVICE_CA_CERT: &str = "certs/device-ca.pem";
const DEFAULT_DEVICE_CA_KEY: &str = "certs/device-ca.key";
const MAX_UNKNOWN_PAIRING_ATTEMPTS: u32 = 25;

#[derive(Debug, Clone)]
struct PairingState {
    database_path: PathBuf,
    ca_cert_path: PathBuf,
    ca_key_path: PathBuf,
    cloudflare_access: CloudflareAccessConfig,
    unknown_attempts: Arc<Mutex<u32>>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteDeviceRequest {
    #[serde(default)]
    pub pairing_code: String,
    #[serde(default)]
    pub csr_pem: String,
    #[serde(default)]
    pub public_key_pem: String,
    #[serde(default)]
    pub common_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompleteDeviceResponse {
    pub device_id: String,
    pub device_name: String,
    pub pairing_code: String,
    pub common_name: Option<String>,
    pub certificate_pem: String,
    pub certificate_chain_pem: String,
    pub expires_at: String,
}

pub fn register_device(
    database_path: &Path,
    device_name: &str,
    expires_in_minutes: i64,
) -> Result<DeviceRegistration> {
    if device_name.trim().is_empty() {
        bail!("device name must not be empty");
    }
    db::create_device_registration(database_path, device_name, expires_in_minutes)
}

pub fn list_devices(database_path: &Path) -> Result<Vec<DeviceRegistration>> {
    db::list_device_registrations(database_path)
}

pub async fn revoke_device(
    database_path: &Path,
    cloudflare_access: &CloudflareAccessConfig,
    device_id: &str,
) -> Result<bool> {
    if let Some(common_name) = db::list_device_registrations(database_path)?
        .into_iter()
        .find(|device| device.device_id == device_id)
        .and_then(|device| device.common_name)
    {
        cloudflare::sync_device_common_name(
            cloudflare_access,
            &common_name,
            DevicePolicyAction::Revoke,
        )
        .await?;
    }
    db::revoke_device_registration(database_path, device_id)
}

pub async fn serve_pairing_listener(
    database_path: PathBuf,
    bind_addr: SocketAddr,
    ca_cert_path: Option<PathBuf>,
    ca_key_path: Option<PathBuf>,
    cloudflare_access: CloudflareAccessConfig,
) -> Result<()> {
    let state = PairingState {
        database_path,
        ca_cert_path: ca_cert_path.unwrap_or_else(default_device_ca_cert_path),
        ca_key_path: ca_key_path.unwrap_or_else(default_device_ca_key_path),
        cloudflare_access,
        unknown_attempts: Arc::new(Mutex::new(0)),
    };

    let app = Router::new()
        .route("/health", get(pairing_health))
        .route("/complete", post(complete_device_pairing))
        .with_state(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind pairing listener at {bind_addr}"))?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("pairing listener exited with an error")
}

pub fn default_device_ca_cert_path() -> PathBuf {
    default_repo_local_path(DEFAULT_DEVICE_CA_CERT)
}

pub fn default_device_ca_key_path() -> PathBuf {
    default_repo_local_path(DEFAULT_DEVICE_CA_KEY)
}

fn default_repo_local_path(relative_path: &str) -> PathBuf {
    env::var_os("OFFICE_AUTOMATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(relative_path)
}

async fn pairing_health() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}

async fn complete_device_pairing(
    State(state): State<PairingState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<CompleteDeviceRequest>,
) -> Response {
    let remote_addr = remote_addr.to_string();
    let pairing_code = payload.pairing_code.trim();
    if pairing_code.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing pairing_code"})),
        )
            .into_response();
    }
    let pending_registration =
        match db::pending_device_registration_by_pairing_code(&state.database_path, pairing_code) {
            Ok(Some(registration)) => registration,
            Ok(None) => {
                let attempts = increment_unknown_attempts(&state);
                let _ = db::log_unknown_device_pairing_attempt(
                    &state.database_path,
                    pairing_code,
                    Some(&remote_addr),
                    if attempts > MAX_UNKNOWN_PAIRING_ATTEMPTS {
                        "too_many_unknown_attempts"
                    } else {
                        "unknown_expired_or_already_paired"
                    },
                );
                if attempts > MAX_UNKNOWN_PAIRING_ATTEMPTS {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(serde_json::json!({"error": "Too many invalid pairing attempts"})),
                    )
                        .into_response();
                }
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "Unknown, expired, or already paired code"})),
                )
                    .into_response();
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        };
    let certificate_common_name = pending_registration.device_id.clone();
    let csr_public_key_pem = match extract_public_key_from_csr_with_openssl(&payload.csr_pem) {
        Ok(public_key_pem) => public_key_pem,
        Err(error) => {
            return invalid_csr_response(&state, pairing_code, Some(&remote_addr), &error);
        }
    };
    let certificate_pem = match sign_csr_with_openssl(
        &state.ca_cert_path,
        &state.ca_key_path,
        &payload.csr_pem,
        &certificate_common_name,
    ) {
        Ok(pem) => pem,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let certificate_chain_pem =
        match build_certificate_chain_pem(&certificate_pem, &state.ca_cert_path) {
            Ok(chain) => chain,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        };
    if let Err(error) = cloudflare::sync_device_common_name(
        &state.cloudflare_access,
        &certificate_common_name,
        DevicePolicyAction::Allow,
    )
    .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response();
    }

    match db::complete_device_registration(
        &state.database_path,
        pairing_code,
        &csr_public_key_pem,
        &certificate_common_name,
        Some(&remote_addr),
    ) {
        Ok(Some(registration)) => Json(CompleteDeviceResponse {
            device_id: registration.device_id,
            device_name: registration.device_name,
            pairing_code: registration.pairing_code,
            common_name: registration.common_name,
            certificate_pem: certificate_pem.clone(),
            certificate_chain_pem,
            expires_at: registration.expires_at,
        })
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Unknown, expired, or already paired code"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn increment_unknown_attempts(state: &PairingState) -> u32 {
    let mut attempts = state
        .unknown_attempts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *attempts += 1;
    *attempts
}

fn record_pairing_failure(
    state: &PairingState,
    pairing_code: &str,
    remote_addr: Option<&str>,
    event: &str,
    error: &str,
) -> bool {
    db::record_device_pairing_failure(
        &state.database_path,
        pairing_code,
        event,
        remote_addr,
        Some(&serde_json::json!({"error": error})),
    )
    .map(|failure| failure.map(|failure| failure.exhausted).unwrap_or(false))
    .unwrap_or(false)
}

fn invalid_csr_response(
    state: &PairingState,
    pairing_code: &str,
    remote_addr: Option<&str>,
    error: &anyhow::Error,
) -> Response {
    let exhausted = record_pairing_failure(
        state,
        pairing_code,
        remote_addr,
        "pairing_csr_rejected",
        &error.to_string(),
    );
    (
        if exhausted {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::BAD_REQUEST
        },
        Json(serde_json::json!({"error": if exhausted {
            "Too many invalid pairing attempts"
        } else {
            "Invalid device CSR"
        }})),
    )
        .into_response()
}

fn extract_public_key_from_csr_with_openssl(csr_pem: &str) -> Result<String> {
    let temp_dir =
        tempfile::tempdir().context("failed to create temporary CSR validation directory")?;
    let csr_path = temp_dir.path().join("device.csr.pem");
    fs::write(&csr_path, csr_pem).context("failed to write CSR")?;

    verify_csr_self_signature_with_openssl(&csr_path)?;

    let output = Command::new("openssl")
        .arg("req")
        .arg("-in")
        .arg(&csr_path)
        .arg("-pubkey")
        .arg("-noout")
        .output()
        .context("failed to invoke openssl for CSR public key extraction")?;

    if !output.status.success() {
        bail!("openssl failed to read CSR public key");
    }

    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .context("openssl returned non-UTF-8 CSR public key")
}

fn verify_csr_self_signature_with_openssl(csr_path: &Path) -> Result<()> {
    let output = Command::new("openssl")
        .arg("req")
        .arg("-in")
        .arg(csr_path)
        .arg("-verify")
        .arg("-noout")
        .output()
        .context("failed to invoke openssl for CSR verification")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "openssl failed to verify CSR self-signature: {}",
        stderr.trim()
    )
}

fn sign_csr_with_openssl(
    ca_cert_path: &Path,
    ca_key_path: &Path,
    csr_pem: &str,
    common_name: &str,
) -> Result<String> {
    let temp_dir = tempfile::tempdir().context("failed to create temporary signing directory")?;
    let csr_path = temp_dir.path().join("device.csr.pem");
    let cert_path = temp_dir.path().join("device.cert.pem");
    let ext_path = temp_dir.path().join("client.ext");
    let serial_path = temp_dir.path().join("device.srl");

    fs::write(&csr_path, csr_pem).context("failed to write CSR")?;
    fs::write(
        &ext_path,
        r#"
[client_cert]
basicConstraints = critical,CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = clientAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
"#,
    )
    .context("failed to write client certificate extension file")?;

    let status = Command::new("openssl")
        .arg("x509")
        .arg("-req")
        .arg("-in")
        .arg(&csr_path)
        .arg("-CA")
        .arg(ca_cert_path)
        .arg("-CAkey")
        .arg(ca_key_path)
        .arg("-CAcreateserial")
        .arg("-CAserial")
        .arg(&serial_path)
        .arg("-out")
        .arg(&cert_path)
        .arg("-subj")
        .arg(format!("/CN={common_name}"))
        .arg("-days")
        .arg("3650")
        .arg("-sha256")
        .arg("-extfile")
        .arg(&ext_path)
        .arg("-extensions")
        .arg("client_cert")
        .status()
        .context("failed to invoke openssl for CSR signing")?;

    if !status.success() {
        bail!("openssl failed to sign CSR");
    }

    fs::read_to_string(&cert_path).context("failed to read signed certificate")
}

fn build_certificate_chain_pem(certificate_pem: &str, ca_cert_path: &Path) -> Result<String> {
    let ca_cert_pem =
        fs::read_to_string(ca_cert_path).context("failed to read device CA certificate")?;
    Ok(format!(
        "{}\n{}",
        certificate_pem.trim(),
        ca_cert_pem.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_device_request_defaults_omitted_proof_fields() {
        let payload: CompleteDeviceRequest =
            serde_json::from_str(r#"{"pairing_code":"ABC123"}"#).expect("deserialize payload");

        assert_eq!(payload.pairing_code, "ABC123");
        assert_eq!(payload.csr_pem, "");
        assert_eq!(payload.public_key_pem, "");
    }

    #[test]
    fn build_certificate_chain_appends_ca_certificate() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let ca_path = temp_dir.path().join("device-ca.pem");
        fs::write(
            &ca_path,
            "-----BEGIN CERTIFICATE-----\nca\n-----END CERTIFICATE-----\n",
        )
        .expect("write ca");

        let chain = build_certificate_chain_pem(
            "-----BEGIN CERTIFICATE-----\nleaf\n-----END CERTIFICATE-----\n\n",
            &ca_path,
        )
        .expect("chain");

        assert_eq!(
            chain,
            "-----BEGIN CERTIFICATE-----\nleaf\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nca\n-----END CERTIFICATE-----"
        );
    }
}
