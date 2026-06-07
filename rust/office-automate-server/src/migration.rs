#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::{
    fmt::Write as FmtWrite,
    fs,
    fs::File,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use rusqlite::{Connection, DatabaseName, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    artifacts::{ArtifactMetadata, is_valid_artifact_hash, is_valid_sha256_digest},
    config::AppConfig,
    db,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReport {
    pub snapshot_dir: PathBuf,
    pub files_copied: usize,
    pub validations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotManifest {
    created_at: String,
    files_copied: usize,
    validations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CloudflaredConfig {
    #[serde(rename = "credentials-file")]
    credentials_file: Option<PathBuf>,
    ingress: Option<Vec<serde_yaml::Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloudflaredCredentialSnapshot {
    source_path: PathBuf,
    snapshot_relative_path: PathBuf,
}

const SNAPSHOT_CREATE_ATTEMPTS: usize = 100;

pub fn create_pre_cutover_snapshot(
    config: &AppConfig,
    config_path: &Path,
    output_dir: &Path,
    cloudflared_config_path: Option<&Path>,
) -> Result<SnapshotReport> {
    ensure_readable_file("config", config_path)?;
    ensure_readable_file("office database", &config.runtime.database_path)?;
    ensure_writable_directory(output_dir)?;

    let snapshot_dir = create_private_snapshot_dir(output_dir)?;

    let mut files_copied = 0_usize;
    let mut validations = Vec::new();

    copy_file(config_path, &snapshot_dir.join("config.yaml"))?;
    files_copied += 1;
    validations.push("config readable".to_string());
    validations.extend(validate_config_material(config)?);

    let office_db_snapshot = snapshot_dir.join("office_climate.db");
    backup_sqlite(
        "office database",
        &config.runtime.database_path,
        &office_db_snapshot,
    )?;
    files_copied += 1;
    db::migrate_database(&office_db_snapshot).with_context(|| {
        format!(
            "failed to migrate snapshot {}",
            office_db_snapshot.display()
        )
    })?;
    quick_check_sqlite("office database", &office_db_snapshot)?;
    validations.push("office database migrated on snapshot copy".to_string());

    files_copied += copy_optional_sqlite(
        "telemetry database",
        &config.runtime.telemetry_db_path,
        &snapshot_dir.join("telemetry.db"),
        &mut validations,
    )?;
    files_copied += copy_optional_sqlite(
        "tool usage database",
        &config.runtime.tool_usage_db_path,
        &snapshot_dir.join("tool_usage.db"),
        &mut validations,
    )?;
    if config.runtime.session_tool_usage_db_path == config.runtime.tool_usage_db_path {
        validations.push(
            "session tool usage database shares project tool usage database snapshot".to_string(),
        );
    } else {
        files_copied += copy_optional_sqlite(
            "session tool usage database",
            &config.runtime.session_tool_usage_db_path,
            &snapshot_dir.join("session_tool_usage.db"),
            &mut validations,
        )?;
    }
    files_copied += copy_optional_sqlite(
        "engram database",
        &config.runtime.engram_db_path,
        &snapshot_dir.join("engram_state.db"),
        &mut validations,
    )?;

    files_copied += copy_optional_file(
        "engram registry",
        &config.runtime.engram_registry_path,
        &snapshot_dir.join("engram_concept_registry.md"),
        &mut validations,
    )?;
    files_copied += copy_optional_file(
        "worktree map",
        &config.runtime.data_dir.join("worktree_map.json"),
        &snapshot_dir.join("worktree_map.json"),
        &mut validations,
    )?;
    files_copied += copy_optional_file(
        "legacy APK",
        &config.runtime.legacy_apk_path,
        &snapshot_dir.join("app-debug.apk"),
        &mut validations,
    )?;
    if let Some(cloudflared_config_path) = cloudflared_config_path {
        let cloudflared_credentials =
            validate_cloudflared_config(cloudflared_config_path, &mut validations)?;
        let cloudflared_credential_snapshot_path = snapshot_dir
            .join("cloudflared")
            .join(&cloudflared_credentials.snapshot_relative_path);
        write_cloudflared_snapshot_config(
            cloudflared_config_path,
            &snapshot_dir.join("cloudflared").join("config.yml"),
            &cloudflared_credential_snapshot_path,
        )?;
        files_copied += 1;
        copy_file(
            &cloudflared_credentials.source_path,
            &cloudflared_credential_snapshot_path,
        )?;
        files_copied += 1;
        validations.push(format!(
            "cloudflared config and credential file copied: {}",
            cloudflared_credentials.snapshot_relative_path.display()
        ));
    } else {
        validations.push("cloudflared config validation skipped".to_string());
    }

    if config.runtime.artifacts_dir.exists() {
        validate_artifact_metadata(&config.runtime.artifacts_dir, &mut validations)?;
        files_copied +=
            copy_dir_recursive(&config.runtime.artifacts_dir, &snapshot_dir.join("apps"))?;
    } else {
        validations.push(format!(
            "optional artifacts directory missing: {}",
            config.runtime.artifacts_dir.display()
        ));
    }

    write_restore_env(&snapshot_dir, config, cloudflared_config_path.is_some())?;
    files_copied += 1;
    validations.push("effective restore environment written: restore-env.sh".to_string());

    write_manifest(
        &snapshot_dir,
        SnapshotManifest {
            created_at: Local::now().to_rfc3339(),
            files_copied,
            validations: validations.clone(),
        },
    )?;

    Ok(SnapshotReport {
        snapshot_dir,
        files_copied,
        validations,
    })
}

fn create_private_snapshot_dir(output_dir: &Path) -> Result<PathBuf> {
    for attempt in 0..SNAPSHOT_CREATE_ATTEMPTS {
        let snapshot_dir = unique_snapshot_dir(output_dir, attempt);
        match create_private_dir(&snapshot_dir) {
            Ok(()) => {
                return fs::canonicalize(&snapshot_dir)
                    .with_context(|| format!("failed to resolve {}", snapshot_dir.display()));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", snapshot_dir.display()));
            }
        }
    }

    bail!(
        "failed to create a unique snapshot directory under {} after {SNAPSHOT_CREATE_ATTEMPTS} attempts",
        output_dir.display()
    );
}

fn unique_snapshot_dir(output_dir: &Path, attempt: usize) -> PathBuf {
    output_dir.join(snapshot_dir_name(Local::now(), attempt))
}

fn snapshot_dir_name(created_at: DateTime<Local>, attempt: usize) -> String {
    format!(
        "office-automate-precutover-{}-{}-{attempt:02}",
        created_at.format("%Y%m%d-%H%M%S-%f"),
        std::process::id()
    )
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }

    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn ensure_readable_file(label: &str, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("{label} is not readable: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{label} is not a file: {}", path.display());
    }
    File::open(path).with_context(|| format!("{label} is not readable: {}", path.display()))?;
    Ok(())
}

fn ensure_writable_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    let probe = path.join(format!(
        ".office-automate-write-test-{}",
        std::process::id()
    ));
    {
        let mut file =
            File::create(&probe).with_context(|| format!("failed to write {}", probe.display()))?;
        file.write_all(b"ok")
            .with_context(|| format!("failed to write {}", probe.display()))?;
    }
    fs::remove_file(&probe).with_context(|| format!("failed to remove {}", probe.display()))?;
    Ok(())
}

fn validate_config_material(config: &AppConfig) -> Result<Vec<String>> {
    let mut validations = Vec::new();
    match &config.orchestrator.google_oauth {
        Some(oauth) => {
            ensure_non_empty_config_value(
                "OAuth client_id",
                &oauth.client_id,
                "google_oauth is configured",
            )?;
            ensure_non_empty_config_value(
                "OAuth client_secret",
                &oauth.client_secret,
                "google_oauth is configured",
            )?;
            validations.push("OAuth material present in config".to_string());
        }
        None => {
            validations.push("OAuth material absent from config".to_string());
        }
    }
    if config.erv.is_configured() {
        validations.push("ERV local credential material present in config".to_string());
    } else {
        validations.push("ERV local credential material absent from config".to_string());
    }
    if config.mitsubishi.is_configured() {
        validations.push("HVAC credential material present in config".to_string());
    } else {
        validations.push("HVAC credential material absent from config".to_string());
    }
    Ok(validations)
}

fn ensure_non_empty_config_value(label: &str, value: &str, context: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must be non-empty when {context}");
    }
    Ok(())
}

fn validate_cloudflared_config(
    path: &Path,
    validations: &mut Vec<String>,
) -> Result<CloudflaredCredentialSnapshot> {
    ensure_readable_file("cloudflared config", path)?;
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let config: CloudflaredConfig = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse cloudflared config {}", path.display()))?;

    let credentials_file = config.credentials_file.with_context(|| {
        format!(
            "cloudflared config missing credentials-file: {}",
            path.display()
        )
    })?;
    let snapshot_relative_path = cloudflared_snapshot_credential_path(&credentials_file)?;
    let credentials_file = resolve_cloudflared_path(path, credentials_file);
    ensure_readable_file("cloudflared credentials file", &credentials_file)?;
    validate_cloudflared_credentials(&credentials_file)?;

    let ingress = config
        .ingress
        .with_context(|| format!("cloudflared config missing ingress: {}", path.display()))?;
    if ingress.is_empty() {
        bail!("cloudflared config ingress is empty: {}", path.display());
    }

    validations.push(format!("cloudflared config readable: {}", path.display()));
    validations.push(format!(
        "cloudflared credential file readable: {}",
        credentials_file.display()
    ));
    validations.push(format!(
        "cloudflared ingress rules present: {}",
        ingress.len()
    ));
    Ok(CloudflaredCredentialSnapshot {
        source_path: credentials_file,
        snapshot_relative_path,
    })
}

fn resolve_cloudflared_path(config_path: &Path, configured_path: PathBuf) -> PathBuf {
    if configured_path.is_absolute() {
        return configured_path;
    }
    if let Some(configured_path) = configured_path.to_str() {
        if let Some(rest) = configured_path.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
    }
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(configured_path)
}

fn write_cloudflared_snapshot_config(
    source: &Path,
    destination: &Path,
    credential_path: &Path,
) -> Result<()> {
    let contents = fs::read_to_string(source)
        .with_context(|| format!("failed to read {}", source.display()))?;
    let mut config: serde_yaml::Value = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse cloudflared config {}", source.display()))?;
    let mapping = config.as_mapping_mut().with_context(|| {
        format!(
            "cloudflared config root must be a mapping: {}",
            source.display()
        )
    })?;
    mapping.insert(
        serde_yaml::Value::String("credentials-file".to_string()),
        serde_yaml::Value::String(credential_path.to_string_lossy().into_owned()),
    );
    let rendered = serde_yaml::to_string(&config)
        .with_context(|| format!("failed to render cloudflared config {}", source.display()))?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(destination, rendered)
        .with_context(|| format!("failed to write {}", destination.display()))
}

fn cloudflared_snapshot_credential_path(configured_path: &Path) -> Result<PathBuf> {
    if configured_path.is_absolute()
        || configured_path
            .to_str()
            .is_some_and(|value| value.starts_with("~/"))
    {
        return configured_path
            .file_name()
            .map(PathBuf::from)
            .context("cloudflared credentials-file has no filename");
    }

    let mut snapshot_path = PathBuf::new();
    for component in configured_path.components() {
        match component {
            std::path::Component::Normal(value) => snapshot_path.push(value),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                bail!(
                    "cloudflared credentials-file uses unsupported snapshot path component: {}",
                    configured_path.display()
                );
            }
        }
    }

    if snapshot_path.as_os_str().is_empty() {
        bail!(
            "cloudflared credentials-file has no snapshot filename: {}",
            configured_path.display()
        );
    }
    Ok(snapshot_path)
}

