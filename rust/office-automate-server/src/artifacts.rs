use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
    process::Command,
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
    upload_policy: ArtifactUploadPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub artifact_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    pub uploaded_at: String,
    pub size_bytes: u64,
    pub uploaded_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signing_cert_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revocation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_artifact_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rolled_back_from_artifact_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct ArtifactRevocationIndex {
    #[serde(default)]
    artifacts: Vec<ArtifactRevocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ArtifactRevocation {
    artifact_hash: String,
    revoked_at: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement_artifact_hash: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactUploadPolicy {
    pub expected_office_climate_signing_cert_sha256: Option<String>,
    pub apksigner_path: Option<PathBuf>,
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
        Self::with_upload_policy(
            artifacts_root,
            legacy_apk_path,
            max_size_bytes,
            ArtifactUploadPolicy::default(),
        )
    }

    pub fn with_upload_policy(
        artifacts_root: PathBuf,
        legacy_apk_path: PathBuf,
        max_size_bytes: u64,
        upload_policy: ArtifactUploadPolicy,
    ) -> Self {
        Self {
            inner: Arc::new(ArtifactStoreInner {
                artifacts_root,
                legacy_apk_path,
                max_size_bytes,
                upload_policy,
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

    fn revocations_path(&self, app: &str) -> PathBuf {
        self.app_dir(app).join("revocations.json")
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
        let mut metadata: ArtifactMetadata = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", meta_path.display()))?;

        if metadata.sha256.is_empty() && is_valid_artifact_hash(&metadata.artifact_hash) {
            let artifact_path = self.hashed_path(app, &metadata.artifact_hash);
            let bytes = fs::read(&artifact_path)
                .await
                .with_context(|| format!("failed to read {}", artifact_path.display()))?;
            let sha256 = format!("{:x}", Sha256::digest(&bytes));
            if !sha256.starts_with(&metadata.artifact_hash) {
                anyhow::bail!(
                    "artifact hash prefix does not match computed sha256 for {}",
                    artifact_path.display()
                );
            }
            metadata.sha256 = sha256;
            self.write_metadata(app, &metadata).await?;
        }

        Ok(Some(metadata))
    }

    async fn write_metadata(&self, app: &str, metadata: &ArtifactMetadata) -> Result<()> {
        let meta_path = self.meta_path(app);
        let temp_path = unique_temp_path(self.app_dir(app), ".tmp-meta-", ".json");
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
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                )
                .header(
                    header::HeaderName::from_static("x-content-type-options"),
                    "nosniff",
                )
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

        if self.is_artifact_revoked(app, artifact_hash).await? {
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
                .header(
                    header::CONTENT_TYPE,
                    "application/vnd.android.package-archive",
                )
                .header(
                    header::HeaderName::from_static("x-content-type-options"),
                    "nosniff",
                )
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

        let signing_cert_sha256 = self
            .verify_upload(app, &temp_path)
            .await
            .map_err(|error| ArtifactError::BadRequest(error.to_string()))?
            .unwrap_or_default();
        let sha256 = format!("{:x}", sha256.finalize());
        let artifact_hash = sha256[..8].to_string();
        let artifact_revoked = match self.is_artifact_revoked(app, &artifact_hash).await {
            Ok(revoked) => revoked,
            Err(error) => {
                remove_file_if_exists(&temp_path).await;
                return Err(ArtifactError::Internal(error));
            }
        };
        if artifact_revoked {
            remove_file_if_exists(&temp_path).await;
            return Err(ArtifactError::BadRequest(format!(
                "artifact hash {artifact_hash} is revoked"
            )));
        }
        let hashed_path = self.hashed_path(app, &artifact_hash);
        if !hashed_path.exists() {
            copy_file_atomically(&temp_path, &hashed_path).await?;
        }

        let latest_path = self.latest_path(app);
        fs::rename(&temp_path, &latest_path)
            .await
            .map_err(|error| {
                ArtifactError::Internal(anyhow!(error).context("replace latest artifact"))
            })?;

        let metadata = ArtifactMetadata {
            artifact_hash,
            sha256,
            uploaded_at: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            size_bytes,
            uploaded_by: user_email.to_string(),
            signing_cert_sha256,
            version_code,
            version_name,
            revoked_at: None,
            revocation_reason: None,
            replacement_artifact_hash: None,
            rolled_back_from_artifact_hash: None,
            rollback_reason: None,
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

    async fn verify_upload(&self, app: &str, apk_path: &Path) -> Result<Option<String>> {
        let expected = match app {
            "office-climate" => self
                .inner
                .upload_policy
                .expected_office_climate_signing_cert_sha256
                .as_deref()
                .map(normalize_cert_digest)
                .transpose()
                .context("invalid expected office-climate signing certificate SHA-256")?,
            _ => None,
        };
        let Some(expected) = expected else {
            return Ok(None);
        };
        let actual = verify_apk_signing_cert_sha256(
            self.inner.upload_policy.apksigner_path.as_deref(),
            apk_path,
        )
        .await?;
        if actual != expected {
            anyhow::bail!(
                "APK signing certificate SHA-256 {actual} does not match expected {expected}"
            );
        }
        Ok(Some(actual))
    }

    pub async fn revoke_current(
        &self,
        app: &str,
        reason: &str,
        replacement_artifact_hash: Option<&str>,
    ) -> Result<Option<ArtifactMetadata>> {
        if !is_valid_app_name(app) {
            anyhow::bail!("invalid app name {app:?}");
        }
        let reason = reason.trim();
        if reason.is_empty() {
            anyhow::bail!("artifact revocation reason must not be empty");
        }
        let Some(mut metadata) = self.read_metadata(app).await? else {
            return Ok(None);
        };
        if let Some(hash) = replacement_artifact_hash {
            if !is_valid_artifact_hash(hash) {
                anyhow::bail!("invalid replacement artifact hash {hash:?}");
            }
            if !self.hashed_path(app, hash).is_file() {
                anyhow::bail!("replacement artifact does not exist: {hash}");
            }
            metadata.replacement_artifact_hash = Some(hash.to_string());
        }
        let revoked_at = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        metadata.revoked_at = Some(revoked_at.clone());
        metadata.revocation_reason = Some(reason.to_string());
        self.record_artifact_revocation(
            app,
            &metadata.artifact_hash,
            &revoked_at,
            reason,
            metadata.replacement_artifact_hash.as_deref(),
        )
        .await?;
        self.write_metadata(app, &metadata).await?;
        Ok(Some(metadata))
    }

    pub async fn rollback_to(
        &self,
        app: &str,
        artifact_hash: &str,
        reason: &str,
        user_email: &str,
    ) -> Result<Option<ArtifactMetadata>> {
        if !is_valid_app_name(app) || !is_valid_artifact_hash(artifact_hash) {
            anyhow::bail!("invalid app name or artifact hash");
        }
        let reason = reason.trim();
        if reason.is_empty() {
            anyhow::bail!("artifact rollback reason must not be empty");
        }
        if self.is_artifact_revoked(app, artifact_hash).await? {
            anyhow::bail!("rollback target artifact {artifact_hash} is revoked");
        }
        let artifact_path = self.hashed_path(app, artifact_hash);
        if !artifact_path.is_file() {
            return Ok(None);
        }
        let bytes = fs::read(&artifact_path)
            .await
            .with_context(|| format!("failed to read {}", artifact_path.display()))?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        if !sha256.starts_with(artifact_hash) {
            anyhow::bail!("rollback artifact hash prefix does not match SHA-256");
        }
        let signing_cert_sha256 = self
            .verify_upload(app, &artifact_path)
            .await?
            .unwrap_or_default();
        let previous = self.read_metadata(app).await?;
        let previous_hash = previous
            .as_ref()
            .map(|metadata| metadata.artifact_hash.clone())
            .filter(|hash| hash != artifact_hash);
        if let Some(previous_hash) = previous_hash.as_deref() {
            let revoked_at = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
            self.record_artifact_revocation(
                app,
                previous_hash,
                &revoked_at,
                reason,
                Some(artifact_hash),
            )
            .await?;
        }
        let latest_path = self.latest_path(app);
        copy_file_atomically_anyhow(&artifact_path, &latest_path).await?;
        let metadata = ArtifactMetadata {
            artifact_hash: artifact_hash.to_string(),
            sha256,
            uploaded_at: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            size_bytes: bytes.len() as u64,
            uploaded_by: user_email.to_string(),
            signing_cert_sha256,
            version_code: None,
            version_name: None,
            revoked_at: None,
            revocation_reason: None,
            replacement_artifact_hash: None,
            rolled_back_from_artifact_hash: previous.map(|metadata| metadata.artifact_hash),
            rollback_reason: Some(reason.to_string()),
        };
        self.write_metadata(app, &metadata).await?;
        Ok(Some(metadata))
    }

    async fn is_artifact_revoked(&self, app: &str, artifact_hash: &str) -> Result<bool> {
        let metadata_revoked = self.read_metadata(app).await?.is_some_and(|metadata| {
            metadata.revoked_at.is_some() && metadata.artifact_hash == artifact_hash
        });
        if metadata_revoked {
            return Ok(true);
        }
        let revocations = self.read_revocations(app).await?;
        Ok(revocations
            .artifacts
            .iter()
            .any(|revocation| revocation.artifact_hash == artifact_hash))
    }

    async fn record_artifact_revocation(
        &self,
        app: &str,
        artifact_hash: &str,
        revoked_at: &str,
        reason: &str,
        replacement_artifact_hash: Option<&str>,
    ) -> Result<()> {
        if !is_valid_artifact_hash(artifact_hash) {
            anyhow::bail!("invalid revoked artifact hash {artifact_hash:?}");
        }
        let mut revocations = self.read_revocations(app).await?;
        revocations
            .artifacts
            .retain(|revocation| revocation.artifact_hash != artifact_hash);
        revocations.artifacts.push(ArtifactRevocation {
            artifact_hash: artifact_hash.to_string(),
            revoked_at: revoked_at.to_string(),
            reason: reason.to_string(),
            replacement_artifact_hash: replacement_artifact_hash.map(str::to_string),
        });
        self.write_revocations(app, &revocations).await
    }

    async fn read_revocations(&self, app: &str) -> Result<ArtifactRevocationIndex> {
        let path = self.revocations_path(app);
        if !path.exists() {
            return Ok(ArtifactRevocationIndex::default());
        }
        let contents = fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))
    }

    async fn write_revocations(
        &self,
        app: &str,
        revocations: &ArtifactRevocationIndex,
    ) -> Result<()> {
        let path = self.revocations_path(app);
        let temp_path = unique_temp_path(self.app_dir(app), ".tmp-revocations-", ".json");
        let contents =
            serde_json::to_vec(revocations).context("failed to serialize revocations")?;
        fs::write(&temp_path, contents)
            .await
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        fs::rename(&temp_path, &path)
            .await
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
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

pub fn is_valid_sha256_digest(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

pub fn normalize_cert_digest(value: &str) -> Result<String> {
    let normalized = value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if !is_valid_sha256_digest(&normalized) {
        anyhow::bail!("invalid SHA-256 certificate digest {value:?}");
    }
    Ok(normalized)
}

pub async fn verify_apk_signing_cert_sha256(
    apksigner_path: Option<&Path>,
    apk_path: &Path,
) -> Result<String> {
    let executable = apksigner_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("apksigner"));
    let output = Command::new(&executable)
        .arg("verify")
        .arg("--print-certs")
        .arg(apk_path)
        .output()
        .await
        .with_context(|| format!("failed to run {}", executable.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("apksigner rejected APK: {}", stderr.trim());
    }
    parse_apksigner_sha256_digest(&String::from_utf8_lossy(&output.stdout))
        .context("apksigner output did not include a signer certificate SHA-256 digest")
}

fn parse_apksigner_sha256_digest(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("sha-256") || !lower.contains("digest") {
            return None;
        }
        let (_, value) = line.split_once(':')?;
        normalize_cert_digest(value).ok()
    })
}

async fn copy_file_atomically_anyhow(source_path: &Path, destination_path: &Path) -> Result<()> {
    let temp_path = unique_temp_path(
        destination_path
            .parent()
            .context("destination has no parent")?,
        ".tmp-artifact-copy-",
        ".apk",
    );
    fs::copy(source_path, &temp_path)
        .await
        .with_context(|| format!("failed to copy {}", source_path.display()))?;
    fs::rename(&temp_path, destination_path)
        .await
        .with_context(|| format!("failed to replace {}", destination_path.display()))?;
    Ok(())
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

    #[tokio::test]
    async fn read_metadata_hydrates_legacy_short_hash_metadata() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = ArtifactStore::new(
            temp_dir.path().join("apps"),
            temp_dir.path().join("legacy.apk"),
        );
        let app_dir = store.app_dir("office-climate");
        fs::create_dir_all(&app_dir).await.expect("app dir");
        fs::write(app_dir.join("dd37c2d7.apk"), b"apk")
            .await
            .expect("hashed artifact");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"dd37c2d7","uploaded_at":"2026-06-05T00:00:00","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .await
        .expect("legacy metadata");

        let metadata = store
            .read_metadata("office-climate")
            .await
            .expect("metadata read")
            .expect("metadata");

        assert_eq!(
            metadata.sha256,
            "dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04"
        );
        let persisted = fs::read_to_string(app_dir.join("meta.json"))
            .await
            .expect("persisted metadata");
        assert!(persisted.contains(
            r#""sha256":"dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04""#
        ));
    }

    #[tokio::test]
    async fn read_metadata_rejects_legacy_hash_prefix_mismatch() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = ArtifactStore::new(
            temp_dir.path().join("apps"),
            temp_dir.path().join("legacy.apk"),
        );
        let app_dir = store.app_dir("office-climate");
        fs::create_dir_all(&app_dir).await.expect("app dir");
        fs::write(app_dir.join("1a2b3c4d.apk"), b"apk")
            .await
            .expect("hashed artifact");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"1a2b3c4d","uploaded_at":"2026-06-05T00:00:00","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .await
        .expect("legacy metadata");

        let error = store
            .read_metadata("office-climate")
            .await
            .expect_err("metadata should reject mismatched digest");

        assert!(error.to_string().contains("artifact hash prefix"));
    }

    #[test]
    fn validates_app_names_and_hashes() {
        assert!(is_valid_app_name("office-climate"));
        assert!(is_valid_app_name("a1"));
        assert!(!is_valid_app_name("-bad"));
        assert!(!is_valid_app_name("Office"));
        assert!(is_valid_artifact_hash("1a2b3c4d"));
        assert!(!is_valid_artifact_hash("1A2B3C4D"));
        assert!(!is_valid_artifact_hash("abcd"));
        assert!(is_valid_sha256_digest(
            "dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04"
        ));
        assert!(!is_valid_sha256_digest("1a2b3c4d"));
        assert!(!is_valid_sha256_digest(
            "DD37C2D7274F7EA982CB83390C36918FEE9CE8889073C44B68CDC00BDB8C3E04"
        ));
    }

    #[test]
    fn parses_apksigner_sha256_digest() {
        let output = "Signer #1 certificate SHA-256 digest: AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99";
        assert_eq!(
            parse_apksigner_sha256_digest(output).expect("digest"),
            "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
        );
    }

    #[tokio::test]
    async fn revoke_current_marks_metadata_revoked() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = ArtifactStore::new(
            temp_dir.path().join("apps"),
            temp_dir.path().join("legacy.apk"),
        );
        let app_dir = store.app_dir("office-climate");
        fs::create_dir_all(&app_dir).await.expect("app dir");
        fs::write(app_dir.join("dd37c2d7.apk"), b"apk")
            .await
            .expect("hashed artifact");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"dd37c2d7","sha256":"dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04","uploaded_at":"2026-06-05T00:00:00","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .await
        .expect("metadata");

        let metadata = store
            .revoke_current("office-climate", "bad release", None)
            .await
            .expect("revoke")
            .expect("metadata");

        assert!(metadata.revoked_at.is_some());
        assert_eq!(metadata.revocation_reason.as_deref(), Some("bad release"));
        assert!(
            store
                .serve_hashed_artifact("office-climate", "dd37c2d7")
                .await
                .expect("serve artifact")
                .is_none()
        );
        let revocations = store
            .read_revocations("office-climate")
            .await
            .expect("revocations");
        assert_eq!(revocations.artifacts[0].artifact_hash, "dd37c2d7");
    }

    #[tokio::test]
    async fn rollback_records_previous_artifact_revocation() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let store = ArtifactStore::new(
            temp_dir.path().join("apps"),
            temp_dir.path().join("legacy.apk"),
        );
        let app_dir = store.app_dir("office-climate");
        fs::create_dir_all(&app_dir).await.expect("app dir");

        let previous_bytes = b"bad apk";
        let previous_sha256 = format!("{:x}", Sha256::digest(previous_bytes));
        let previous_hash = &previous_sha256[..8];
        fs::write(app_dir.join(format!("{previous_hash}.apk")), previous_bytes)
            .await
            .expect("previous artifact");

        let rollback_bytes = b"known good apk";
        let rollback_sha256 = format!("{:x}", Sha256::digest(rollback_bytes));
        let rollback_hash = &rollback_sha256[..8];
        fs::write(app_dir.join(format!("{rollback_hash}.apk")), rollback_bytes)
            .await
            .expect("rollback artifact");

        fs::write(
            app_dir.join("meta.json"),
            format!(
                r#"{{"artifact_hash":"{previous_hash}","sha256":"{previous_sha256}","uploaded_at":"2026-06-05T00:00:00","size_bytes":{},"uploaded_by":"test@example.com"}}"#,
                previous_bytes.len()
            ),
        )
        .await
        .expect("metadata");

        let metadata = store
            .rollback_to(
                "office-climate",
                rollback_hash,
                "bad release",
                "local_operator",
            )
            .await
            .expect("rollback")
            .expect("metadata");

        assert_eq!(metadata.artifact_hash, rollback_hash);
        assert_eq!(
            metadata.rolled_back_from_artifact_hash.as_deref(),
            Some(previous_hash)
        );
        assert!(
            store
                .serve_hashed_artifact("office-climate", previous_hash)
                .await
                .expect("serve previous")
                .is_none()
        );
        assert!(
            store
                .serve_hashed_artifact("office-climate", rollback_hash)
                .await
                .expect("serve rollback")
                .is_some()
        );

        let error = store
            .rollback_to(
                "office-climate",
                previous_hash,
                "try bad rollback",
                "local_operator",
            )
            .await
            .expect_err("revoked rollback target should fail");
        assert!(error.to_string().contains("is revoked"));
    }
}
