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
use serde::Serialize;
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message as TungsteniteMessage,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};

use crate::{
    artifacts::is_valid_artifact_hash,
    auth::AuthManager,
    config::AppConfig,
    db, erv, hvac,
    state::StateMachine,
    yolink::{self, YoLinkCloudClient, YoLinkState},
};

const INTERFACE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowValidationOptions {
    pub base_url: Option<String>,
    pub public_url: Option<String>,
    pub skip_live_devices: bool,
    pub skip_http_interface: bool,
    pub max_air_quality_age_seconds: u64,
}

impl Default for ShadowValidationOptions {
    fn default() -> Self {
        Self {
            base_url: None,
            public_url: None,
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
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttCutoverStrategy {
    BridgeMirror,
    AtomicSwitch,
}

impl MqttCutoverStrategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::BridgeMirror => "bridge-mirror",
            Self::AtomicSwitch => "atomic-switch",
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

    validate_cutover_active_write_gates(config, &mut report)?;
    validate_cutover_snapshot(&options.snapshot_dir, &mut report)?;
    validate_legacy_controller_stopped(&options, &mut report).await?;
    validate_mqtt_cutover_strategy(config, options.mqtt_strategy, &mut report)?;
    validate_live_devices(config, &mut report).await?;
    validate_cutover_http_interfaces(config, &options, &mut report).await?;
    write_cutover_log(config, &options, &report)?;

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
    strategy: MqttCutoverStrategy,
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

    let detail = match strategy {
        MqttCutoverStrategy::BridgeMirror => {
            "bridge/mirror strategy recorded; active controller must continue receiving mirrored fresh readings"
        }
        MqttCutoverStrategy::AtomicSwitch => {
            "atomic switch strategy recorded; Qingping feed moves in the same window as active-controller cutover"
        }
    };
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
        if auth.supports_public_http_auth() {
            let public_status = get_json(&client, public_url, "/status", Some(&auth)).await?;
            validate_status_shape(&public_status)?;
            report.push_pass(
                "cloudflare-public-status",
                format!("public URL returned authenticated /status through Cloudflare Tunnel: {public_url}"),
            );
        } else {
            report.push_skip(
                "cloudflare-public-status",
                "OAuth config has no jwt_secret, so protected public /status cannot be authenticated non-interactively; validate browser/mobile auth manually",
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
    validate_public_oauth_login(config, &client, public_url, report).await?;

    if auth.supports_public_http_auth() {
        let public_status = get_json(&client, public_url, "/status", Some(&auth)).await?;
        validate_status_shape(&public_status)?;
        validate_fresh_air_quality(&public_status, options.max_air_quality_age_seconds)?;
        report.push_pass(
            "cloudflare-public-status",
            format!("public URL returned authenticated fresh /status through Cloudflare Tunnel: {public_url}"),
        );
    } else if let Some(verified_at) = options.manual_public_oauth_verified_at.as_deref() {
        if verified_at.trim().is_empty() {
            bail!("manual public OAuth verification timestamp cannot be empty");
        }
        report.push_pass(
            "cloudflare-public-status",
            format!(
                "manual browser/mobile OAuth verification recorded at {verified_at}; validation token unavailable"
            ),
        );
    } else {
        bail!(
            "OAuth config has no jwt_secret, so protected public /status cannot be authenticated non-interactively; supply --manual-public-oauth-verified-at after browser/mobile verification"
        );
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
        bail!("cutover validation requires Google OAuth config for the Cloudflare public URL");
    }

    let url = join_url(public_url, "/auth/login")?;
    let response = client
        .get(url)
        .send()
        .await
        .context("failed to call public OAuth login endpoint")?;
    if response.status() != StatusCode::OK {
        bail!("public OAuth login endpoint returned {}", response.status());
    }
    let payload: Value = response
        .json()
        .await
        .context("failed to parse public OAuth login payload")?;
    if !payload
        .get("authorization_url")
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with("https://"))
        || !payload.get("state").and_then(Value::as_str).is_some()
    {
        bail!("public OAuth login payload missing authorization_url or state");
    }
    report.push_pass(
        "cloudflare-oauth-login",
        format!("/auth/login returned OAuth start payload through {public_url}"),
    );
    Ok(())
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
            report.push_pass(
                "artifact-interface",
                format!("office-climate metadata exists with artifact_hash={hash}"),
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
    let updated_at = NaiveDateTime::parse_from_str(last_update, "%Y-%m-%dT%H:%M:%S")
        .with_context(|| format!("invalid air_quality.last_update {last_update:?}"))?;
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
            max_air_quality_age_seconds: 300,
        };
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error = validate_legacy_controller_stopped(&options, &mut report)
            .await
            .expect_err("empty timestamp should fail");
        assert!(error.to_string().contains("legacy-controller-stopped-at"));
    }

    #[test]
    fn cutover_validation_records_mqtt_strategy() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let mut config = test_config(&db_path);
        let mut report = ShadowValidationReport { checks: Vec::new() };

        let error =
            validate_mqtt_cutover_strategy(&config, MqttCutoverStrategy::BridgeMirror, &mut report)
                .expect_err("missing device mac should fail");
        assert!(error.to_string().contains("qingping.device_mac"));

        config.qingping.device_mac = Some("AA:BB:CC:DD:EE:FF".to_string());
        validate_mqtt_cutover_strategy(&config, MqttCutoverStrategy::BridgeMirror, &mut report)
            .expect("strategy passes");
        let detail = report
            .checks
            .iter()
            .find(|check| check.name == "mqtt-feed-strategy")
            .expect("mqtt check")
            .detail
            .as_str();
        assert!(detail.contains("bridge/mirror strategy"));
        assert!(detail.contains("127.0.0.1:1883"));
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