fn write_restore_env(
    snapshot_dir: &Path,
    config: &AppConfig,
    include_cloudflared_config: bool,
) -> Result<()> {
    let path = snapshot_dir.join("restore-env.sh");
    let session_tool_usage_db =
        if config.runtime.session_tool_usage_db_path == config.runtime.tool_usage_db_path {
            "tool_usage.db"
        } else {
            "session_tool_usage.db"
        };

    let mut contents = String::new();
    writeln!(
        &mut contents,
        "#!/usr/bin/env bash\n# Generated by office-automate-server snapshot.\n# Contains deployment restore values and may include secrets; keep permissions restricted.\nset -euo pipefail\nSNAPSHOT_DIR=\"$(cd \"$(dirname \"${{BASH_SOURCE[0]}}\")\" && pwd)\"\n"
    )?;
    write_snapshot_env(&mut contents, "OFFICE_AUTOMATE_CONFIG", "config.yaml")?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_ROOT",
        &config.runtime.root.display().to_string(),
    )?;
    writeln!(
        &mut contents,
        "export OFFICE_AUTOMATE_DATA_DIR=\"${{SNAPSHOT_DIR}}\""
    )?;
    if let Some(base_url) = config.runtime.base_url.as_deref() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_BASE_URL", base_url)?;
    }
    if let Some(public_url) = config.runtime.public_url.as_deref() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_PUBLIC_URL", public_url)?;
    }
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_MQTT_HOST",
        &config.runtime.mqtt_host,
    )?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_MQTT_PORT",
        &config.runtime.mqtt_port.to_string(),
    )?;

    if !config.yolink.uaid.trim().is_empty() {
        write_literal_env(
            &mut contents,
            "OFFICE_AUTOMATE_YOLINK_UAID",
            &config.yolink.uaid,
        )?;
    }
    if !config.yolink.secret_key.trim().is_empty() {
        write_literal_env(
            &mut contents,
            "OFFICE_AUTOMATE_YOLINK_SECRET_KEY",
            &config.yolink.secret_key,
        )?;
    }
    if let Some(username) = config.mitsubishi.username.as_deref() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_KUMO_USERNAME", username)?;
    }
    if let Some(password) = config.mitsubishi.password.as_deref() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_KUMO_PASSWORD", password)?;
    }
    if let Some(serial) = config.mitsubishi.device_serial.as_deref() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_KUMO_DEVICE_SERIAL", serial)?;
    }
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_KUMO_BASE_URL",
        &config.mitsubishi.base_url,
    )?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_KUMO_ACTIVE_CONTROL_ENABLED",
        bool_env(config.mitsubishi.active_control_enabled),
    )?;

    if !config.erv.ip.trim().is_empty() {
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_ERV_IP", &config.erv.ip)?;
    }
    if !config.erv.device_id.trim().is_empty() {
        write_literal_env(
            &mut contents,
            "OFFICE_AUTOMATE_ERV_DEVICE_ID",
            &config.erv.device_id,
        )?;
    }
    if !config.erv.local_key.trim().is_empty() {
        write_literal_env(
            &mut contents,
            "OFFICE_AUTOMATE_ERV_LOCAL_KEY",
            &config.erv.local_key,
        )?;
    }
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_ERV_ACTIVE_CONTROL_ENABLED",
        bool_env(config.erv.active_control_enabled),
    )?;

    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_PRESENCE_ENABLED",
        bool_env(config.presence.enabled),
    )?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_PRESENCE_POLL_INTERVAL_SECONDS",
        &config.presence.poll_interval_seconds.to_string(),
    )?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_PRESENCE_COMMAND_TIMEOUT_SECONDS",
        &config.presence.command_timeout_seconds.to_string(),
    )?;

    if !config.telemetry.repos.is_empty() {
        let repos = config
            .telemetry
            .repos
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(",");
        write_literal_env(&mut contents, "OFFICE_AUTOMATE_TELEMETRY_REPOS", &repos)?;
    }
    write_snapshot_env(
        &mut contents,
        "OFFICE_AUTOMATE_TELEMETRY_DB",
        "telemetry.db",
    )?;
    write_snapshot_env(
        &mut contents,
        "OFFICE_AUTOMATE_TOOL_USAGE_DB",
        "tool_usage.db",
    )?;
    write_snapshot_env(
        &mut contents,
        "OFFICE_AUTOMATE_SESSION_TOOL_USAGE_DB",
        session_tool_usage_db,
    )?;
    write_snapshot_env(
        &mut contents,
        "OFFICE_AUTOMATE_ENGRAM_DB",
        "engram_state.db",
    )?;
    write_snapshot_env(
        &mut contents,
        "OFFICE_AUTOMATE_ENGRAM_REGISTRY",
        "engram_concept_registry.md",
    )?;
    write_literal_env(
        &mut contents,
        "OFFICE_AUTOMATE_TELEMETRY_DAYS",
        &config.telemetry.days.to_string(),
    )?;
    if include_cloudflared_config {
        write_snapshot_env(
            &mut contents,
            "CLOUDFLARED_CONFIG",
            "cloudflared/config.yml",
        )?;
    }

    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))?;
    Ok(())
}

