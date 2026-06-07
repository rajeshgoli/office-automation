use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::db::{self, DeviceRegistration};

const DEFAULT_DEVICE_CA_CERT: &str = "/Users/rajesh/.office-automate/device-ca/device-ca.pem";
const DEFAULT_DEVICE_CA_KEY: &str = "/Users/rajesh/.office-automate/device-ca/device-ca.key";

#[derive(Debug, Clone)]
struct PairingState {
    database_path: PathBuf,
    ca_cert_path: PathBuf,
    ca_key_path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct CompleteDeviceRequest {
    pub pairing_code: String,
    pub csr_pem: String,
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

pub fn revoke_device(database_path: &Path, device_id: &str) -> Result<bool> {
    db::revoke_device_registration(database_path, device_id)
}

pub async fn serve_pairing_listener(
    database_path: PathBuf,
    bind_addr: SocketAddr,
    ca_cert_path: Option<PathBuf>,
    ca_key_path: Option<PathBuf>,
) -> Result<()> {
    let state = PairingState {
        database_path,
        ca_cert_path: ca_cert_path.unwrap_or_else(default_device_ca_cert_path),
        ca_key_path: ca_key_path.unwrap_or_else(default_device_ca_key_path),
    };

    let app = Router::new()
        .route("/health", get(pairing_health))
        .route("/complete", post(complete_device_pairing))
        .with_state(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind pairing listener at {bind_addr}"))?;
    axum::serve(listener, app)
        .await
        .context("pairing listener exited with an error")
}

pub fn default_device_ca_cert_path() -> PathBuf {
    PathBuf::from(DEFAULT_DEVICE_CA_CERT)
}

pub fn default_device_ca_key_path() -> PathBuf {
    PathBuf::from(DEFAULT_DEVICE_CA_KEY)
}

async fn pairing_health() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}

async fn complete_device_pairing(
    State(state): State<PairingState>,
    Json(payload): Json<CompleteDeviceRequest>,
) -> Response {
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

    match db::complete_device_registration(
        &state.database_path,
        pairing_code,
        &payload.public_key_pem,
        &certificate_common_name,
    ) {
        Ok(Some(registration)) => Json(CompleteDeviceResponse {
            device_id: registration.device_id,
            device_name: registration.device_name,
            pairing_code: registration.pairing_code,
            common_name: registration.common_name,
            certificate_pem: certificate_pem.clone(),
            certificate_chain_pem: certificate_pem.clone(),
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
