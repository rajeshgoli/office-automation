use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose};
use chrono::{Local, NaiveDateTime, TimeZone};
use futures_util::{SinkExt, StreamExt};
use ipnet::IpNet;
use reqwest::{StatusCode, Url};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_yaml::Value as YamlValue;
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Error as TungsteniteError, Message as TungsteniteMessage,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};

use crate::{
    artifacts::{is_valid_artifact_hash, is_valid_sha256_digest},
    auth::AuthManager,
    config::AppConfig,
    db, erv, http, hvac,
    state::StateMachine,
    yolink::{self, YoLinkCloudClient, YoLinkState},
};

const INTERFACE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowValidationOptions {
    pub base_url: Option<String>,
    pub public_url: Option<String>,
    pub cloudflared_config: Option<PathBuf>,
    pub cloudflare_evidence: Option<PathBuf>,
    pub cloudflare_access_client_id: Option<String>,
    pub cloudflare_access_client_secret: Option<String>,
    pub manual_public_access_verified_at: Option<String>,
    pub skip_live_devices: bool,
    pub skip_http_interface: bool,
    pub max_air_quality_age_seconds: u64,
}

impl Default for ShadowValidationOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            public_url: None,
            cloudflared_config: None,
            cloudflare_evidence: None,
            cloudflare_access_client_id: None,
            cloudflare_access_client_secret: None,
            manual_public_access_verified_at: None,
            skip_live_devices: false,
            skip_http_interface: false,
            max_air_quality_age_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutoverValidationOptions {
    pub base_url: Option<String>,
    pub public_url: Option<String>,
    pub legacy_base_url: Option<String>,
    pub legacy_controller_stopped_at: String,
    pub mqtt_strategy: MqttCutoverStrategy,
    pub snapshot_dir: PathBuf,
    pub cutover_log: PathBuf,
    pub manual_public_oauth_verified_at: Option<String>,
    pub cloudflared_config: Option<PathBuf>,
    pub cloudflare_evidence: Option<PathBuf>,
    pub cloudflare_access_client_id: Option<String>,
    pub cloudflare_access_client_secret: Option<String>,
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackValidationOptions {
    pub legacy_base_url: Option<String>,
    pub legacy_public_url: Option<String>,
    pub rust_base_url: Option<String>,
    pub rust_public_url: Option<String>,
    pub rust_stopped_at: String,
    pub legacy_started_at: String,
    pub mqtt_rollback_state: MqttRollbackState,
    pub snapshot_dir: PathBuf,
    pub restore_verification: RestoreVerification,
    pub rollback_log: PathBuf,
    pub manual_legacy_public_verified_at: Option<String>,
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttCutoverStrategy {
    AtomicSwitch,
}

impl MqttCutoverStrategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::AtomicSwitch => "atomic-switch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttRollbackState {
    NotMoved,
    RepointedLegacy,
}

impl MqttRollbackState {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotMoved => "not-moved",
            Self::RepointedLegacy => "repointed-legacy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreVerification {
    RestoredFromSnapshot,
    VerifiedSafeNoRestore,
}

impl RestoreVerification {
    fn as_str(self) -> &'static str {
        match self {
            Self::RestoredFromSnapshot => "restored-from-snapshot",
            Self::VerifiedSafeNoRestore => "verified-safe-no-restore",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ShadowValidationReport {
    pub checks: Vec<ValidationCheck>,
}

impl ShadowValidationReport {
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationCheck {
    pub name: String,
    pub status: ValidationStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ValidationStatus {
    Passed,
    Skipped,
}

impl ValidationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Skipped => "skipped",
        }
    }
}

impl ShadowValidationReport {
    fn push_pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ValidationCheck {
            name: name.into(),
            status: ValidationStatus::Passed,
            detail: detail.into(),
        });
    }

    fn push_skip(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(ValidationCheck {
            name: name.into(),
            status: ValidationStatus::Skipped,
            detail: detail.into(),
        });
    }
}

pub async fn run_shadow_validation(
    config: &AppConfig,
    options: ShadowValidationOptions,
) -> Result<ShadowValidationReport> {
    let mut report = ShadowValidationReport { checks: Vec::new() };

    validate_http_startup_config(config, options.public_url.as_deref(), &mut report)?;
    validate_active_write_gates(config, &mut report)?;
    validate_database_inputs(config, &mut report)?;

    if options.skip_live_devices {
        report.push_skip(
            "live-device-read-checks",
            "skipped by --skip-live-devices; do not use this for final cutover validation",
        );
    } else {
        validate_live_devices(config, &mut report).await?;
    }

    if options.skip_http_interface {
        report.push_skip(
            "http-interface-parity",
            "skipped by --skip-http-interface; do not use this for final cutover validation",
        );
    } else {
        validate_http_interfaces(config, &options, &mut report).await?;
    }

    Ok(report)
}

pub async fn run_cutover_validation(
    config: &AppConfig,
    options: CutoverValidationOptions,
) -> Result<ShadowValidationReport> {
    let mut report = ShadowValidationReport { checks: Vec::new() };

    validate_http_startup_config(config, options.public_url.as_deref(), &mut report)?;
    validate_cutover_active_write_gates(config, &mut report)?;
    validate_cutover_snapshot(&options.snapshot_dir, &mut report)?;
    validate_legacy_controller_stopped(&options, &mut report).await?;
    validate_mqtt_cutover_strategy(config, options.mqtt_strategy, &mut report)?;
    validate_live_devices(config, &mut report).await?;
    validate_cutover_http_interfaces(config, &options, &mut report).await?;
    write_cutover_log(config, &options, &report)?;

    Ok(report)
}

pub async fn run_rollback_validation(
    config: &AppConfig,
    options: RollbackValidationOptions,
) -> Result<ShadowValidationReport> {
    let mut report = ShadowValidationReport { checks: Vec::new() };

    validate_rollback_active_write_gates(config, &mut report)?;
    validate_cutover_snapshot(&options.snapshot_dir, &mut report)?;
    validate_rust_controller_stopped(&options, &mut report).await?;
    validate_mqtt_rollback_state(config, options.mqtt_rollback_state, &mut report)?;
    validate_restore_verification(options.restore_verification, &mut report)?;
    validate_legacy_recovered_http_interfaces(config, &options, &mut report).await?;
    write_rollback_log(config, &options, &report)?;

    Ok(report)
}

fn validate_active_write_gates(
    config: &AppConfig,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if config.erv.active_control_enabled {
        bail!("shadow validation requires ERV active control disabled");
    }
    if config.mitsubishi.active_control_enabled {
        bail!("shadow validation requires HVAC active control disabled");
    }

    report.push_pass(
        "active-write-gates",
        "ERV and HVAC active-control flags are disabled",
    );
    Ok(())
}

fn validate_http_startup_config(
    config: &AppConfig,
    public_url: Option<&str>,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let effective_public_url = public_url.or(config.runtime.public_url.as_deref());
    http::validate_http_startup_config_for_public_url(config, effective_public_url)?;
    report.push_pass(
        "http-startup-config",
        "HTTP listener startup config is safe for configured public/local exposure",
    );
    Ok(())
}

fn validate_rollback_active_write_gates(
    config: &AppConfig,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if config.erv.active_control_enabled {
        bail!("rollback validation requires ERV active control disabled in Rust config");
    }
    if config.mitsubishi.active_control_enabled {
        bail!("rollback validation requires HVAC active control disabled in Rust config");
    }

    report.push_pass(
        "rust-active-write-gates-disabled",
        "ERV and HVAC active-control flags are disabled before any Rust shadow follow-up",
    );
    Ok(())
}

fn validate_cutover_active_write_gates(
    config: &AppConfig,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if !config.erv.active_control_enabled {
        bail!("cutover validation requires ERV active control enabled in Rust config");
    }
    if !config.mitsubishi.active_control_enabled {
        bail!("cutover validation requires HVAC active control enabled in Rust config");
    }

    report.push_pass(
        "rust-active-write-gates",
        "ERV and HVAC active-control flags are enabled for Rust",
    );
    Ok(())
}

fn validate_cutover_snapshot(
    snapshot_dir: &Path,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if !snapshot_dir.is_dir() {
        bail!(
            "cutover snapshot directory is missing: {}",
            snapshot_dir.display()
        );
    }
    let manifest = snapshot_dir.join("manifest.json");
    if !manifest.is_file() {
        bail!(
            "cutover snapshot manifest is missing: {}",
            manifest.display()
        );
    }
    report.push_pass(
        "rollback-snapshot",
        format!(
            "pre-cutover rollback snapshot manifest present at {}",
            manifest.display()
        ),
    );
    Ok(())
}

async fn validate_legacy_controller_stopped(
    options: &CutoverValidationOptions,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if options.legacy_controller_stopped_at.trim().is_empty() {
        bail!(
            "cutover requires --legacy-controller-stopped-at before Rust active control remains enabled"
        );
    }

    if let Some(legacy_base_url) = options.legacy_base_url.as_deref() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .context("failed to create legacy-controller probe client")?;
        let url = join_url(legacy_base_url, "/status")?;
        match client.get(url).send().await {
            Ok(response) => {
                bail!(
                    "legacy controller URL still responded with {}; do not run two active controllers",
                    response.status()
                );
            }
            Err(error) => report.push_pass(
                "legacy-controller-disabled",
                format!(
                    "operator recorded stopped_at={}; legacy /status did not respond: {}",
                    options.legacy_controller_stopped_at, error
                ),
            ),
        }
    } else {
        report.push_pass(
            "legacy-controller-disabled",
            format!(
                "operator recorded stopped_at={}; no legacy URL probe supplied",
                options.legacy_controller_stopped_at
            ),
        );
    }

    Ok(())
}

fn validate_mqtt_cutover_strategy(
    config: &AppConfig,
    _strategy: MqttCutoverStrategy,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let device_mac = config
        .qingping
        .device_mac
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context(
            "cutover validation requires qingping.device_mac so the active feed can be audited",
        )?;

    let detail = "atomic switch strategy recorded; Qingping feed moves in the same window as active-controller cutover";
    report.push_pass(
        "mqtt-feed-strategy",
        format!(
            "{detail}; rust_broker={}:{} device_mac={device_mac}",
            config.runtime.mqtt_host, config.runtime.mqtt_port
        ),
    );
    Ok(())
}

fn validate_database_inputs(config: &AppConfig, report: &mut ShadowValidationReport) -> Result<()> {
    validate_sqlite_quick_check(&config.runtime.database_path, "office climate database")?;
    let history = db::read_history(&config.runtime.database_path, 24, 10)
        .context("failed to read compatibility history rows from office climate database")?;
    report.push_pass(
        "office-climate-db",
        format!(
            "quick_check ok; readable history rows: sensors={} occupancy={} devices={} climate={}",
            history.sensor_readings.len(),
            history.occupancy_history.len(),
            history.device_events.len(),
            history.climate_actions.len()
        ),
    );

    validate_optional_sqlite(
        &config.runtime.telemetry_db_path,
        "telemetry-db",
        "telemetry database",
        report,
    )?;
    validate_optional_sqlite(
        &config.runtime.tool_usage_db_path,
        "tool-usage-db",
        "tool usage database",
        report,
    )?;
    validate_optional_sqlite(
        &config.runtime.engram_db_path,
        "engram-db",
        "Engram database",
        report,
    )?;

    Ok(())
}

