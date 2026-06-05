use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use axum::{
    Json,
    body::Body,
    extract::Multipart,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Local;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};

pub const ARTIFACT_MAX_SIZE_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    inner: Arc<ArtifactStoreInner>,
}

#[derive(Debug)]
struct ArtifactStoreInner {
    artifacts_root: PathBuf,
    legacy_apk_path: PathBuf,
    max_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub artifact_hash: String,
    pub uploaded_at: String,
    pub size_bytes: u64,
    pub uploaded_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
}

impl ArtifactStore {
    pub fn new(artifacts_root: PathBuf, legacy_apk_path: PathBuf) -> Self {
        Self::with_max_size(artifacts_root, legacy_apk_path, ARTIFACT_MAX_SIZE_BYTES)
    }

    pub fn with_max_size(
        artifacts_root: PathBuf,
        legacy_apk_path: PathBuf,
        max_size_bytes: u64,
    ) -> Self {
        Self {
            inner: Arc::new(ArtifactStoreInner {
                artifacts_root,
                legacy_apk_path,
                max_size_bytes,
            }),
        }
    }

    fn app_dir(&self, app: &str) -> PathBuf {
        self.inner.artifacts_root.join(app)
    }

    fn latest_path(&self, app: &str) -> PathBuf {
        self.app_dir(app).join("latest.apk")
    }

    fn hashed_path(&self, app: &str, artifact_hash: &str) -> PathBuf {
        self.app_dir(app).join(format!("{artifact_hash}.apk"))
    }

    fn meta_path(&self, app: &str) -> PathBuf {
        self.app_dir(app).join("meta.json")
    }

    pub async fn read_metadata(&self, app: &str) -> Result<Option<ArtifactMetadata>> {
        if !is_valid_app_name(app) {
            return Ok(None);
        }

        let meta_path = self.meta_path(app);
        if !meta_path.exists() {
            return Ok(None);
        }

        let contents = fs::read_to_string(&meta_path)
            .await
            .with_context(|| format!("failed to read {}", meta_path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", meta_path.display()))
            .map(Some)
    }

    async fn write_metadata(&self, app: &str, metadata: &ArtifactMetadata) -> Result<()> {
        let meta_path = self.meta_path(app);
        let temp_path = unique_temp_path(&self.app_dir(app), ".tmp-meta-", ".json");
        let contents = serde_json::to_vec(metadata).context("failed to serialize metadata")?;
        fs::write(&temp_path, contents)
            .await
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        fs::rename(&temp_path, &meta_path)
            .await
            .with_context(|| format!("failed to replace {}", meta_path.display()))?;
        Ok(())
    }

    pub async fn serve_legacy_apk(&self) -> Result<Option<Response>> {
        if !self.inner.legacy_apk_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(&self.inner.legacy_apk_path)
            .await
            .with_context(|| format!("failed to read {}", self.inner.legacy_apk_path.display()))?;
        Ok(Some(
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=office-climate.apk",
                )
                .body(Body::from(bytes))
                .expect("valid APK response"),
        ))
    }

    pub async fn serve_hashed_artifact(
        &self,
        app: &str,
        artifact_hash: &str,
    ) -> Result<Option<Response>> {
        if !is_valid_app_name(app) || !is_valid_artifact_hash(artifact_hash) {
            return Ok(None);
        }

        let artifact_path = self.hashed_path(app, artifact_hash);
        if !artifact_path.exists() {
            return Ok(None);
        }

        let bytes = fs::read(&artifact_path)
            .await
            .with_context(|| format!("failed to read {}", artifact_path.display()))?;
        Ok(Some(
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable")
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename={app}.apk"),
                )
                .body(Body::from(bytes))
                .expect("valid artifact response"),
        ))
    }