fn write_literal_env(contents: &mut String, name: &str, value: &str) -> Result<()> {
    writeln!(contents, "export {name}={}", shell_quote(value))?;
    Ok(())
}

fn write_snapshot_env(contents: &mut String, name: &str, relative_path: &str) -> Result<()> {
    writeln!(
        contents,
        "export {name}=\"${{SNAPSHOT_DIR}}/{relative_path}\""
    )?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn bool_env(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn validate_cloudflared_credentials(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| {
        format!(
            "cloudflared credentials file is not readable: {}",
            path.display()
        )
    })?;
    if metadata.len() == 0 {
        bail!("cloudflared credentials file is empty: {}", path.display());
    }

    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let credentials: serde_json::Value = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse cloudflared credentials {}", path.display()))?;
    let object = credentials.as_object().with_context(|| {
        format!(
            "cloudflared credentials file is not a JSON object: {}",
            path.display()
        )
    })?;

    for key in ["AccountTag", "TunnelID", "TunnelSecret"] {
        let present = object
            .get(key)
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty());
        if !present {
            bail!(
                "cloudflared credentials file missing {key}: {}",
                path.display()
            );
        }
    }

    Ok(())
}

fn copy_optional_sqlite(
    label: &str,
    source: &Path,
    destination: &Path,
    validations: &mut Vec<String>,
) -> Result<usize> {
    if !source.exists() {
        validations.push(format!("optional {label} missing: {}", source.display()));
        return Ok(0);
    }

    ensure_readable_file(label, source)?;
    backup_sqlite(label, source, destination)?;
    quick_check_sqlite(label, destination)?;
    validations.push(format!("{label} quick_check ok"));
    Ok(1)
}