fn validate_optional_sqlite(
    path: &Path,
    name: &str,
    label: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if !path.exists() {
        report.push_skip(name, format!("{label} not present at {}", path.display()));
        return Ok(());
    }

    validate_sqlite_quick_check(path, label)?;
    report.push_pass(name, format!("quick_check ok at {}", path.display()));
    Ok(())
}

fn validate_sqlite_quick_check(path: &Path, label: &str) -> Result<()> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open {label} at {}", path.display()))?;
    let quick_check: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .with_context(|| format!("failed to run quick_check for {label}"))?;
    if quick_check != "ok" {
        bail!("{label} quick_check failed: {quick_check}");
    }
    Ok(())
}

async fn validate_live_devices(
    config: &AppConfig,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if !config.erv.is_configured() {
        bail!("shadow validation requires configured ERV read credentials");
    }
    let erv_status = erv::smoke_erv(config)
        .await
        .context("ERV read-only smoke check failed")?;
    report.push_pass(
        "erv-read",
        format!(
            "read local status: running={} speed={}",
            erv_status.power,
            erv_status
                .fan_speed
                .map(|speed| speed.as_str())
                .unwrap_or("unknown")
        ),
    );

    if !config.mitsubishi.is_configured() {
        bail!("shadow validation requires configured HVAC read credentials");
    }
    let hvac_status = hvac::smoke_hvac(config)
        .await
        .context("HVAC read-only smoke check failed")?;
    report.push_pass(
        "hvac-read",
        format!(
            "read Kumo status: mode={} setpoint_c={:.1}",
            hvac_status.mode, hvac_status.setpoint_c
        ),
    );

    if !config.yolink.is_configured() {
        bail!("shadow validation requires configured YoLink API credentials");
    }
    let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
        &config.thresholds,
        current_timestamp_seconds(),
    )));
    let yolink_state = YoLinkState::new(state_machine, config.runtime.database_path.clone());
    let client = YoLinkCloudClient::new(config.yolink.clone());
    let (_, home_id) = yolink::initialize_yolink_inventory(&client, &yolink_state)
        .await
        .context("YoLink read-only inventory check failed")?;
    let (door, window, motion) = yolink_state.classified_device_ids();
    report.push_pass(
        "yolink-read",
        format!(
            "home_id={home_id}; classified door={} window={} motion={}",
            present_label(&door),
            present_label(&window),
            present_label(&motion)
        ),
    );

    Ok(())
}

async fn validate_http_interfaces(
    config: &AppConfig,
    options: &ShadowValidationOptions,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let base_url = options
        .base_url
        .as_deref()
        .or(config.runtime.base_url.as_deref())
        .context("shadow validation requires --base-url or OFFICE_AUTOMATE_BASE_URL")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to create validation HTTP client")?;

    let auth = interface_probe_auth(config)?;

    let status = get_json(&client, base_url, "/status", Some(&auth)).await?;
    validate_status_shape(&status)?;
    validate_fresh_air_quality(&status, options.max_air_quality_age_seconds)?;
    report.push_pass(
        "status-interface",
        format!("/status returned compatible shape from {base_url}"),
    );

    let history = get_json(&client, base_url, "/history?hours=1&limit=5", Some(&auth)).await?;
    for key in [
        "sensor_readings",
        "occupancy_history",
        "device_events",
        "climate_actions",
    ] {
        if !history.get(key).is_some_and(Value::is_array) {
            bail!("/history response missing array field {key}");
        }
    }
    report.push_pass(
        "history-interface",
        "/history returned compatibility arrays",
    );

    let leverage = get_json(
        &client,
        base_url,
        "/history/project-leverage?days=7",
        Some(&auth),
    )
    .await?;
    if !leverage.get("projects").is_some_and(Value::is_object) {
        bail!("/history/project-leverage response missing projects object");
    }
    report.push_pass(
        "project-leverage-interface",
        "/history/project-leverage returned projects object",
    );

    validate_artifact_interface(&client, base_url, report).await?;
    validate_auth_interface(config, &client, base_url, &auth, report).await?;

    let public_url = options
        .public_url
        .as_deref()
        .or(config.runtime.public_url.as_deref());
    if let Some(public_url) = public_url {
        validate_cloudflared_public_config_optional(
            options.cloudflared_config.as_deref(),
            public_url,
            report,
        )?;
        validate_cloudflare_drift_evidence_optional(
            options.cloudflare_evidence.as_deref(),
            public_url,
            report,
        )?;
        validate_public_access_blocks_unauthenticated(public_url, report).await?;
        let access_auth = public_access_probe_auth(
            options.cloudflare_access_client_id.as_deref(),
            options.cloudflare_access_client_secret.as_deref(),
        )?;
        if access_auth.is_some() && auth.supports_public_http_auth() {
            let public_status = get_public_json(
                &client,
                public_url,
                "/status",
                Some(&auth),
                access_auth.as_ref(),
            )
            .await?;
            validate_status_shape(&public_status)?;
            report.push_pass(
                "cloudflare-public-status",
                format!(
                    "public URL returned authenticated /status through Cloudflare Access and Office auth: {public_url}"
                ),
            );
        } else if let Some(verified_at) = options.manual_public_access_verified_at.as_deref() {
            validate_manual_verification_timestamp(
                verified_at,
                "manual public Access verification",
            )?;
            report.push_pass(
                "cloudflare-public-status",
                format!(
                    "manual authenticated Cloudflare Access plus Office auth verification recorded at {verified_at}"
                ),
            );
        } else {
            report.push_skip(
                "cloudflare-public-status",
                "no Cloudflare Access service token or manual verification timestamp supplied; authenticated public /status was not probed",
            );
        }
    } else {
        report.push_skip(
            "cloudflare-public-status",
            "no --public-url or OFFICE_AUTOMATE_PUBLIC_URL supplied",
        );
    }

    validate_websocket_interface(base_url, &auth, report).await?;

    Ok(())
}

async fn validate_cutover_http_interfaces(
    config: &AppConfig,
    options: &CutoverValidationOptions,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let base_url = options
        .base_url
        .as_deref()
        .or(config.runtime.base_url.as_deref())
        .context("cutover validation requires --base-url or OFFICE_AUTOMATE_BASE_URL")?;
    let public_url = options
        .public_url
        .as_deref()
        .or(config.runtime.public_url.as_deref())
        .context("cutover validation requires --public-url or OFFICE_AUTOMATE_PUBLIC_URL")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to create cutover validation HTTP client")?;
    let auth = interface_probe_auth(config)?;

    let status = get_json(&client, base_url, "/status", Some(&auth)).await?;
    validate_status_shape(&status)?;
    validate_fresh_air_quality(&status, options.max_air_quality_age_seconds)?;
    report.push_pass(
        "rust-status-fresh",
        format!("/status returned compatible shape with fresh air-quality reading from {base_url}"),
    );

    validate_websocket_interface(base_url, &auth, report).await?;
    validate_cloudflared_public_config_required(
        options.cloudflared_config.as_deref(),
        public_url,
        report,
    )?;
    validate_cloudflare_drift_evidence_required(
        options.cloudflare_evidence.as_deref(),
        public_url,
        report,
    )?;
    validate_public_access_blocks_unauthenticated(public_url, report).await?;

    let access_auth = public_access_probe_auth(
        options.cloudflare_access_client_id.as_deref(),
        options.cloudflare_access_client_secret.as_deref(),
    )?;
    if auth.supports_public_http_auth() {
        let Some(access_auth) = access_auth.as_ref() else {
            if let Some(verified_at) = options.manual_public_oauth_verified_at.as_deref() {
                validate_manual_verification_timestamp(
                    verified_at,
                    "manual authenticated public Access verification",
                )?;
                report.push_pass(
                    "cloudflare-public-status",
                    format!(
                        "manual authenticated Cloudflare Access plus Office auth verification recorded at {verified_at}"
                    ),
                );
                return Ok(());
            }
            bail!(
                "cutover validation requires Cloudflare Access service-token headers or --manual-public-oauth-verified-at after browser/mobile Access plus Office auth verification"
            );
        };
        let public_status = get_public_json(
            &client,
            public_url,
            "/status",
            Some(&auth),
            Some(access_auth),
        )
        .await?;
        validate_status_shape(&public_status)?;
        validate_fresh_air_quality(&public_status, options.max_air_quality_age_seconds)?;
        report.push_pass(
            "cloudflare-public-status",
            format!(
                "public URL returned authenticated fresh /status through Cloudflare Access and Office auth: {public_url}"
            ),
        );
    } else if let Some(verified_at) = options.manual_public_oauth_verified_at.as_deref() {
        validate_manual_verification_timestamp(
            verified_at,
            "manual authenticated public Access verification",
        )?;
        report.push_pass(
            "cloudflare-public-status",
            format!(
                "manual authenticated Cloudflare Access plus Office auth verification recorded at {verified_at}; validation token unavailable"
            ),
        );
    } else {
        bail!(
            "cutover validation requires Cloudflare Access service-token headers or --manual-public-oauth-verified-at after browser/mobile Access plus Office auth verification"
        );
    }

    Ok(())
}

