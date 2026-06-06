use std::{
    fs,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::Local;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::{
    artifacts::{ArtifactMetadata, is_valid_artifact_hash},
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

pub fn create_pre_cutover_snapshot(
    config: &AppConfig,
    config_path: &Path,
    output_dir: &Path,
) -> Result<SnapshotReport> {
    ensure_readable_file("config", config_path)?;
    ensure_readable_file("office database", &config.runtime.database_path)?;
    ensure_writable_directory(output_dir)?;

    let snapshot_dir = unique_snapshot_dir(output_dir);
    fs::create_dir(&snapshot_dir)
        .with_context(|| format!("failed to create {}", snapshot_dir.display()))?;

    let mut files_copied = 0_usize;
    let mut validations = Vec::new();

    copy_file(config_path, &snapshot_dir.join("config.yaml"))?;
    files_copied += 1;
    validations.push("config readable".to_string());
    validations.extend(validate_config_material(config));

    let office_db_snapshot = snapshot_dir.join("office_climate.db");
    copy_file(&config.runtime.database_path, &office_db_snapshot)?;
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
        "legacy APK",
        &config.runtime.legacy_apk_path,
        &snapshot_dir.join("legacy").join("office-climate.apk"),
        &mut validations,
    )?;

    if config.runtime.artifacts_dir.exists() {
        validate_artifact_metadata(&config.runtime.artifacts_dir, &mut validations)?;
        files_copied += copy_dir_recursive(
            &config.runtime.artifacts_dir,
            &snapshot_dir.join("artifacts"),
        )?;
    } else {
        validations.push(format!(
            "optional artifacts directory missing: {}",
            config.runtime.artifacts_dir.display()
        ));
    }

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

fn unique_snapshot_dir(output_dir: &Path) -> PathBuf {
    output_dir.join(format!(
        "office-automate-precutover-{}",
        Local::now().format("%Y%m%d-%H%M%S")
    ))
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

fn validate_config_material(config: &AppConfig) -> Vec<String> {
    let mut validations = Vec::new();
    if config.orchestrator.google_oauth.is_some() {
        validations.push("OAuth material present in config".to_string());
    } else {
        validations.push("OAuth material absent from config".to_string());
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
    validations
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
    copy_file(source, destination)?;
    quick_check_sqlite(label, destination)?;
    validations.push(format!("{label} quick_check ok"));
    Ok(1)
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
        let metadata: ArtifactMetadata = serde_json::from_str(&contents)
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
        ErvConfig, MitsubishiConfig, OrchestratorConfig, PresenceConfig, QingpingConfig,
        RuntimeConfig, TelemetryConfig, ThresholdsConfig, YoLinkConfig,
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
                tool_usage_db_path: root.join("data/tool_usage.db"),
                engram_db_path: root.join("data/engram_state.db"),
                engram_registry_path: root.join("data/engram_concept_registry.md"),
            },
        }
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
        fs::write(app_dir.join("1a2b3c4d.apk"), b"apk").expect("hashed");
        fs::write(
            app_dir.join("meta.json"),
            r#"{"artifact_hash":"1a2b3c4d","uploaded_at":"2026-06-05T00:00:00Z","size_bytes":3,"uploaded_by":"test@example.com"}"#,
        )
        .expect("metadata");

        let config = test_config(root, config_path.clone(), database_path);
        let output_dir = root.join("snapshots");
        let report = create_pre_cutover_snapshot(&config, &config_path, &output_dir)
            .expect("snapshot succeeds");

        assert!(report.snapshot_dir.join("config.yaml").is_file());
        assert!(report.snapshot_dir.join("office_climate.db").is_file());
        assert!(
            report
                .snapshot_dir
                .join("artifacts/office-climate/meta.json")
                .is_file()
        );
        assert!(report.snapshot_dir.join("manifest.json").is_file());
        assert!(report.files_copied >= 5);
        assert!(
            report
                .validations
                .iter()
                .any(|validation| validation == "office database migrated on snapshot copy")
        );
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
        let error = create_pre_cutover_snapshot(&config, &config_path, &root.join("snapshots"))
            .expect_err("invalid metadata should fail");

        assert!(error.to_string().contains("invalid artifact hash"));
    }
}