fn backup_sqlite(label: &str, source: &Path, destination: &Path) -> Result<()> {
    ensure_readable_file(label, source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if destination.exists() {
        fs::remove_file(destination)
            .with_context(|| format!("failed to replace {}", destination.display()))?;
    }
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {label}: {}", source.display()))?;
    connection
        .backup(DatabaseName::Main, destination, None)
        .with_context(|| {
            format!(
                "failed to create consistent SQLite backup of {label}: {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
    Ok(())
}

fn copy_optional_file(
    label: &str,
    source: &Path,
    destination: &Path,
    validations: &mut Vec<String>,
) -> Result<usize> {
    if !source.exists() {
        validations.push(format!("optional {label} missing: {}", source.display()));
        return Ok(0);
    }

    ensure_readable_file(label, source)?;
    copy_file(source, destination)?;
    validations.push(format!("{label} readable"));
    Ok(1)
}

fn quick_check_sqlite(label: &str, path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {label}: {}", path.display()))?;
    let result: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .with_context(|| format!("failed to quick_check {label}: {}", path.display()))?;
    if result != "ok" {
        bail!(
            "{label} quick_check failed for {}: {result}",
            path.display()
        );
    }
    Ok(())
}

fn validate_artifact_metadata(artifacts_dir: &Path, validations: &mut Vec<String>) -> Result<()> {
    let mut apps_checked = 0_usize;
    for entry in fs::read_dir(artifacts_dir)
        .with_context(|| format!("failed to read artifacts dir {}", artifacts_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read entry in {}", artifacts_dir.display()))?;
        let app_dir = entry.path();
        if !app_dir.is_dir() {
            continue;
        }

        let meta_path = app_dir.join("meta.json");
        if !meta_path.exists() {
            continue;
        }
        let contents = fs::read_to_string(&meta_path)
            .with_context(|| format!("failed to read {}", meta_path.display()))?;
        let mut metadata: ArtifactMetadata = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", meta_path.display()))?;
        if !is_valid_artifact_hash(&metadata.artifact_hash) {
            bail!(
                "invalid artifact hash in {}: {}",
                meta_path.display(),
                metadata.artifact_hash
            );
        }
        let hashed_path = app_dir.join(format!("{}.apk", metadata.artifact_hash));
        if !hashed_path.is_file() {
            bail!(
                "artifact metadata references missing APK: {}",
                hashed_path.display()
            );
        }
        let hashed_bytes = fs::read(&hashed_path)
            .with_context(|| format!("failed to read {}", hashed_path.display()))?;
        let actual_sha256 = format!("{:x}", Sha256::digest(&hashed_bytes));
        if metadata.sha256.is_empty() {
            if !actual_sha256.starts_with(&metadata.artifact_hash) {
                bail!(
                    "artifact hash prefix does not match computed sha256 in {}",
                    meta_path.display()
                );
            }
            metadata.sha256 = actual_sha256.clone();
            let updated_metadata =
                serde_json::to_vec(&metadata).context("failed to serialize hydrated metadata")?;
            fs::write(&meta_path, updated_metadata)
                .with_context(|| format!("failed to hydrate {}", meta_path.display()))?;
        }
        if !is_valid_sha256_digest(&metadata.sha256) {
            bail!(
                "invalid artifact sha256 in {}: {}",
                meta_path.display(),
                metadata.sha256
            );
        }
        if !metadata.sha256.starts_with(&metadata.artifact_hash) {
            bail!(
                "artifact hash prefix does not match sha256 in {}",
                meta_path.display()
            );
        }
        if actual_sha256 != metadata.sha256 {
            bail!(
                "artifact metadata sha256 does not match APK {}",
                hashed_path.display()
            );
        }
        let latest_path = app_dir.join("latest.apk");
        if !latest_path.is_file() {
            bail!(
                "artifact metadata has no latest APK: {}",
                latest_path.display()
            );
        }
        apps_checked += 1;
    }
    validations.push(format!(
        "artifact metadata checked for {apps_checked} app(s)"
    ));
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<usize> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    let mut files_copied = 0_usize;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            files_copied += copy_dir_recursive(&source_path, &destination_path)?;
        } else if source_path.is_file() {
            copy_file(&source_path, &destination_path)?;
            files_copied += 1;
        }
    }
    Ok(files_copied)
}

fn write_manifest(snapshot_dir: &Path, manifest: SnapshotManifest) -> Result<()> {
    let path = snapshot_dir.join("manifest.json");
    let contents =
        serde_json::to_vec_pretty(&manifest).context("failed to serialize snapshot manifest")?;
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ErvConfig, GoogleOAuthConfig, MitsubishiConfig, OrchestratorConfig, PresenceConfig,
        QingpingConfig, RuntimeConfig, TelemetryConfig, ThresholdsConfig, YoLinkConfig,
    };

    fn test_config(root: &Path, config_path: PathBuf, database_path: PathBuf) -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            presence: PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            erv: ErvConfig::default(),
            mitsubishi: MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            telemetry: TelemetryConfig::default(),
            runtime: RuntimeConfig {
                root: root.to_path_buf(),
                config_path,
                data_dir: root.join("data"),
                database_path,
                frontend_dist: root.join("frontend/dist"),
                artifacts_dir: root.join("data/apps"),
                legacy_apk_path: root.join("data/app-debug.apk"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
                telemetry_db_path: root.join("data/telemetry.db"),
                session_tool_usage_db_path: root.join("data/claude_tool_usage.db"),
                tool_usage_db_path: root.join("data/tool_usage.db"),
                engram_db_path: root.join("data/engram_state.db"),
                engram_registry_path: root.join("data/engram_concept_registry.md"),
            },
        }
    }

    fn create_sqlite(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("sqlite parent");
        }
        let connection = Connection::open(path).expect("sqlite");
        connection
            .execute_batch("CREATE TABLE test_data (id INTEGER PRIMARY KEY);")
            .expect("sqlite schema");
    }

    fn read_test_value(path: &Path, id: i64) -> String {
        let connection = Connection::open(path).expect("sqlite");
        connection
            .query_row("SELECT value FROM test_data WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .expect("test data value")
    }

    fn cloudflared_credentials_file(path: &Path) -> String {
        let contents = fs::read_to_string(path).expect("cloudflared config contents");
        let config: serde_yaml::Value =
            serde_yaml::from_str(&contents).expect("cloudflared config yaml");
        config
            .get("credentials-file")
            .and_then(serde_yaml::Value::as_str)
            .expect("credentials-file")
            .to_string()
    }

    #[test]
    fn snapshot_dir_name_includes_retry_attempt_suffix() {
        let created_at = Local::now();

        let first = snapshot_dir_name(created_at.clone(), 0);
        let retry = snapshot_dir_name(created_at, 1);

        assert_ne!(first, retry);
        assert!(first.ends_with("-00"));
        assert!(retry.ends_with("-01"));
    }

    #[test]
    fn snapshot_copies_config_database_artifacts_and_manifest() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let app_dir = root.join("data/apps/office-climate");
        fs::create_dir_all(&app_dir).expect("app dir");
        fs::write(app_dir.join("latest.apk"), b"apk").expect("latest");
        fs::write(app_dir.join("dd37c2d7.apk"), b"apk").expect("hashed");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"dd37c2d7","sha256":"dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04","uploaded_at":"2026-06-05T00:00:00Z","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .expect("metadata");
        fs::write(
            root.join("data/worktree_map.json"),
            r#"{"office-automate-pr76":"office-automate"}"#,
        )
        .expect("worktree map");
        fs::write(root.join("data/app-debug.apk"), b"legacy").expect("legacy apk");

        let config = test_config(root, config_path.clone(), database_path);
        let output_dir = root.join("snapshots");
        let report = create_pre_cutover_snapshot(&config, &config_path, &output_dir, None)
            .expect("snapshot succeeds");

        assert!(report.snapshot_dir.join("config.yaml").is_file());
        #[cfg(unix)]
        {
            let mode = fs::metadata(&report.snapshot_dir)
                .expect("snapshot dir metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
        assert!(report.snapshot_dir.join("office_climate.db").is_file());
        assert!(report.snapshot_dir.join("worktree_map.json").is_file());
        assert!(
            report
                .snapshot_dir
                .join("apps/office-climate/meta.json")
                .is_file()
        );
        assert!(report.snapshot_dir.join("app-debug.apk").is_file());
        assert!(
            !report
                .snapshot_dir
                .join("legacy/office-climate.apk")
                .exists()
        );
        assert!(report.snapshot_dir.join("manifest.json").is_file());
        assert!(report.files_copied >= 6);
        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "office database migrated on snapshot copy")
        );
        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "worktree map readable")
        );
    }

    #[test]
    fn snapshot_validates_non_empty_oauth_material() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let mut config = test_config(root, config_path.clone(), database_path);
        config.orchestrator.google_oauth = Some(GoogleOAuthConfig {
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            ..GoogleOAuthConfig::default()
        });

        let report =
            create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
                .expect("snapshot succeeds");

        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "OAuth material present in config")
        );
    }

    #[test]
    fn snapshot_rejects_empty_oauth_material_when_configured() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let mut config = test_config(root, config_path.clone(), database_path);
        config.orchestrator.google_oauth = Some(GoogleOAuthConfig {
            client_id: "client-id".to_string(),
            client_secret: "   ".to_string(),
            ..GoogleOAuthConfig::default()
        });

        let error =
            create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
                .expect_err("empty OAuth secret should fail");

        assert!(
            error
                .to_string()
                .contains("OAuth client_secret must be non-empty when google_oauth is configured")
        );
    }

    #[test]
    fn snapshot_writes_effective_restore_environment() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let mut config = test_config(root, config_path.clone(), database_path);
        config.runtime.base_url = Some("http://127.0.0.1:9001".to_string());
        config.runtime.public_url = Some("https://office.example.test".to_string());
        config.erv.ip = "192.0.2.10".to_string();
        config.erv.device_id = "erv-device".to_string();
        config.erv.local_key = "local'key".to_string();
        config.mitsubishi.username = Some("kumo@example.test".to_string());
        config.mitsubishi.password = Some("kumo-password".to_string());
        config.mitsubishi.device_serial = Some("serial-1".to_string());
        config.yolink.uaid = "uaid".to_string();
        config.yolink.secret_key = "secret-key".to_string();
        config.presence.enabled = true;

        let report =
            create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
                .expect("snapshot succeeds");
        let restore_env_path = report.snapshot_dir.join("restore-env.sh");
        let contents = fs::read_to_string(&restore_env_path).expect("restore env");

        assert!(contents.contains("export OFFICE_AUTOMATE_CONFIG=\"${SNAPSHOT_DIR}/config.yaml\""));
        assert!(contents.contains("export OFFICE_AUTOMATE_DATA_DIR=\"${SNAPSHOT_DIR}\""));
        assert!(
            contents
                .contains("export OFFICE_AUTOMATE_TELEMETRY_DB=\"${SNAPSHOT_DIR}/telemetry.db\"")
        );
        assert!(contents.contains(
            "export OFFICE_AUTOMATE_SESSION_TOOL_USAGE_DB=\"${SNAPSHOT_DIR}/session_tool_usage.db\""
        ));
        assert!(contents.contains("export OFFICE_AUTOMATE_ERV_LOCAL_KEY='local'\"'\"'key'"));
        assert!(contents.contains("export OFFICE_AUTOMATE_KUMO_PASSWORD='kumo-password'"));
        assert!(contents.contains("export OFFICE_AUTOMATE_YOLINK_SECRET_KEY='secret-key'"));
        assert!(report.validations.iter().any(
            |validation| validation == "effective restore environment written: restore-env.sh"
        ));

        #[cfg(unix)]
        {
            let mode = fs::metadata(&restore_env_path)
                .expect("restore env metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn snapshot_copies_distinct_session_tool_usage_database() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let mut config = test_config(root, config_path.clone(), database_path);
        create_sqlite(&config.runtime.tool_usage_db_path);
        create_sqlite(&config.runtime.session_tool_usage_db_path);

        let report =
            create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
                .expect("snapshot succeeds");

        assert!(report.snapshot_dir.join("tool_usage.db").is_file());
        assert!(report.snapshot_dir.join("session_tool_usage.db").is_file());
        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "session tool usage database quick_check ok")
        );

        config.runtime.session_tool_usage_db_path = config.runtime.tool_usage_db_path.clone();
        let shared_report = create_pre_cutover_snapshot(
            &config,
            &config_path,
            &root.join("shared-snapshots"),
            None,
        )
        .expect("shared snapshot succeeds");
        assert!(shared_report.validations.iter().any(|validation| validation
            == "session tool usage database shares project tool usage database snapshot"));
    }

    #[test]
    fn sqlite_backup_includes_committed_wal_rows() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let source = root.join("source.db");
        let destination = root.join("snapshot/telemetry.db");
        let source_connection = Connection::open(&source).expect("source sqlite");
        source_connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE test_data (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO test_data (id, value) VALUES (7, 'committed-wal-row');",
            )
            .expect("wal sqlite");

        backup_sqlite("source sqlite", &source, &destination).expect("sqlite backup");

        assert_eq!(read_test_value(&destination, 7), "committed-wal-row");
    }

    #[test]
    fn snapshot_rejects_invalid_artifact_metadata() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let app_dir = root.join("data/apps/office-climate");
        fs::create_dir_all(&app_dir).expect("app dir");
        fs::write(app_dir.join("latest.apk"), b"apk").expect("latest");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"not-valid","uploaded_at":"2026-06-05T00:00:00Z","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .expect("metadata");

        let config = test_config(root, config_path.clone(), database_path);
        let error =
            create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
                .expect_err("invalid metadata should fail");

        assert!(error.to_string().contains("invalid artifact hash"));
    }

    #[test]
    fn snapshot_hydrates_legacy_artifact_metadata_sha256() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let app_dir = root.join("data/apps/office-climate");
        fs::create_dir_all(&app_dir).expect("app dir");
        fs::write(app_dir.join("latest.apk"), b"apk").expect("latest");
        fs::write(app_dir.join("dd37c2d7.apk"), b"apk").expect("hashed");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"dd37c2d7","uploaded_at":"2026-06-05T00:00:00Z","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .expect("legacy metadata");

        let config = test_config(root, config_path.clone(), database_path);
        create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"), None)
            .expect("snapshot succeeds with legacy metadata");

        let hydrated = fs::read_to_string(app_dir.join("meta.json")).expect("hydrated metadata");
        assert!(hydrated.contains(
            r#""sha256":"dd37c2d7274f7ea982cb83390c36918fee9ce8889073c44b68cdc00bdb8c3e04""#
        ));
    }

    #[test]
    fn snapshot_validates_cloudflared_config_and_credentials() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let cloudflared_dir = root.join("cloudflared");
        fs::create_dir_all(&cloudflared_dir).expect("cloudflared dir");
        fs::write(
            cloudflared_dir.join("office-tunnel.json"),
            r#"{"AccountTag":"account","TunnelID":"tunnel-id","TunnelSecret":"secret"}"#,
        )
        .expect("credentials");
        let cloudflared_config = cloudflared_dir.join("config.yml");
        fs::write(
            &cloudflared_config,
            "credentials-file: office-tunnel.json\ningress:\n  - hostname: office.example.test\n    service: http://localhost:9001\n  - service: http_status:404\n",
        )
        .expect("cloudflared config");

        let config = test_config(root, config_path.clone(), database_path);
        let report = create_pre_cutover_snapshot(
            &config,
            &config_path,
            &root.join("snapshots"),
            Some(&cloudflared_config),
        )
        .expect("snapshot succeeds");

        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation.starts_with("cloudflared credential file readable:"))
        );
        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "cloudflared ingress rules present: 2")
        );
        assert!(report.snapshot_dir.join("cloudflared/config.yml").is_file());
        assert!(
            report
                .snapshot_dir
                .join("cloudflared/office-tunnel.json")
                .is_file()
        );
        assert_eq!(
            cloudflared_credentials_file(&report.snapshot_dir.join("cloudflared/config.yml")),
            report
                .snapshot_dir
                .join("cloudflared/office-tunnel.json")
                .display()
                .to_string()
        );
    }

    #[test]
    fn snapshot_preserves_cloudflared_relative_credential_subdirectories() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let cloudflared_dir = root.join("cloudflared");
        fs::create_dir_all(cloudflared_dir.join("creds")).expect("cloudflared creds dir");
        fs::write(
            cloudflared_dir.join("creds/office-tunnel.json"),
            r#"{"AccountTag":"account","TunnelID":"tunnel-id","TunnelSecret":"secret"}"#,
        )
        .expect("credentials");
        let cloudflared_config = cloudflared_dir.join("config.yml");
        fs::write(
            &cloudflared_config,
            "credentials-file: creds/office-tunnel.json\ningress:\n  - hostname: office.example.test\n    service: http://localhost:9001\n  - service: http_status:404\n",
        )
        .expect("cloudflared config");

        let config = test_config(root, config_path.clone(), database_path);
        let report = create_pre_cutover_snapshot(
            &config,
            &config_path,
            &root.join("snapshots"),
            Some(&cloudflared_config),
        )
        .expect("snapshot succeeds");

        assert!(
            report
                .snapshot_dir
                .join("cloudflared/creds/office-tunnel.json")
                .is_file()
        );
        assert!(
            !report
                .snapshot_dir
                .join("cloudflared/office-tunnel.json")
                .exists()
        );
        assert_eq!(
            cloudflared_credentials_file(&report.snapshot_dir.join("cloudflared/config.yml")),
            report
                .snapshot_dir
                .join("cloudflared/creds/office-tunnel.json")
                .display()
                .to_string()
        );
        assert!(report.validations.iter().any(|validation| {
            validation == "cloudflared config and credential file copied: creds/office-tunnel.json"
        }));
    }

    #[test]
    fn snapshot_rewrites_absolute_cloudflared_credential_path() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let external_dir = root.join("external-cloudflared");
        fs::create_dir_all(&external_dir).expect("external cloudflared dir");
        let credentials_path = external_dir.join("office-tunnel.json");
        fs::write(
            &credentials_path,
            r#"{"AccountTag":"account","TunnelID":"tunnel-id","TunnelSecret":"secret"}"#,
        )
        .expect("credentials");
        let cloudflared_config = root.join("cloudflared/config.yml");
        fs::create_dir_all(cloudflared_config.parent().expect("cloudflared parent"))
            .expect("cloudflared dir");
        fs::write(
            &cloudflared_config,
            format!(
                "credentials-file: {}\ningress:\n  - hostname: office.example.test\n    service: http://localhost:9001\n  - service: http_status:404\n",
                credentials_path.display()
            ),
        )
        .expect("cloudflared config");

        let config = test_config(root, config_path.clone(), database_path);
        let report = create_pre_cutover_snapshot(
            &config,
            &config_path,
            &root.join("snapshots"),
            Some(&cloudflared_config),
        )
        .expect("snapshot succeeds");

        assert!(
            report
                .snapshot_dir
                .join("cloudflared/office-tunnel.json")
                .is_file()
        );
        assert_eq!(
            cloudflared_credentials_file(&report.snapshot_dir.join("cloudflared/config.yml")),
            report
                .snapshot_dir
                .join("cloudflared/office-tunnel.json")
                .display()
                .to_string()
        );
        assert!(
            fs::read_to_string(report.snapshot_dir.join("restore-env.sh"))
                .expect("restore env")
                .contains("export CLOUDFLARED_CONFIG=\"${SNAPSHOT_DIR}/cloudflared/config.yml\"")
        );
    }

    #[test]
    fn snapshot_rejects_missing_cloudflared_credentials() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path();
        let config_path = root.join("config.yaml");
        fs::write(&config_path, "orchestrator:\n  port: 9001\n").expect("config");
        let database_path = root.join("data/office_climate.db");
        fs::create_dir_all(database_path.parent().expect("db parent")).expect("data dir");
        db::migrate_database(&database_path).expect("office db");

        let cloudflared_config = root.join("cloudflared/config.yml");
        fs::create_dir_all(cloudflared_config.parent().expect("cloudflared parent"))
            .expect("cloudflared dir");
        fs::write(
            &cloudflared_config,
            "credentials-file: missing.json\ningress:\n  - service: http://localhost:9001\n",
        )
        .expect("cloudflared config");

        let config = test_config(root, config_path.clone(), database_path);
        let error = create_pre_cutover_snapshot(
            &config,
            &config_path,
            &root.join("snapshots"),
            Some(&cloudflared_config),
        )
        .expect_err("missing cloudflared credentials should fail");

        assert!(
            error
                .to_string()
                .contains("cloudflared credentials file is not readable")
        );
    }
}