async fn validate_rust_controller_stopped(
    options: &RollbackValidationOptions,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if options.rust_stopped_at.trim().is_empty() {
        bail!("rollback requires --rust-stopped-at after stopping office-automate-server");
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .context("failed to create Rust-controller rollback probe client")?;
    let mut probed = false;

    if let Some(rust_base_url) = options.rust_base_url.as_deref() {
        validate_endpoint_absent(&client, rust_base_url, "local Rust /status").await?;
        report.push_pass(
            "rust-local-controller-stopped",
            format!(
                "operator recorded stopped_at={}; local Rust /status did not respond at {}",
                options.rust_stopped_at, rust_base_url
            ),
        );
        probed = true;
    }

    if let Some(rust_public_url) = options.rust_public_url.as_deref() {
        validate_endpoint_absent(&client, rust_public_url, "public Rust /status").await?;
        report.push_pass(
            "rust-public-tunnel-stopped",
            format!(
                "operator recorded stopped_at={}; public Rust /status did not respond at {}",
                options.rust_stopped_at, rust_public_url
            ),
        );
        probed = true;
    }

    if !probed {
        report.push_pass(
            "rust-controller-stopped",
            format!(
                "operator recorded stopped_at={}; no Rust URL probe supplied",
                options.rust_stopped_at
            ),
        );
    }

    Ok(())
}

async fn validate_endpoint_absent(
    client: &reqwest::Client,
    base_url: &str,
    label: &str,
) -> Result<()> {
    let url = join_url(base_url, "/status")?;
    match client.get(url).send().await {
        Ok(response) => {
            bail!(
                "{label} still responded with {}; rollback must not leave Rust active",
                response.status()
            );
        }
        Err(_) => Ok(()),
    }
}

fn validate_mqtt_rollback_state(
    config: &AppConfig,
    state: MqttRollbackState,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let device_mac = config
        .qingping
        .device_mac
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context(
            "rollback validation requires qingping.device_mac so the feed rollback can be audited",
        )?;

    let detail = match state {
        MqttRollbackState::NotMoved => "Qingping feed never moved off the legacy-compatible path",
        MqttRollbackState::RepointedLegacy => "Qingping device was repointed to the legacy broker",
    };
    report.push_pass(
        "mqtt-feed-rollback",
        format!("{detail}; device_mac={device_mac}"),
    );
    Ok(())
}

fn validate_restore_verification(
    restore: RestoreVerification,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let detail = match restore {
        RestoreVerification::RestoredFromSnapshot => {
            "operator restored copied state from the pre-cutover snapshot"
        }
        RestoreVerification::VerifiedSafeNoRestore => {
            "operator verified Rust-written state is legacy-compatible; no restore required"
        }
    };
    report.push_pass("snapshot-restore-verification", detail);
    Ok(())
}

async fn validate_legacy_recovered_http_interfaces(
    config: &AppConfig,
    options: &RollbackValidationOptions,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let legacy_base_url = options
        .legacy_base_url
        .as_deref()
        .context("rollback validation requires --legacy-base-url")?;
    if options.legacy_started_at.trim().is_empty() {
        bail!("rollback requires --legacy-started-at after starting the legacy backend/tunnel");
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to create rollback validation HTTP client")?;
    let auth = interface_probe_auth(config)?;

    let status = get_json(&client, legacy_base_url, "/status", Some(&auth)).await?;
    validate_status_shape(&status)?;
    validate_fresh_air_quality(&status, options.max_air_quality_age_seconds)?;
    validate_legacy_climate_safety(&status)?;
    report.push_pass(
        "legacy-status-fresh",
        format!(
            "legacy /status recovered at {}; started_at={}",
            legacy_base_url, options.legacy_started_at
        ),
    );

    validate_websocket_interface(legacy_base_url, &auth, report).await?;

    if let Some(legacy_public_url) = options.legacy_public_url.as_deref() {
        validate_public_oauth_login(config, &client, legacy_public_url, report).await?;
        if auth.supports_public_http_auth() {
            let public_status =
                get_json(&client, legacy_public_url, "/status", Some(&auth)).await?;
            validate_status_shape(&public_status)?;
            validate_fresh_air_quality(&public_status, options.max_air_quality_age_seconds)?;
            validate_legacy_climate_safety(&public_status)?;
            report.push_pass(
                "legacy-cloudflare-public-status",
                format!("legacy public URL returned fresh /status through Cloudflare Tunnel: {legacy_public_url}"),
            );
        } else if let Some(verified_at) = options.manual_legacy_public_verified_at.as_deref() {
            if verified_at.trim().is_empty() {
                bail!("manual legacy public verification timestamp cannot be empty");
            }
            report.push_pass(
                "legacy-cloudflare-public-status",
                format!(
                    "manual browser/mobile legacy public verification recorded at {verified_at}; validation token unavailable"
                ),
            );
        } else {
            bail!(
                "OAuth config has no jwt_secret, so protected legacy public /status cannot be authenticated non-interactively; supply --manual-legacy-public-verified-at after browser/mobile verification"
            );
        }
    } else {
        report.push_skip(
            "legacy-cloudflare-public-status",
            "no --legacy-public-url or OFFICE_AUTOMATE_LEGACY_PUBLIC_URL supplied",
        );
    }

    Ok(())
}

fn validate_legacy_climate_safety(status: &Value) -> Result<()> {
    if !status
        .get("safety_interlock")
        .is_some_and(Value::is_boolean)
    {
        bail!("legacy /status safety_interlock is missing or not boolean");
    }
    let local_key_invalid = status
        .get("erv")
        .and_then(|erv| erv.get("control"))
        .and_then(|control| control.get("local_key_invalid"))
        .and_then(Value::as_bool)
        .context("legacy /status missing erv.control.local_key_invalid")?;
    if local_key_invalid {
        bail!("legacy ERV local key is invalid after rollback");
    }
    Ok(())
}

async fn validate_public_oauth_login(
    config: &AppConfig,
    client: &reqwest::Client,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if config.orchestrator.google_oauth.is_none() {
        bail!(
            "rollback validation requires Google OAuth config for the legacy Cloudflare public URL"
        );
    }

    let url = join_url(public_url, "/auth/login")?;
    let response = client
        .get(url)
        .send()
        .await
        .context("failed to call legacy public OAuth login endpoint")?;
    if response.status() != StatusCode::OK {
        bail!(
            "legacy public OAuth login endpoint returned {}",
            response.status()
        );
    }
    let payload: Value = response
        .json()
        .await
        .context("failed to parse legacy public OAuth login payload")?;
    if !payload
        .get("authorization_url")
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with("https://"))
        || !payload.get("state").and_then(Value::as_str).is_some()
    {
        bail!("legacy public OAuth login payload missing authorization_url or state");
    }
    report.push_pass(
        "legacy-cloudflare-oauth-login",
        format!("legacy /auth/login returned OAuth start payload through {public_url}"),
    );
    Ok(())
}

fn validate_cloudflared_public_config_optional(
    cloudflared_config: Option<&Path>,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if let Some(cloudflared_config) = cloudflared_config {
        validate_cloudflared_public_config(cloudflared_config, public_url, report)
    } else {
        report.push_skip(
            "cloudflare-tunnel-config",
            "no --cloudflared-config supplied; tunnel ingress shape was not validated",
        );
        Ok(())
    }
}

fn validate_cloudflared_public_config_required(
    cloudflared_config: Option<&Path>,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let cloudflared_config = cloudflared_config
        .context("cutover validation requires --cloudflared-config or CLOUDFLARED_CONFIG")?;
    validate_cloudflared_public_config(cloudflared_config, public_url, report)
}

#[derive(Debug, Deserialize)]
struct CloudflareDriftEvidence {
    source: Option<String>,
    captured_at: Option<String>,
    hostname: Option<String>,
    access_application: Option<CloudflareAccessApplicationEvidence>,
    dns: Option<CloudflareDnsEvidence>,
    tunnel: Option<CloudflareTunnelEvidence>,
    access_audit: Option<CloudflareAccessAuditEvidence>,
}

#[derive(Debug, Deserialize)]
struct CloudflareAccessApplicationEvidence {
    hostname: Option<String>,
    require_access: Option<bool>,
    policies: Option<Vec<CloudflareAccessPolicyEvidence>>,
}

#[derive(Debug, Deserialize)]
struct CloudflareAccessPolicyEvidence {
    name: Option<String>,
    action: Option<String>,
    includes_public: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CloudflareDnsEvidence {
    wildcard_records: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CloudflareTunnelEvidence {
    hostname: Option<String>,
    origin_service: Option<String>,
    private_network_routes: Option<Vec<String>>,
    final_ingress_service: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CloudflareAccessAuditEvidence {
    checked_at: Option<String>,
    unauthenticated_blocks_seen: Option<bool>,
    authenticated_success_seen: Option<bool>,
}

fn validate_cloudflare_drift_evidence_optional(
    cloudflare_evidence: Option<&Path>,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    if let Some(cloudflare_evidence) = cloudflare_evidence {
        validate_cloudflare_drift_evidence(cloudflare_evidence, public_url, report)
    } else {
        report.push_skip(
            "cloudflare-drift-evidence",
            "no --cloudflare-evidence supplied; Cloudflare account/API state was not validated",
        );
        Ok(())
    }
}

fn validate_cloudflare_drift_evidence_required(
    cloudflare_evidence: Option<&Path>,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let cloudflare_evidence = cloudflare_evidence.context(
        "cutover validation requires --cloudflare-evidence or OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE",
    )?;
    validate_cloudflare_drift_evidence(cloudflare_evidence, public_url, report)
}

fn validate_cloudflare_drift_evidence(
    cloudflare_evidence: &Path,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let public_host = cloudflare_public_host(public_url)?;
    let contents = fs::read_to_string(cloudflare_evidence).with_context(|| {
        format!(
            "failed to read Cloudflare evidence {}",
            cloudflare_evidence.display()
        )
    })?;
    let evidence: CloudflareDriftEvidence = serde_json::from_str(&contents).with_context(|| {
        format!(
            "failed to parse Cloudflare evidence JSON {}",
            cloudflare_evidence.display()
        )
    })?;

    let source = required_evidence_str(&evidence.source, "source")?;
    match normalize_evidence_token(source).as_str() {
        "cloudflare_api"
        | "api"
        | "terraform_export"
        | "terraform"
        | "dashboard_screenshot_manifest"
        | "dashboard" => {}
        _ => bail!("unsupported Cloudflare evidence source {source:?}"),
    }
    let captured_at = required_evidence_str(&evidence.captured_at, "captured_at")?;
    validate_manual_verification_timestamp(captured_at, "Cloudflare evidence captured_at")?;
    validate_evidence_hostname(
        "hostname",
        required_evidence_str(&evidence.hostname, "hostname")?,
        &public_host,
    )?;

    let access_application = evidence
        .access_application
        .as_ref()
        .context("Cloudflare evidence missing access_application")?;
    validate_evidence_hostname(
        "access_application.hostname",
        required_evidence_str(&access_application.hostname, "access_application.hostname")?,
        &public_host,
    )?;
    if access_application.require_access != Some(true) {
        bail!("Cloudflare evidence must prove require_access=true for the Access application");
    }
    let policies = access_application
        .policies
        .as_deref()
        .context("Cloudflare evidence missing access_application.policies")?;
    if policies.is_empty() {
        bail!("Cloudflare evidence Access application must include at least one policy");
    }
    let mut has_forwarding_policy = false;
    for policy in policies {
        let name = required_evidence_str(&policy.name, "access_application.policies[].name")?;
        let action = required_evidence_str(&policy.action, "access_application.policies[].action")?;
        let normalized_action = normalize_evidence_token(action);
        if normalized_action == "bypass" {
            bail!("Cloudflare evidence policy {name:?} uses Bypass, which is not allowed");
        }
        if policy.includes_public != Some(false) {
            bail!(
                "Cloudflare evidence policy {name:?} must prove includes_public=false; public allow rules are not allowed"
            );
        }
        if matches!(normalized_action.as_str(), "allow" | "service_auth") {
            has_forwarding_policy = true;
        }
    }
    if !has_forwarding_policy {
        bail!("Cloudflare evidence must include at least one Allow or Service Auth policy");
    }

    let dns = evidence
        .dns
        .as_ref()
        .context("Cloudflare evidence missing dns")?;
    let wildcard_records = dns
        .wildcard_records
        .as_deref()
        .context("Cloudflare evidence missing dns.wildcard_records")?;
    if !wildcard_records.is_empty() {
        bail!(
            "Cloudflare evidence shows wildcard DNS records for Office Automate: {}",
            wildcard_records.join(", ")
        );
    }

    let tunnel = evidence
        .tunnel
        .as_ref()
        .context("Cloudflare evidence missing tunnel")?;
    validate_evidence_hostname(
        "tunnel.hostname",
        required_evidence_str(&tunnel.hostname, "tunnel.hostname")?,
        &public_host,
    )?;
    let origin_service = required_evidence_str(&tunnel.origin_service, "tunnel.origin_service")?;
    validate_cloudflared_origin_service(origin_service)
        .context("Cloudflare evidence tunnel.origin_service is unsafe")?;
    let private_network_routes = tunnel
        .private_network_routes
        .as_deref()
        .context("Cloudflare evidence missing tunnel.private_network_routes")?;
    if !private_network_routes.is_empty() {
        bail!(
            "Cloudflare evidence shows private network routes on the Office tunnel: {}",
            private_network_routes.join(", ")
        );
    }
    let final_ingress_service = required_evidence_str(
        &tunnel.final_ingress_service,
        "tunnel.final_ingress_service",
    )?;
    if final_ingress_service != "http_status:404" {
        bail!("Cloudflare evidence tunnel.final_ingress_service must be http_status:404");
    }

    let access_audit = evidence
        .access_audit
        .as_ref()
        .context("Cloudflare evidence missing access_audit")?;
    let checked_at = required_evidence_str(&access_audit.checked_at, "access_audit.checked_at")?;
    validate_manual_verification_timestamp(checked_at, "Cloudflare Access audit checked_at")?;
    if access_audit.unauthenticated_blocks_seen != Some(true) {
        bail!("Cloudflare evidence must prove Access audit logs include unauthenticated blocks");
    }
    if access_audit.authenticated_success_seen != Some(true) {
        bail!("Cloudflare evidence must prove Access audit logs include authenticated successes");
    }

    report.push_pass(
        "cloudflare-drift-evidence",
        format!(
            "Cloudflare evidence {} proves exact hostname {public_host}, Access policies, DNS, tunnel route, and audit allow/deny state",
            cloudflare_evidence.display()
        ),
    );
    Ok(())
}

fn validate_cloudflared_public_config(
    cloudflared_config: &Path,
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let public_host = cloudflare_public_host(public_url)?;
    let contents = fs::read_to_string(cloudflared_config).with_context(|| {
        format!(
            "failed to read cloudflared config {}",
            cloudflared_config.display()
        )
    })?;
    let root: YamlValue = serde_yaml::from_str(&contents).with_context(|| {
        format!(
            "failed to parse cloudflared config {}",
            cloudflared_config.display()
        )
    })?;

    validate_cloudflared_private_routing_disabled(&root)?;

    let ingress = yaml_mapping_value(&root, "ingress")
        .and_then(YamlValue::as_sequence)
        .with_context(|| {
            format!(
                "cloudflared config missing ingress sequence: {}",
                cloudflared_config.display()
            )
        })?;
    if ingress.len() < 2 {
        bail!(
            "cloudflared config must contain an exact hostname rule and a final http_status:404 rule: {}",
            cloudflared_config.display()
        );
    }

    let mut matching_hostname_rules = 0usize;
    for (index, rule) in ingress.iter().enumerate() {
        let Some(mapping) = rule.as_mapping() else {
            bail!("cloudflared ingress rule {} is not a mapping", index + 1);
        };
        let hostname = yaml_mapping_str(mapping, "hostname");
        let service = yaml_mapping_str(mapping, "service")
            .with_context(|| format!("cloudflared ingress rule {} missing service", index + 1))?;
        if let Some(hostname) = hostname {
            let hostname = hostname.to_ascii_lowercase();
            if hostname.contains('*') {
                bail!(
                    "cloudflared ingress rule {} uses wildcard hostname {hostname:?}",
                    index + 1
                );
            }
            if hostname != public_host {
                bail!(
                    "cloudflared ingress rule {} publishes unexpected hostname {hostname:?}; expected only {public_host:?}",
                    index + 1
                );
            }
            matching_hostname_rules += 1;
            validate_cloudflared_origin_service(service).with_context(|| {
                format!("unsafe service for cloudflared ingress rule {}", index + 1)
            })?;
        } else if index + 1 != ingress.len() {
            bail!(
                "cloudflared ingress rule {} has no hostname before the final rule; only the final deny rule may omit hostname",
                index + 1
            );
        }
    }

    if matching_hostname_rules != 1 {
        bail!(
            "cloudflared config must publish exactly one rule for {public_host:?}, found {matching_hostname_rules}"
        );
    }

    let final_rule = ingress.last().expect("checked ingress length");
    let final_mapping = final_rule
        .as_mapping()
        .context("cloudflared final ingress rule is not a mapping")?;
    if yaml_mapping_str(final_mapping, "hostname").is_some() {
        bail!("cloudflared final ingress rule must not specify a hostname");
    }
    let final_service = yaml_mapping_str(final_mapping, "service")
        .context("cloudflared final ingress rule missing service")?;
    if final_service != "http_status:404" {
        bail!("cloudflared final ingress rule must be service: http_status:404");
    }

    report.push_pass(
        "cloudflare-tunnel-config",
        format!(
            "cloudflared config {} publishes exact hostname {public_host}, uses loopback/Unix origin, disables private routing, and ends with http_status:404",
            cloudflared_config.display()
        ),
    );
    Ok(())
}

fn cloudflare_public_host(public_url: &str) -> Result<String> {
    Ok(Url::parse(public_url)
        .with_context(|| format!("invalid Cloudflare public URL {public_url}"))?
        .host_str()
        .context("Cloudflare public URL must include a hostname")?
        .to_ascii_lowercase())
}

fn required_evidence_str<'a>(value: &'a Option<String>, field: &str) -> Result<&'a str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("Cloudflare evidence missing {field}"))
}

fn normalize_evidence_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

fn validate_evidence_hostname(field: &str, value: &str, public_host: &str) -> Result<()> {
    let value = value.trim().to_ascii_lowercase();
    if value.contains('*') {
        bail!("Cloudflare evidence {field} uses wildcard hostname {value:?}");
    }
    if value != public_host {
        bail!(
            "Cloudflare evidence {field}={value:?} does not match public hostname {public_host:?}"
        );
    }
    Ok(())
}

fn validate_cloudflared_private_routing_disabled(root: &YamlValue) -> Result<()> {
    let Some(warp_routing) = yaml_mapping_value(root, "warp-routing") else {
        return Ok(());
    };
    let Some(mapping) = warp_routing.as_mapping() else {
        bail!("cloudflared warp-routing must be a mapping when present");
    };
    let enabled = yaml_mapping_value_from_mapping(mapping, "enabled")
        .and_then(YamlValue::as_bool)
        .unwrap_or(false);
    if enabled {
        bail!("cloudflared warp-routing.enabled must be false for the public Office tunnel");
    }
    Ok(())
}

fn validate_cloudflared_origin_service(service: &str) -> Result<()> {
    if service == "http_status:404" {
        bail!("hostname rule must route to the local origin, not http_status:404");
    }
    if service.starts_with("unix:") {
        return Ok(());
    }
    let parsed = Url::parse(service).with_context(|| {
        format!("cloudflared service must be a URL or unix socket, got {service:?}")
    })?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            bail!("cloudflared service scheme {scheme:?} is not allowed for the public origin")
        }
    }
    let host = parsed
        .host_str()
        .context("cloudflared service URL must include a host")?;
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    let ip = host.parse::<std::net::IpAddr>().with_context(|| {
        format!("cloudflared public origin host must be loopback or localhost, got {host:?}")
    })?;
    if !ip.is_loopback() {
        bail!("cloudflared public origin host must be loopback, got {host:?}");
    }
    Ok(())
}