    pub async fn store_upload(
        &self,
        app: &str,
        mut multipart: Multipart,
        user_email: &str,
    ) -> Result<UploadOutcome, ArtifactError> {
        if !is_valid_app_name(app) {
            return Err(ArtifactError::BadRequest("Invalid app name".to_string()));
        }

        let app_dir = self.app_dir(app);
        fs::create_dir_all(&app_dir)
            .await
            .map_err(|error| ArtifactError::Internal(anyhow!(error).context("create app dir")))?;

        let temp_path = unique_temp_path(&app_dir, ".tmp-artifact-", ".apk");
        let mut temp_file = fs::File::create(&temp_path)
            .await
            .map_err(|error| ArtifactError::Internal(anyhow!(error).context("create temp")))?;
        let mut size_bytes = 0_u64;
        let mut sha256 = Sha256::new();
        let mut file_uploaded = false;
        let mut version_code = None;
        let mut version_name = None;

        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|_| ArtifactError::BadRequest("Expected multipart form upload".to_string()))?
        {
            let Some(name) = field.name().map(str::to_string) else {
                continue;
            };

            match name.as_str() {
                "file" => {
                    file_uploaded = true;
                    while let Some(chunk) = field.chunk().await.map_err(|_| {
                        ArtifactError::BadRequest("Expected multipart form upload".to_string())
                    })? {
                        size_bytes += chunk.len() as u64;
                        if size_bytes > self.inner.max_size_bytes {
                            remove_file_if_exists(&temp_path).await;
                            return Err(ArtifactError::PayloadTooLarge);
                        }
                        sha256.update(&chunk);
                        temp_file.write_all(&chunk).await.map_err(|error| {
                            ArtifactError::Internal(anyhow!(error).context("write artifact chunk"))
                        })?;
                    }
                }
                "version_code" => {
                    let raw = field.text().await.map_err(|_| {
                        ArtifactError::BadRequest("Expected multipart form upload".to_string())
                    })?;
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        version_code = Some(trimmed.parse::<i64>().map_err(|_| {
                            ArtifactError::BadRequest("version_code must be an integer".to_string())
                        })?);
                    }
                }
                "version_name" => {
                    let raw = field.text().await.map_err(|_| {
                        ArtifactError::BadRequest("Expected multipart form upload".to_string())
                    })?;
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        version_name = Some(trimmed.to_string());
                    }
                }
                _ => {}
            }
        }

        temp_file
            .flush()
            .await
            .map_err(|error| ArtifactError::Internal(anyhow!(error).context("flush artifact")))?;
        drop(temp_file);

        if !file_uploaded {
            remove_file_if_exists(&temp_path).await;
            return Err(ArtifactError::BadRequest(
                "Missing multipart field 'file'".to_string(),
            ));
        }

        let latest_path = self.latest_path(app);
        fs::rename(&temp_path, &latest_path)
            .await
            .map_err(|error| {
                ArtifactError::Internal(anyhow!(error).context("replace latest artifact"))
            })?;

        let artifact_hash = format!("{:x}", sha256.finalize())[..8].to_string();
        let hashed_path = self.hashed_path(app, &artifact_hash);
        if !hashed_path.exists() {
            copy_file_atomically(&latest_path, &hashed_path).await?;
        }

        let metadata = ArtifactMetadata {
            artifact_hash,
            uploaded_at: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            size_bytes,
            uploaded_by: user_email.to_string(),
            version_code,
            version_name,
        };
        self.write_metadata(app, &metadata)
            .await
            .map_err(ArtifactError::Internal)?;

        Ok(UploadOutcome {
            app: app.to_string(),
            size_bytes,
            download_url: format!("/apps/{app}/latest.apk"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct UploadOutcome {
    pub app: String,
    pub size_bytes: u64,
    pub download_url: String,
}

#[derive(Debug)]
pub enum ArtifactError {
    BadRequest(String),
    PayloadTooLarge,
    Internal(anyhow::Error),
}

impl IntoResponse for ArtifactError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": message})),
            )
                .into_response(),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({"ok": false, "error": "Artifact exceeds 100 MB limit"})),
            )
                .into_response(),
            Self::Internal(error) => {
                tracing::error!("artifact error: {error:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": "Failed to store artifact"})),
                )
                    .into_response()
            }
        }
    }
}

pub fn is_valid_app_name(app: &str) -> bool {
    let mut chars = app.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

pub fn is_valid_artifact_hash(artifact_hash: &str) -> bool {
    artifact_hash.len() == 8
        && artifact_hash
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

async fn copy_file_atomically(
    source_path: &PathBuf,
    destination_path: &PathBuf,
) -> Result<(), ArtifactError> {
    let temp_path = unique_temp_path(
        destination_path
            .parent()
            .ok_or_else(|| ArtifactError::Internal(anyhow!("destination has no parent")))?,
        ".tmp-artifact-copy-",
        ".apk",
    );
    let mut source = fs::File::open(source_path)
        .await
        .map_err(|error| ArtifactError::Internal(anyhow!(error).context("open source artifact")))?;
    let mut destination = fs::File::create(&temp_path).await.map_err(|error| {
        ArtifactError::Internal(anyhow!(error).context("create hashed artifact temp"))
    })?;
    let mut bytes = Vec::new();
    source
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ArtifactError::Internal(anyhow!(error).context("read source artifact")))?;
    destination.write_all(&bytes).await.map_err(|error| {
        ArtifactError::Internal(anyhow!(error).context("write hashed artifact"))
    })?;
    destination.flush().await.map_err(|error| {
        ArtifactError::Internal(anyhow!(error).context("flush hashed artifact"))
    })?;
    drop(destination);
    fs::rename(&temp_path, destination_path)
        .await
        .map_err(|error| {
            ArtifactError::Internal(anyhow!(error).context("replace hashed artifact"))
        })?;
    Ok(())
}

fn unique_temp_path(dir: impl Into<PathBuf>, prefix: &str, suffix: &str) -> PathBuf {
    let mut bytes = [0_u8; 8];
    OsRng.fill_bytes(&mut bytes);
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    dir.into().join(format!("{prefix}{token}{suffix}"))
}

async fn remove_file_if_exists(path: &PathBuf) {
    let _ = fs::remove_file(path).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_app_names_and_hashes() {
        assert!(is_valid_app_name("office-climate"));
        assert!(is_valid_app_name("a1"));
        assert!(!is_valid_app_name("-bad"));
        assert!(!is_valid_app_name("Office"));
        assert!(is_valid_artifact_hash("1a2b3c4d"));
        assert!(!is_valid_artifact_hash("1A2B3C4D"));
        assert!(!is_valid_artifact_hash("abcd"));
    }
}