fn yaml_mapping_value<'a>(value: &'a YamlValue, key: &str) -> Option<&'a YamlValue> {
    value
        .as_mapping()
        .and_then(|mapping| yaml_mapping_value_from_mapping(mapping, key))
}

fn yaml_mapping_value_from_mapping<'a>(
    mapping: &'a serde_yaml::Mapping,
    key: &str,
) -> Option<&'a YamlValue> {
    mapping.get(YamlValue::String(key.to_string()))
}

fn yaml_mapping_str<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a str> {
    yaml_mapping_value_from_mapping(mapping, key).and_then(YamlValue::as_str)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicAccessProbeAuth {
    client_id: String,
    client_secret: String,
}

fn public_access_probe_auth(
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> Result<Option<PublicAccessProbeAuth>> {
    match (client_id, client_secret) {
        (Some(client_id), Some(client_secret))
            if !client_id.trim().is_empty() && !client_secret.trim().is_empty() =>
        {
            Ok(Some(PublicAccessProbeAuth {
                client_id: client_id.trim().to_string(),
                client_secret: client_secret.trim().to_string(),
            }))
        }
        (None, None) => Ok(None),
        (Some(client_id), None) if client_id.trim().is_empty() => Ok(None),
        (None, Some(client_secret)) if client_secret.trim().is_empty() => Ok(None),
        _ => bail!(
            "Cloudflare Access validation requires both client id and client secret when either is supplied"
        ),
    }
}

fn apply_public_access_auth(
    builder: reqwest::RequestBuilder,
    access_auth: Option<&PublicAccessProbeAuth>,
) -> reqwest::RequestBuilder {
    if let Some(access_auth) = access_auth {
        builder
            .header("CF-Access-Client-Id", access_auth.client_id.as_str())
            .header(
                "CF-Access-Client-Secret",
                access_auth.client_secret.as_str(),
            )
    } else {
        builder
    }
}

async fn validate_public_access_blocks_unauthenticated(
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to create unauthenticated public Access probe client")?;

    for probe in http::PUBLIC_ACCESS_PROBES {
        let url = join_url(public_url, probe.path)?;
        let request = match probe.method {
            http::PublicAccessProbeMethod::Get => client.get(url),
            http::PublicAccessProbeMethod::Post => client.post(url),
        };
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to probe unauthenticated public {}", probe.path))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await.with_context(|| {
            format!(
                "failed to read unauthenticated public {} response",
                probe.path
            )
        })?;
        validate_public_access_block_http_response(probe.path, status, &headers, &body)?;
        report.push_pass(
            format!("cloudflare-access-blocks-{}", probe.name),
            format!(
                "unauthenticated public {} {} was blocked before origin with status {}",
                probe.method.as_str(),
                probe.path,
                status
            ),
        );
    }

    validate_public_websocket_access_blocked(public_url, report).await?;
    Ok(())
}

fn validate_public_access_block_http_response(
    path: &str,
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) -> Result<()> {
    if looks_like_office_origin_response(status, headers, body) {
        bail!(
            "unauthenticated public {path} reached the Office Automate origin instead of Cloudflare Access"
        );
    }
    if is_cloudflare_access_block_status(status, headers) {
        return Ok(());
    }
    bail!(
        "unauthenticated public {path} was not blocked by Cloudflare Access before origin; status={status}"
    );
}

fn is_cloudflare_access_block_status(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> bool {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return true;
    }
    if status.is_redirection() {
        return headers
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|location| {
                location.contains("/cdn-cgi/access") || location.contains("cloudflareaccess.com")
            });
    }
    false
}

fn looks_like_office_origin_response(
    status: StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &[u8],
) -> bool {
    if headers
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let challenge = value.to_ascii_lowercase();
            challenge.contains("basic")
                || challenge.contains("office automate")
                || challenge.contains("office climate")
        })
    {
        return true;
    }
    if matches!(status, StatusCode::OK | StatusCode::SWITCHING_PROTOCOLS) {
        return true;
    }
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for key in [
            "authorization_url",
            "login_url",
            "state",
            "sensors",
            "air_quality",
            "projects",
            "artifact_hash",
            "download_url",
        ] {
            if value.get(key).is_some() {
                return true;
            }
        }
    }
    let lowercase = trimmed.to_ascii_lowercase();
    lowercase.contains("office automate") || lowercase.contains("officeclimate://auth")
}

async fn validate_public_websocket_access_blocked(
    public_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let request = websocket_url(public_url)?
        .into_client_request()
        .context("failed to build unauthenticated public WebSocket probe request")?;
    let result = timeout(INTERFACE_TIMEOUT, connect_async(request))
        .await
        .context("timed out probing unauthenticated public /ws WebSocket")?;
    match result {
        Ok((_socket, _response)) => {
            bail!(
                "unauthenticated public /ws WebSocket upgrade succeeded; Cloudflare Access did not block before origin"
            )
        }
        Err(TungsteniteError::Http(response)) => {
            let status = StatusCode::from_u16(response.status().as_u16())
                .context("invalid WebSocket probe HTTP status")?;
            let headers = response.headers();
            let location = headers
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let access_blocked = matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
                || (status.is_redirection()
                    && (location.contains("/cdn-cgi/access")
                        || location.contains("cloudflareaccess.com")));
            if !access_blocked {
                bail!(
                    "unauthenticated public /ws WebSocket was not blocked by Cloudflare Access before origin; status={status}"
                );
            }
            report.push_pass(
                "cloudflare-access-blocks-websocket",
                format!(
                    "unauthenticated public /ws WebSocket upgrade was blocked with status {status}"
                ),
            );
            Ok(())
        }
        Err(error) => Err(error).context("failed to verify public /ws Cloudflare Access block"),
    }
}

fn write_cutover_log(
    config: &AppConfig,
    options: &CutoverValidationOptions,
    report: &ShadowValidationReport,
) -> Result<()> {
    if let Some(parent) = options
        .cutover_log
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create cutover log directory {}",
                parent.display()
            )
        })?;
    }

    let base_url = options
        .base_url
        .as_deref()
        .or(config.runtime.base_url.as_deref())
        .unwrap_or("not recorded");
    let public_url = options
        .public_url
        .as_deref()
        .or(config.runtime.public_url.as_deref())
        .unwrap_or("not recorded");
    let legacy_url = options.legacy_base_url.as_deref().unwrap_or("not supplied");
    let manifest = options.snapshot_dir.join("manifest.json");

    let mut contents = String::new();
    writeln!(&mut contents, "# Backend/MQTT Cutover Log\n")?;
    writeln!(
        &mut contents,
        "| Field | Value |\n| --- | --- |\n| Generated at | {} |\n| Rust config | {} |\n| Local Rust URL | {} |\n| Cloudflare public URL | {} |\n| Legacy backend URL | {} |\n| Legacy controller stopped at | {} |\n| MQTT strategy | {} |\n| Rust MQTT broker | {}:{} |\n| Rollback snapshot | {} |\n| Snapshot manifest | {} |\n| Cutover log | {} |",
        markdown_cell(&Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()),
        markdown_cell(&config.runtime.config_path.display().to_string()),
        markdown_cell(base_url),
        markdown_cell(public_url),
        markdown_cell(legacy_url),
        markdown_cell(&options.legacy_controller_stopped_at),
        options.mqtt_strategy.as_str(),
        markdown_cell(&config.runtime.mqtt_host),
        config.runtime.mqtt_port,
        markdown_cell(&options.snapshot_dir.display().to_string()),
        markdown_cell(&manifest.display().to_string()),
        markdown_cell(&options.cutover_log.display().to_string()),
    )?;

    writeln!(&mut contents, "\n## Checks\n")?;
    writeln!(&mut contents, "| Status | Check | Detail |")?;
    writeln!(&mut contents, "| --- | --- | --- |")?;
    for check in &report.checks {
        writeln!(
            &mut contents,
            "| {} | {} | {} |",
            check.status.as_str(),
            markdown_cell(&check.name),
            markdown_cell(&check.detail),
        )?;
    }

    writeln!(&mut contents, "\n## Rollback Point\n")?;
    writeln!(
        &mut contents,
        "Use the snapshot at `{}` as the rollback source for this cutover window.",
        options.snapshot_dir.display()
    )?;
    writeln!(
        &mut contents,
        "Rollback sequence: stop `office-automate-server` and the Cloudflare Tunnel service, start the legacy backend and legacy tunnel, repoint Qingping to the legacy broker if it moved, then restore copied state from the snapshot if Rust wrote incompatible data."
    )?;

    fs::write(&options.cutover_log, contents).with_context(|| {
        format!(
            "failed to write cutover log {}",
            options.cutover_log.display()
        )
    })?;
    Ok(())
}

fn write_rollback_log(
    config: &AppConfig,
    options: &RollbackValidationOptions,
    report: &ShadowValidationReport,
) -> Result<()> {
    if let Some(parent) = options
        .rollback_log
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create rollback log directory {}",
                parent.display()
            )
        })?;
    }

    let legacy_base_url = options.legacy_base_url.as_deref().unwrap_or("not supplied");
    let legacy_public_url = options
        .legacy_public_url
        .as_deref()
        .unwrap_or("not supplied");
    let rust_base_url = options.rust_base_url.as_deref().unwrap_or("not supplied");
    let rust_public_url = options.rust_public_url.as_deref().unwrap_or("not supplied");
    let manifest = options.snapshot_dir.join("manifest.json");

    let mut contents = String::new();
    writeln!(&mut contents, "# Backend/MQTT Rollback Log\n")?;
    writeln!(
        &mut contents,
        "| Field | Value |\n| --- | --- |\n| Generated at | {} |\n| Rust config | {} |\n| Legacy local URL | {} |\n| Legacy Cloudflare public URL | {} |\n| Rust local URL probe | {} |\n| Rust public URL probe | {} |\n| Rust stopped at | {} |\n| Legacy started at | {} |\n| MQTT rollback state | {} |\n| Restore verification | {} |\n| Rollback snapshot | {} |\n| Snapshot manifest | {} |\n| Rollback log | {} |",
        markdown_cell(&Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()),
        markdown_cell(&config.runtime.config_path.display().to_string()),
        markdown_cell(legacy_base_url),
        markdown_cell(legacy_public_url),
        markdown_cell(rust_base_url),
        markdown_cell(rust_public_url),
        markdown_cell(&options.rust_stopped_at),
        markdown_cell(&options.legacy_started_at),
        options.mqtt_rollback_state.as_str(),
        options.restore_verification.as_str(),
        markdown_cell(&options.snapshot_dir.display().to_string()),
        markdown_cell(&manifest.display().to_string()),
        markdown_cell(&options.rollback_log.display().to_string()),
    )?;

    writeln!(&mut contents, "\n## Checks\n")?;
    writeln!(&mut contents, "| Status | Check | Detail |")?;
    writeln!(&mut contents, "| --- | --- | --- |")?;
    for check in &report.checks {
        writeln!(
            &mut contents,
            "| {} | {} | {} |",
            check.status.as_str(),
            markdown_cell(&check.name),
            markdown_cell(&check.detail),
        )?;
    }

    writeln!(&mut contents, "\n## Recovery State\n")?;
    writeln!(
        &mut contents,
        "Legacy backend and Cloudflare Tunnel are the active climate-control path after this rollback validation."
    )?;
    writeln!(
        &mut contents,
        "Keep Rust active-control flags disabled before any later shadow-mode follow-up."
    )?;

    fs::write(&options.rollback_log, contents).with_context(|| {
        format!(
            "failed to write rollback log {}",
            options.rollback_log.display()
        )
    })?;
    Ok(())
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', "<br>")
        .replace('\r', "")
}

async fn validate_artifact_interface(
    client: &reqwest::Client,
    base_url: &str,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let url = join_url(base_url, "/apps/office-climate/meta.json")?;
    let response = client
        .get(url)
        .send()
        .await
        .context("failed to call artifact metadata endpoint")?;
    match response.status() {
        StatusCode::OK => {
            let metadata: Value = response
                .json()
                .await
                .context("failed to parse artifact metadata JSON")?;
            let hash = metadata
                .get("artifact_hash")
                .and_then(Value::as_str)
                .context("artifact metadata missing artifact_hash")?;
            if !is_valid_artifact_hash(hash) {
                bail!("artifact metadata contains invalid artifact_hash {hash:?}");
            }
            let sha256 = metadata
                .get("sha256")
                .and_then(Value::as_str)
                .context("artifact metadata missing sha256")?;
            if !is_valid_sha256_digest(sha256) {
                bail!("artifact metadata contains invalid sha256 {sha256:?}");
            }
            if !sha256.starts_with(hash) {
                bail!("artifact metadata hash prefix does not match sha256");
            }
            report.push_pass(
                "artifact-interface",
                format!("office-climate metadata exists with artifact_hash={hash} and full sha256"),
            );
        }
        StatusCode::NOT_FOUND => report.push_skip(
            "artifact-interface",
            "office-climate artifact metadata not present in copied data",
        ),
        status => bail!("artifact metadata endpoint returned {status}"),
    }
    Ok(())
}

async fn validate_auth_interface(
    config: &AppConfig,
    client: &reqwest::Client,
    base_url: &str,
    auth: &InterfaceProbeAuth,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let url = join_url(base_url, "/auth/login")?;
    let response = auth
        .apply_http_auth(client.get(url))
        .header(reqwest::header::HOST, "office-automate-shadow.local")
        .send()
        .await
        .context("failed to call auth login endpoint")?;
    if config.orchestrator.google_oauth.is_some() {
        if response.status() != StatusCode::OK {
            bail!("OAuth login endpoint returned {}", response.status());
        }
        let payload: Value = response
            .json()
            .await
            .context("failed to parse OAuth login payload")?;
        if !payload
            .get("authorization_url")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("https://"))
            || !payload.get("state").and_then(Value::as_str).is_some()
        {
            bail!("OAuth login payload missing authorization_url or state");
        }
        report.push_pass(
            "oauth-interface",
            "/auth/login returned OAuth start payload",
        );
    } else if response.status() == StatusCode::NOT_IMPLEMENTED {
        report.push_pass(
            "oauth-interface",
            "/auth/login correctly reports OAuth not configured",
        );
    } else {
        bail!(
            "OAuth disabled config expected 501 from /auth/login, got {}",
            response.status()
        );
    }
    Ok(())
}

async fn validate_websocket_interface(
    base_url: &str,
    auth: &InterfaceProbeAuth,
    report: &mut ShadowValidationReport,
) -> Result<()> {
    let status = probe_websocket_status(base_url, auth).await?;
    validate_status_shape(&status)?;
    report.push_pass(
        "websocket-auth-interface",
        format!(
            "/ws delivered authenticated initial status from {base_url} using {}",
            auth.description()
        ),
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InterfaceProbeAuth {
    Open,
    Basic { authorization: String },
    OAuthFirstMessage { token: String, email: String },
    OAuthTrustedNetwork { forwarded_for: String },
}

impl InterfaceProbeAuth {
    fn description(&self) -> &'static str {
        match self {
            Self::Open => "open WebSocket mode",
            Self::Basic { .. } => "Basic Authorization header",
            Self::OAuthFirstMessage { .. } => "OAuth first-message token",
            Self::OAuthTrustedNetwork { .. } => "OAuth trusted-network bypass",
        }
    }

    fn supports_public_http_auth(&self) -> bool {
        !matches!(self, Self::OAuthTrustedNetwork { .. })
    }

    fn apply_http_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::Open => builder,
            Self::Basic { authorization } => {
                builder.header(reqwest::header::AUTHORIZATION, authorization.as_str())
            }
            Self::OAuthFirstMessage { token, .. } => builder.bearer_auth(token),
            Self::OAuthTrustedNetwork { forwarded_for } => {
                builder.header("x-forwarded-for", forwarded_for.as_str())
            }
        }
    }
}

fn interface_probe_auth(config: &AppConfig) -> Result<InterfaceProbeAuth> {
    if let Some(oauth) = &config.orchestrator.google_oauth {
        if oauth
            .jwt_secret
            .as_deref()
            .is_some_and(|secret| !secret.trim().is_empty())
        {
            let email = oauth
                .allowed_emails
                .iter()
                .find(|email| !email.trim().is_empty())
                .context("OAuth WebSocket validation requires at least one allowed email")?
                .trim()
                .to_ascii_lowercase();
            let token = AuthManager::new(&config.orchestrator)?
                .generate_jwt(&email)
                .context("failed to generate validation WebSocket JWT")?;
            return Ok(InterfaceProbeAuth::OAuthFirstMessage { token, email });
        }

        if let Some(forwarded_for) = first_trusted_network_probe_ip(&oauth.trusted_networks)? {
            return Ok(InterfaceProbeAuth::OAuthTrustedNetwork { forwarded_for });
        }

        bail!(
            "OAuth WebSocket validation requires google_oauth.jwt_secret or a trusted_networks entry"
        );
    }

    match (
        config.orchestrator.auth_username.as_deref(),
        config.orchestrator.auth_password.as_deref(),
    ) {
        (Some(username), Some(password))
            if !username.trim().is_empty() && !password.trim().is_empty() =>
        {
            let encoded = general_purpose::STANDARD.encode(format!("{username}:{password}"));
            Ok(InterfaceProbeAuth::Basic {
                authorization: format!("Basic {encoded}"),
            })
        }
        _ => Ok(InterfaceProbeAuth::Open),
    }
}

fn first_trusted_network_probe_ip(networks: &[String]) -> Result<Option<String>> {
    for network in networks {
        let network = network.trim();
        if network.is_empty() {
            continue;
        }
        let network = network
            .parse::<IpNet>()
            .with_context(|| format!("invalid trusted network {network:?}"))?;
        return Ok(Some(network.addr().to_string()));
    }
    Ok(None)
}

async fn probe_websocket_status(base_url: &str, auth: &InterfaceProbeAuth) -> Result<Value> {
    let mut request = websocket_url(base_url)?.into_client_request()?;
    match auth {
        InterfaceProbeAuth::Basic { authorization } => {
            request.headers_mut().insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(authorization)
                    .context("invalid Basic authorization header")?,
            );
        }
        InterfaceProbeAuth::OAuthTrustedNetwork { forwarded_for } => {
            request.headers_mut().insert(
                "x-forwarded-for",
                HeaderValue::from_str(forwarded_for)
                    .context("invalid trusted-network forwarded address")?,
            );
        }
        InterfaceProbeAuth::Open | InterfaceProbeAuth::OAuthFirstMessage { .. } => {}
    }

    let (mut socket, _) = timeout(INTERFACE_TIMEOUT, connect_async(request))
        .await
        .context("timed out connecting to /ws")?
        .context("failed to connect to /ws")?;

    if let InterfaceProbeAuth::OAuthFirstMessage { token, .. } = auth {
        socket
            .send(TungsteniteMessage::Text(
                json!({"type": "auth", "token": token}).to_string().into(),
            ))
            .await
            .context("failed to send WebSocket auth message")?;
    }

    let message = timeout(INTERFACE_TIMEOUT, socket.next())
        .await
        .context("timed out waiting for initial WebSocket status")?
        .context("WebSocket closed before initial status")?
        .context("WebSocket returned an error before initial status")?;

    match message {
        TungsteniteMessage::Text(text) => {
            serde_json::from_str(&text).context("initial WebSocket message was not JSON")
        }
        TungsteniteMessage::Close(frame) => {
            bail!("WebSocket closed before initial status: {frame:?}")
        }
        other => bail!("WebSocket returned non-text initial message: {other:?}"),
    }
}

async fn get_json(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    auth: Option<&InterfaceProbeAuth>,
) -> Result<Value> {
    let url = join_url(base_url, path)?;
    let mut request = client.get(url);
    if let Some(auth) = auth {
        request = auth.apply_http_auth(request);
    }
    request
        .send()
        .await
        .with_context(|| format!("failed to call {path}"))?
        .error_for_status()
        .with_context(|| format!("{path} returned non-success status"))?
        .json()
        .await
        .with_context(|| format!("failed to parse {path} JSON"))
}

async fn get_public_json(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    auth: Option<&InterfaceProbeAuth>,
    access_auth: Option<&PublicAccessProbeAuth>,
) -> Result<Value> {
    let url = join_url(base_url, path)?;
    let mut request = apply_public_access_auth(client.get(url), access_auth);
    if let Some(auth) = auth {
        request = auth.apply_http_auth(request);
    }
    request
        .send()
        .await
        .with_context(|| format!("failed to call public {path}"))?
        .error_for_status()
        .with_context(|| format!("public {path} returned non-success status"))?
        .json()
        .await
        .with_context(|| format!("failed to parse public {path} JSON"))
}

fn validate_manual_verification_timestamp(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{label} timestamp cannot be empty");
    }
    Ok(())
}

fn join_url(base_url: &str, path: &str) -> Result<Url> {
    let mut base = base_url.trim_end_matches('/').to_string();
    base.push_str(path);
    Url::parse(&base).with_context(|| format!("invalid validation URL {base}"))
}

fn websocket_url(base_url: &str) -> Result<String> {
    let mut url = join_url(base_url, "/ws")?;
    match url.scheme() {
        "http" => {
            if url.set_scheme("ws").is_err() {
                bail!("failed to convert validation URL to ws scheme");
            }
        }
        "https" => {
            if url.set_scheme("wss").is_err() {
                bail!("failed to convert validation URL to wss scheme");
            }
        }
        "ws" | "wss" => {}
        scheme => bail!("unsupported WebSocket validation URL scheme {scheme:?}"),
    }
    Ok(url.to_string())
}

fn validate_status_shape(status: &Value) -> Result<()> {
    for key in [
        "state",
        "is_present",
        "sensors",
        "air_quality",
        "erv",
        "hvac",
        "manual_override",
    ] {
        if status.get(key).is_none() {
            bail!("/status response missing {key}");
        }
    }
    Ok(())
}

fn validate_fresh_air_quality(status: &Value, max_age_seconds: u64) -> Result<()> {
    let last_update = status
        .get("air_quality")
        .and_then(|air_quality| air_quality.get("last_update"))
        .and_then(Value::as_str)
        .context("/status air_quality.last_update is missing; Qingping shadow feed is not fresh")?;
    let updated_at = parse_air_quality_last_update(last_update)?;
    let updated_at = Local
        .from_local_datetime(&updated_at)
        .single()
        .with_context(|| format!("ambiguous air_quality.last_update {last_update:?}"))?;
    let age = Local::now()
        .signed_duration_since(updated_at)
        .num_seconds()
        .max(0) as u64;
    if age > max_age_seconds {
        bail!(
            "Qingping air-quality reading is stale: age={}s max={}s",
            age,
            max_age_seconds
        );
    }
    Ok(())
}

fn parse_air_quality_last_update(last_update: &str) -> Result<NaiveDateTime> {
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(last_update, format) {
            return Ok(parsed);
        }
    }
    bail!("invalid air_quality.last_update {last_update:?}");
}

fn current_timestamp_seconds() -> f64 {
    Local::now().timestamp_millis() as f64 / 1_000.0
}

fn present_label(value: &Option<String>) -> &'static str {
    if value.is_some() { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{
            ErvConfig, GoogleOAuthConfig, MitsubishiConfig, OrchestratorConfig, QingpingConfig,
            RuntimeConfig, ThresholdsConfig, YoLinkConfig,
        },
        db,
    };

    fn test_config(database_path: &Path) -> AppConfig {
        let root = database_path
            .parent()
            .expect("database parent")
            .to_path_buf();
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            presence: crate::config::PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            erv: ErvConfig::default(),
            mitsubishi: MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            telemetry: crate::config::TelemetryConfig::default(),
            runtime: RuntimeConfig {
                root: root.clone(),
                config_path: root.join("config.yaml"),
                data_dir: root.clone(),
                database_path: database_path.to_path_buf(),
                frontend_dist: root.join("frontend/dist"),
                artifacts_dir: root.join("apps"),
                legacy_apk_path: root.join("app-debug.apk"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
                telemetry_db_path: root.join("telemetry.db"),
                session_tool_usage_db_path: root.join("claude-tool-usage.db"),
                tool_usage_db_path: root.join("tool_usage.db"),
                engram_db_path: root.join("engram_state.db"),
                engram_registry_path: root.join("engram_concept_registry.md"),
            },
        }
    }

    #[tokio::test]
    async fn shadow_validation_rejects_active_write_gates() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&db_path).expect("migration");
        let mut config = test_config(&db_path);
        config.erv.active_control_enabled = true;

        let error = run_shadow_validation(
            &config,
            ShadowValidationOptions {
                skip_live_devices: true,
                skip_http_interface: true,
                ..ShadowValidationOptions::default()
            },
        )
        .await
        .expect_err("active ERV writes should fail shadow validation");

        assert!(error.to_string().contains("ERV active control disabled"));
    }

    #[tokio::test]
    async fn shadow_validation_can_run_offline_database_checks() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&db_path).expect("migration");
        let config = test_config(&db_path);

        let report = run_shadow_validation(
            &config,
            ShadowValidationOptions {
                skip_live_devices: true,
                skip_http_interface: true,
                ..ShadowValidationOptions::default()
            },
        )
        .await
        .expect("offline validation");

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "active-write-gates")
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "office-climate-db")
        );
    }

    #[test]
    fn validation_records_http_startup_config_and_rejects_public_basic_mode() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut report = ShadowValidationReport { checks: Vec::new() };
        let config = test_config(&db_path);

        validate_http_startup_config(&config, None, &mut report)
            .expect("default config should pass");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "http-startup-config")
        );

        let mut public_basic = test_config(&db_path);
        public_basic.runtime.public_url = Some("https://office.example.test".to_string());
        public_basic.orchestrator.auth_username = Some("user".to_string());
        public_basic.orchestrator.auth_password = Some("pass".to_string());
        let error = validate_http_startup_config(&public_basic, None, &mut report)
            .expect_err("public Basic-only config should fail validation");
        assert!(error.to_string().contains("requires Google OAuth/JWT"));
    }

    #[tokio::test]
    async fn shadow_validation_rejects_option_only_public_basic_mode() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.orchestrator.auth_username = Some("user".to_string());
        config.orchestrator.auth_password = Some("pass".to_string());

        let error = run_shadow_validation(
            &config,
            ShadowValidationOptions {
                public_url: Some("https://office.example.test".to_string()),
                skip_live_devices: true,
                skip_http_interface: true,
                ..ShadowValidationOptions::default()
            },
        )
        .await
        .expect_err("option-only public Basic mode should fail shadow validation");

        assert!(error.to_string().contains("requires Google OAuth/JWT"));
    }

    #[tokio::test]
    async fn cutover_validation_rejects_option_only_public_basic_mode() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.orchestrator.auth_username = Some("user".to_string());
        config.orchestrator.auth_password = Some("pass".to_string());

        let error = run_cutover_validation(
            &config,
            CutoverValidationOptions {
                base_url: None,
                public_url: Some("https://office.example.test".to_string()),
                legacy_base_url: None,
                legacy_controller_stopped_at: String::new(),
                mqtt_strategy: MqttCutoverStrategy::AtomicSwitch,
                snapshot_dir: temp_dir.path().join("snapshot"),
                cutover_log: temp_dir.path().join("cutover.md"),
                manual_public_oauth_verified_at: None,
                cloudflared_config: None,
                cloudflare_evidence: None,
                cloudflare_access_client_id: None,
                cloudflare_access_client_secret: None,
                max_air_quality_age_seconds: 300,
            },
        )
        .await
        .expect_err(
            "option-only public Basic mode should fail cutover validation before other gates",
        );

        assert!(error.to_string().contains("requires Google OAuth/JWT"));
    }

    #[test]
    fn cutover_validation_requires_active_write_gates() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error = validate_cutover_active_write_gates(&config, &mut report)
            .expect_err("inactive writes should fail cutover validation");
        assert!(error.to_string().contains("ERV active control enabled"));

        config.erv.active_control_enabled = true;
        config.mitsubishi.active_control_enabled = true;
        validate_cutover_active_write_gates(&config, &mut report).expect("active write gates pass");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "rust-active-write-gates")
        );
    }

    #[test]
    fn rollback_validation_requires_inactive_write_gates() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        let mut report = ShadowValidationReport { checks: Vec::new() };

        config.erv.active_control_enabled = true;
        let error = validate_rollback_active_write_gates(&config, &mut report)
            .expect_err("active ERV writes should fail rollback validation");
        assert!(error.to_string().contains("ERV active control disabled"));

        config.erv.active_control_enabled = false;
        validate_rollback_active_write_gates(&config, &mut report)
            .expect("inactive gates should pass");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "rust-active-write-gates-disabled")
        );
    }

    #[test]
    fn cutover_validation_requires_snapshot_manifest() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error = validate_cutover_snapshot(temp_dir.path(), &mut report)
            .expect_err("missing manifest should fail");
        assert!(error.to_string().contains("manifest is missing"));

        fs::write(temp_dir.path().join("manifest.json"), "{}").expect("manifest");
        validate_cutover_snapshot(temp_dir.path(), &mut report).expect("snapshot passes");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "rollback-snapshot")
        );
    }

    #[tokio::test]
    async fn cutover_validation_requires_legacy_stop_timestamp() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let options = CutoverValidationOptions {
            base_url: Some("http://127.0.0.1:9001".to_string()),
            public_url: Some("https://office.example.test".to_string()),
            legacy_base_url: None,
            legacy_controller_stopped_at: String::new(),
            mqtt_strategy: MqttCutoverStrategy::AtomicSwitch,
            snapshot_dir: temp_dir.path().join("snapshot"),
            cutover_log: temp_dir.path().join("cutover.md"),
            manual_public_oauth_verified_at: None,
            cloudflared_config: None,
            cloudflare_evidence: None,
            cloudflare_access_client_id: None,
            cloudflare_access_client_secret: None,
            max_air_quality_age_seconds: 300,
        };
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error = validate_legacy_controller_stopped(&options, &mut report)
            .await
            .expect_err("empty timestamp should fail");
        assert!(error.to_string().contains("legacy-controller-stopped-at"));
    }

    #[tokio::test]
    async fn rollback_validation_requires_rust_stop_timestamp() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let options = RollbackValidationOptions {
            legacy_base_url: Some("http://legacy.example.test".to_string()),
            legacy_public_url: None,
            rust_base_url: None,
            rust_public_url: None,
            rust_stopped_at: String::new(),
            legacy_started_at: "2026-06-06T04:05:00-07:00".to_string(),
            mqtt_rollback_state: MqttRollbackState::RepointedLegacy,
            snapshot_dir: temp_dir.path().join("snapshot"),
            restore_verification: RestoreVerification::RestoredFromSnapshot,
            rollback_log: temp_dir.path().join("rollback.md"),
            manual_legacy_public_verified_at: None,
            max_air_quality_age_seconds: 300,
        };
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error = validate_rust_controller_stopped(&options, &mut report)
            .await
            .expect_err("empty timestamp should fail");
        assert!(error.to_string().contains("rust-stopped-at"));
    }

    #[test]
    fn cutover_validation_records_mqtt_strategy() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error =
            validate_mqtt_cutover_strategy(&config, MqttCutoverStrategy::AtomicSwitch, &mut report)
                .expect_err("missing device mac should fail");
        assert!(error.to_string().contains("qingping.device_mac"));

        config.qingping.device_mac = Some("AA:BB:CC:DD:EE:FF".to_string());
        validate_mqtt_cutover_strategy(&config, MqttCutoverStrategy::AtomicSwitch, &mut report)
            .expect("strategy passes");
        let detail = report
            .checks
            .iter()
            .find(|check| check.name == "mqtt-feed-strategy")
            .expect("mqtt check")
            .detail
            .as_str();
        assert!(detail.contains("atomic switch strategy"));
        assert!(detail.contains("127.0.0.1:1883"));
    }

    #[test]
    fn rollback_validation_records_mqtt_state_and_restore_decision() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error =
            validate_mqtt_rollback_state(&config, MqttRollbackState::RepointedLegacy, &mut report)
                .expect_err("missing device mac should fail");
        assert!(error.to_string().contains("qingping.device_mac"));

        config.qingping.device_mac = Some("AA:BB:CC:DD:EE:FF".to_string());
        validate_mqtt_rollback_state(&config, MqttRollbackState::RepointedLegacy, &mut report)
            .expect("mqtt rollback state passes");
        validate_restore_verification(RestoreVerification::VerifiedSafeNoRestore, &mut report)
            .expect("restore verification passes");

        let mqtt_detail = report
            .checks
            .iter()
            .find(|check| check.name == "mqtt-feed-rollback")
            .expect("mqtt check")
            .detail
            .as_str();
        assert!(mqtt_detail.contains("repointed"));
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "snapshot-restore-verification")
        );
    }

    #[test]
    fn legacy_climate_safety_rejects_invalid_erv_key() {
        let status = json!({
            "state": "away",
            "is_present": false,
            "sensors": {},
            "air_quality": {"last_update": "2026-06-06T04:00:00"},
            "erv": {"control": {"local_key_invalid": true}},
            "hvac": {},
            "manual_override": {},
            "safety_interlock": false
        });

        let error = validate_legacy_climate_safety(&status)
            .expect_err("invalid ERV key should fail safety check");
        assert!(error.to_string().contains("local key is invalid"));

        let mut recovered = status;
        recovered["erv"]["control"]["local_key_invalid"] = json!(false);
        validate_legacy_climate_safety(&recovered).expect("recovered safety passes");
    }

    #[test]
    fn air_quality_freshness_accepts_whole_and_fractional_local_timestamps() {
        let whole_seconds = Local::now()
            .naive_local()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        let fractional_seconds = format!("{whole_seconds}.123456");

        for last_update in [whole_seconds, fractional_seconds] {
            let status = json!({
                "air_quality": {"last_update": last_update},
            });

            validate_fresh_air_quality(&status, 60)
                .expect("fresh air-quality timestamp should pass");
        }
    }

    #[test]
    fn write_cutover_log_records_checks_and_rollback_point() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.runtime.base_url = Some("http://127.0.0.1:9001".to_string());
        config.runtime.public_url = Some("https://office.example.test".to_string());
        let snapshot_dir = temp_dir.path().join("snapshot");
        fs::create_dir(&snapshot_dir).expect("snapshot dir");
        fs::write(snapshot_dir.join("manifest.json"), "{}").expect("manifest");
        let options = CutoverValidationOptions {
            base_url: None,
            public_url: None,
            legacy_base_url: Some("http://legacy.example.test".to_string()),
            legacy_controller_stopped_at: "2026-06-06T03:00:00-07:00".to_string(),
            mqtt_strategy: MqttCutoverStrategy::AtomicSwitch,
            snapshot_dir: snapshot_dir.clone(),
            cutover_log: temp_dir.path().join("logs").join("cutover.md"),
            manual_public_oauth_verified_at: Some("2026-06-06T03:10:00-07:00".to_string()),
            cloudflared_config: None,
            cloudflare_evidence: None,
            cloudflare_access_client_id: None,
            cloudflare_access_client_secret: None,
            max_air_quality_age_seconds: 300,
        };
        let report = ShadowValidationReport {
            checks: vec![ValidationCheck {
                name: "cloudflare-public-status".to_string(),
                status: ValidationStatus::Passed,
                detail: "public URL returned fresh /status".to_string(),
            }],
        };

        write_cutover_log(&config, &options, &report).expect("write log");

        let contents = fs::read_to_string(&options.cutover_log).expect("log contents");
        assert!(contents.contains("# Backend/MQTT Cutover Log"));
        assert!(contents.contains("atomic-switch"));
        assert!(contents.contains("cloudflare-public-status"));
        assert!(contents.contains(&snapshot_dir.display().to_string()));
        assert!(contents.contains("Rollback sequence"));
    }

    #[test]
    fn write_rollback_log_records_checks_and_recovery_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.runtime.base_url = Some("http://127.0.0.1:9001".to_string());
        let snapshot_dir = temp_dir.path().join("snapshot");
        fs::create_dir(&snapshot_dir).expect("snapshot dir");
        fs::write(snapshot_dir.join("manifest.json"), "{}").expect("manifest");
        let options = RollbackValidationOptions {
            legacy_base_url: Some("http://legacy.example.test".to_string()),
            legacy_public_url: Some("https://office.example.test".to_string()),
            rust_base_url: Some("http://127.0.0.1:9001".to_string()),
            rust_public_url: None,
            rust_stopped_at: "2026-06-06T04:00:00-07:00".to_string(),
            legacy_started_at: "2026-06-06T04:05:00-07:00".to_string(),
            mqtt_rollback_state: MqttRollbackState::RepointedLegacy,
            snapshot_dir: snapshot_dir.clone(),
            restore_verification: RestoreVerification::RestoredFromSnapshot,
            rollback_log: temp_dir.path().join("logs").join("rollback.md"),
            manual_legacy_public_verified_at: Some("2026-06-06T04:10:00-07:00".to_string()),
            max_air_quality_age_seconds: 300,
        };
        let report = ShadowValidationReport {
            checks: vec![ValidationCheck {
                name: "legacy-status-fresh".to_string(),
                status: ValidationStatus::Passed,
                detail: "legacy /status recovered".to_string(),
            }],
        };

        write_rollback_log(&config, &options, &report).expect("write log");

        let contents = fs::read_to_string(&options.rollback_log).expect("log contents");
        assert!(contents.contains("# Backend/MQTT Rollback Log"));
        assert!(contents.contains("repointed-legacy"));
        assert!(contents.contains("restored-from-snapshot"));
        assert!(contents.contains("legacy-status-fresh"));
        assert!(contents.contains(&snapshot_dir.display().to_string()));
        assert!(contents.contains("Legacy backend and Cloudflare Tunnel are the active"));
    }

    #[test]
    fn cloudflared_public_config_accepts_exact_loopback_origin_and_final_404() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_path = temp_dir.path().join("cloudflared.yml");
        fs::write(
            &config_path,
            "credentials-file: tunnel.json\ningress:\n  - hostname: office.example.test\n    service: http://127.0.0.1:9001\n  - service: http_status:404\n",
        )
        .expect("cloudflared config");
        let mut report = ShadowValidationReport { checks: Vec::new() };

        validate_cloudflared_public_config(
            &config_path,
            "https://office.example.test",
            &mut report,
        )
        .expect("config should pass");

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "cloudflare-tunnel-config")
        );
    }

    #[test]
    fn cloudflared_public_config_rejects_private_origin_wildcards_and_missing_404() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let private_origin = temp_dir.path().join("private-origin.yml");
        fs::write(
            &private_origin,
            "ingress:\n  - hostname: office.example.test\n    service: http://192.168.5.10:9001\n  - service: http_status:404\n",
        )
        .expect("private origin config");
        let wildcard = temp_dir.path().join("wildcard.yml");
        fs::write(
            &wildcard,
            "ingress:\n  - hostname: '*.example.test'\n    service: http://127.0.0.1:9001\n  - service: http_status:404\n",
        )
        .expect("wildcard config");
        let no_final_404 = temp_dir.path().join("no-final-404.yml");
        fs::write(
            &no_final_404,
            "ingress:\n  - hostname: office.example.test\n    service: http://127.0.0.1:9001\n  - service: http://127.0.0.1:9002\n",
        )
        .expect("missing final 404 config");
        let warp_routing = temp_dir.path().join("warp-routing.yml");
        fs::write(
            &warp_routing,
            "warp-routing:\n  enabled: true\ningress:\n  - hostname: office.example.test\n    service: http://127.0.0.1:9001\n  - service: http_status:404\n",
        )
        .expect("warp routing config");

        for (path, expected) in [
            (
                &private_origin,
                "unsafe service for cloudflared ingress rule",
            ),
            (&wildcard, "wildcard hostname"),
            (
                &no_final_404,
                "final ingress rule must be service: http_status:404",
            ),
            (&warp_routing, "warp-routing.enabled must be false"),
        ] {
            let mut report = ShadowValidationReport { checks: Vec::new() };
            let error = validate_cloudflared_public_config(
                path,
                "https://office.example.test",
                &mut report,
            )
            .expect_err("unsafe tunnel config should fail");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }

    #[test]
    fn cloudflare_drift_evidence_accepts_fail_closed_account_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let evidence_path = temp_dir.path().join("cloudflare-evidence.json");
        fs::write(
            &evidence_path,
            serde_json::to_vec(&valid_cloudflare_drift_evidence()).expect("evidence json"),
        )
        .expect("write evidence");
        let mut report = ShadowValidationReport { checks: Vec::new() };

        validate_cloudflare_drift_evidence(
            &evidence_path,
            "https://office.example.test",
            &mut report,
        )
        .expect("evidence should pass");

        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "cloudflare-drift-evidence")
        );
    }

    #[test]
    fn cloudflare_drift_evidence_rejects_unsafe_account_drift() {
        let mut bypass_policy = valid_cloudflare_drift_evidence();
        bypass_policy["access_application"]["policies"][0]["action"] = json!("Bypass");

        let mut public_policy = valid_cloudflare_drift_evidence();
        public_policy["access_application"]["policies"][0]["includes_public"] = json!(true);

        let mut wildcard_dns = valid_cloudflare_drift_evidence();
        wildcard_dns["dns"]["wildcard_records"] = json!(["*.example.test"]);

        let mut private_route = valid_cloudflare_drift_evidence();
        private_route["tunnel"]["private_network_routes"] = json!(["192.168.5.0/24"]);

        let mut missing_audit = valid_cloudflare_drift_evidence();
        missing_audit["access_audit"]["unauthenticated_blocks_seen"] = json!(false);

        let cases = [
            (bypass_policy, "Bypass"),
            (public_policy, "includes_public=false"),
            (wildcard_dns, "wildcard DNS"),
            (private_route, "private network routes"),
            (missing_audit, "unauthenticated blocks"),
        ];

        for (index, (evidence, expected)) in cases.into_iter().enumerate() {
            let temp_dir = tempfile::tempdir().expect("temp dir");
            let evidence_path = temp_dir
                .path()
                .join(format!("cloudflare-evidence-{index}.json"));
            fs::write(
                &evidence_path,
                serde_json::to_vec(&evidence).expect("evidence json"),
            )
            .expect("write evidence");
            let mut report = ShadowValidationReport { checks: Vec::new() };

            let error = validate_cloudflare_drift_evidence(
                &evidence_path,
                "https://office.example.test",
                &mut report,
            )
            .expect_err("unsafe Cloudflare evidence should fail");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }

    #[test]
    fn cloudflare_drift_evidence_rejects_wrong_hostname_and_origin() {
        let mut wrong_hostname = valid_cloudflare_drift_evidence();
        wrong_hostname["hostname"] = json!("other.example.test");

        let mut private_origin = valid_cloudflare_drift_evidence();
        private_origin["tunnel"]["origin_service"] = json!("http://192.168.5.10:9001");

        let cases = [
            (wrong_hostname, "does not match public hostname"),
            (private_origin, "tunnel.origin_service is unsafe"),
        ];

        for (index, (evidence, expected)) in cases.into_iter().enumerate() {
            let temp_dir = tempfile::tempdir().expect("temp dir");
            let evidence_path = temp_dir
                .path()
                .join(format!("cloudflare-evidence-{index}.json"));
            fs::write(
                &evidence_path,
                serde_json::to_vec(&evidence).expect("evidence json"),
            )
            .expect("write evidence");
            let mut report = ShadowValidationReport { checks: Vec::new() };

            let error = validate_cloudflare_drift_evidence(
                &evidence_path,
                "https://office.example.test",
                &mut report,
            )
            .expect_err("unsafe Cloudflare evidence should fail");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }

    fn valid_cloudflare_drift_evidence() -> Value {
        json!({
            "source": "cloudflare_api",
            "captured_at": "2026-06-07T14:30:00-07:00",
            "hostname": "office.example.test",
            "access_application": {
                "hostname": "office.example.test",
                "require_access": true,
                "policies": [
                    {
                        "name": "allow-device-mtls",
                        "action": "Service Auth",
                        "includes_public": false
                    },
                    {
                        "name": "allow-rajesh",
                        "action": "Allow",
                        "includes_public": false
                    }
                ]
            },
            "dns": {
                "wildcard_records": []
            },
            "tunnel": {
                "hostname": "office.example.test",
                "origin_service": "http://127.0.0.1:9001",
                "private_network_routes": [],
                "final_ingress_service": "http_status:404"
            },
            "access_audit": {
                "checked_at": "2026-06-07T14:35:00-07:00",
                "unauthenticated_blocks_seen": true,
                "authenticated_success_seen": true
            }
        })
    }

    #[test]
    fn public_access_response_classifier_accepts_access_blocks_and_rejects_origin() {
        let headers = reqwest::header::HeaderMap::new();
        validate_public_access_block_http_response(
            "/status",
            StatusCode::FORBIDDEN,
            &headers,
            b"<html>Cloudflare Access</html>",
        )
        .expect("Cloudflare-style 403 should pass");

        let mut redirect_headers = reqwest::header::HeaderMap::new();
        redirect_headers.insert(
            reqwest::header::LOCATION,
            reqwest::header::HeaderValue::from_static("/cdn-cgi/access/login/example"),
        );
        validate_public_access_block_http_response(
            "/auth/login",
            StatusCode::FOUND,
            &redirect_headers,
            b"",
        )
        .expect("Cloudflare Access redirect should pass");

        let mut basic_challenge_headers = reqwest::header::HeaderMap::new();
        basic_challenge_headers.insert(
            reqwest::header::WWW_AUTHENTICATE,
            reqwest::header::HeaderValue::from_static("Basic realm=\"Office Climate\""),
        );
        let error = validate_public_access_block_http_response(
            "/status",
            StatusCode::UNAUTHORIZED,
            &basic_challenge_headers,
            b"Authentication required",
        )
        .expect_err("Office Basic auth challenge should fail");
        assert!(
            error
                .to_string()
                .contains("reached the Office Automate origin")
        );

        let error = validate_public_access_block_http_response(
            "/status",
            StatusCode::UNAUTHORIZED,
            &headers,
            br#"{"error":"authentication required","login_url":"/auth/login"}"#,
        )
        .expect_err("Office origin 401 should fail");
        assert!(
            error
                .to_string()
                .contains("reached the Office Automate origin")
        );

        let error = validate_public_access_block_http_response(
            "/auth/login",
            StatusCode::OK,
            &headers,
            br#"{"authorization_url":"https://accounts.google.com","state":"abc"}"#,
        )
        .expect_err("Office OAuth payload should fail");
        assert!(
            error
                .to_string()
                .contains("reached the Office Automate origin")
        );
    }

    #[test]
    fn public_access_probe_inventory_covers_current_public_skip_and_sensitive_routes() {
        let paths = http::PUBLIC_ACCESS_PROBES
            .iter()
            .map(|probe| probe.path)
            .collect::<std::collections::HashSet<_>>();
        for path in [
            "/",
            "/index.html",
            "/status",
            "/auth/login",
            "/auth/callback",
            "/auth/device/start",
            "/auth/device/poll",
            "/assets/app.js",
            "/manifest.json",
            "/favicon.png",
            "/apps/office-climate/meta.json",
            "/apps/office-climate/latest.apk",
            "/apps/office-climate/00000000.apk",
            "/apk",
            "/deploy/office-climate",
        ] {
            assert!(
                paths.contains(path),
                "missing public Access probe for {path}"
            );
        }
    }

    #[test]
    fn public_access_probe_auth_requires_id_and_secret_pair() {
        assert_eq!(
            public_access_probe_auth(None, None).expect("no access token"),
            None
        );
        assert!(
            public_access_probe_auth(Some("id"), None)
                .expect_err("partial access token should fail")
                .to_string()
                .contains("requires both")
        );
        assert!(
            public_access_probe_auth(Some("id"), Some("secret"))
                .expect("access token pair")
                .is_some()
        );
    }

    #[test]
    fn websocket_url_maps_http_schemes_to_ws() {
        assert_eq!(
            websocket_url("http://127.0.0.1:9001").expect("local ws url"),
            "ws://127.0.0.1:9001/ws"
        );
        assert_eq!(
            websocket_url("https://office.example.test/").expect("public ws url"),
            "wss://office.example.test/ws"
        );
    }

    #[test]
    fn interface_probe_auth_uses_oauth_first_message_with_stable_secret() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.orchestrator.google_oauth = Some(GoogleOAuthConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            allowed_emails: vec!["Engineer@RajesHGo.li".to_string()],
            jwt_secret: Some("test-secret".to_string()),
            ..GoogleOAuthConfig::default()
        });

        let auth = interface_probe_auth(&config).expect("probe auth");
        match auth {
            InterfaceProbeAuth::OAuthFirstMessage { token, email } => {
                assert_eq!(email, "engineer@rajeshgo.li");
                assert!(!token.is_empty());
            }
            other => panic!("expected OAuth first-message auth, got {other:?}"),
        }
    }

    #[test]
    fn interface_probe_auth_uses_trusted_network_without_stable_oauth_secret() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        config.orchestrator.google_oauth = Some(GoogleOAuthConfig {
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            allowed_emails: vec!["engineer@rajeshgo.li".to_string()],
            trusted_networks: vec!["192.168.0.0/16".to_string()],
            ..GoogleOAuthConfig::default()
        });

        let auth = interface_probe_auth(&config).expect("probe auth");
        assert_eq!(
            auth,
            InterfaceProbeAuth::OAuthTrustedNetwork {
                forwarded_for: "192.168.0.0".to_string()
            }
        );
    }
}
