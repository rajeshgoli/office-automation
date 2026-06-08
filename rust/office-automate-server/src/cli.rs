use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    artifacts::{ARTIFACT_MAX_SIZE_BYTES, ArtifactStore, ArtifactUploadPolicy},
    config::AppConfig,
    db, device, edge, erv, http, hvac, migration, presence, telemetry,
    validation::{
        self, CutoverValidationOptions, MqttCutoverStrategy, MqttRollbackState,
        RestoreVerification, RollbackValidationOptions, SecurityValidationOptions,
        ShadowValidationOptions,
    },
};

#[derive(Debug, Parser)]
#[command(name = "office-automate-server")]
#[command(about = "Office Automate Rust backend")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the HTTP/API server.
    Serve(ConfigArgs),
    /// Run the quarantined public HTTP edge.
    ServeEdge(EdgeConfigArgs),
    /// Create or upgrade the SQLite schema.
    Migrate(ConfigArgs),
    /// Register a new device pairing code.
    RegisterDevice(RegisterDeviceArgs),
    /// List enrolled device registrations.
    ListDevices(ConfigArgs),
    /// Revoke an enrolled device registration.
    RevokeDevice(RevokeDeviceArgs),
    /// Mark the current app artifact revoked so clients refuse it.
    RevokeArtifact(RevokeArtifactArgs),
    /// Roll latest app artifact back to a known content-addressed APK.
    RollbackArtifact(RollbackArtifactArgs),
    /// Run local dependency checks without changing device state.
    Smoke(SmokeArgs),
    /// Run local telemetry collectors.
    Collect(CollectArgs),
    /// Create and validate a pre-cutover rollback snapshot.
    Snapshot(SnapshotArgs),
    /// Run cutover validation gates.
    Validate(ValidateArgs),
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct ConfigArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct EdgeConfigArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_EDGE_CONFIG")]
    pub config: PathBuf,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct SmokeArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[command(subcommand)]
    pub target: Option<SmokeTarget>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct CollectArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG", global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub target: CollectTarget,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RegisterDeviceArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[arg(long)]
    pub device_name: String,
    #[arg(long, default_value_t = 15)]
    pub expires_in_minutes: i64,
    #[arg(long, default_value = "0.0.0.0:19191")]
    pub listen: SocketAddr,
    #[arg(long, env = "OFFICE_AUTOMATE_DEVICE_CA_CERT")]
    pub device_ca_cert: Option<PathBuf>,
    #[arg(long, env = "OFFICE_AUTOMATE_DEVICE_CA_KEY")]
    pub device_ca_key: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RevokeDeviceArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[arg(value_name = "DEVICE_ID")]
    pub device_id: Option<String>,
    #[arg(long = "device-id")]
    pub device_id_flag: Option<String>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RevokeArtifactArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[arg(long, default_value = "office-climate")]
    pub app: String,
    #[arg(long)]
    pub reason: String,
    #[arg(long)]
    pub replacement_artifact_hash: Option<String>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RollbackArtifactArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[arg(long, default_value = "office-climate")]
    pub app: String,
    #[arg(long)]
    pub artifact_hash: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct SnapshotArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[arg(long, env = "OFFICE_AUTOMATE_SNAPSHOT_DIR")]
    pub output_dir: PathBuf,
    #[arg(long, env = "CLOUDFLARED_CONFIG")]
    pub cloudflared_config: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct ValidateArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
    #[command(subcommand)]
    pub target: ValidateTarget,
}

#[derive(Debug, Subcommand, Clone, Copy, PartialEq, Eq)]
pub enum SmokeTarget {
    /// Verify local ERV read credential and connectivity.
    Erv,
    /// Verify Mitsubishi Kumo HVAC status read.
    Hvac,
    /// Verify macOS keyboard/display presence signals.
    Presence,
}

#[derive(Debug, Subcommand, Clone, Copy, PartialEq, Eq)]
pub enum CollectTarget {
    /// Collect session-output telemetry into telemetry.db.
    Telemetry(CollectTelemetryArgs),
    /// Collect project leverage rows into office_climate.db.
    Leverage,
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub enum ValidateTarget {
    /// Validate Rust shadow mode before backend/MQTT cutover.
    Shadow(ShadowValidationArgs),
    /// Validate backend/MQTT cutover with Rust as the only active controller.
    Cutover(CutoverValidationArgs),
    /// Validate rollback from Rust active control to the legacy controller.
    Rollback(RollbackValidationArgs),
    /// Validate public security posture and edge/tunnel containment.
    Security(SecurityValidationArgs),
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct ShadowValidationArgs {
    /// Local Rust server URL, for example http://127.0.0.1:9001.
    #[arg(long, env = "OFFICE_AUTOMATE_SHADOW_BASE_URL")]
    pub base_url: Option<String>,
    /// Public Cloudflare Tunnel URL for the Rust shadow server.
    #[arg(long, env = "OFFICE_AUTOMATE_SHADOW_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Local cloudflared config to validate for exact hostname and final deny ingress.
    #[arg(long, env = "CLOUDFLARED_CONFIG")]
    pub cloudflared_config: Option<PathBuf>,
    /// Sanitized Cloudflare API/Terraform/dashboard evidence JSON.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE")]
    pub cloudflare_evidence: Option<PathBuf>,
    /// Cloudflare Access service-token client id for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID")]
    pub cloudflare_access_client_id: Option<String>,
    /// Cloudflare Access service-token client secret for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET")]
    pub cloudflare_access_client_secret: Option<String>,
    /// Manual timestamp after browser/mobile Cloudflare Access plus Office auth verification.
    #[arg(long, env = "OFFICE_AUTOMATE_MANUAL_PUBLIC_ACCESS_VERIFIED_AT")]
    pub manual_public_access_verified_at: Option<String>,
    /// Skip ERV/HVAC/YoLink live read-only checks.
    #[arg(long)]
    pub skip_live_devices: bool,
    /// Skip Rust HTTP interface parity probes.
    #[arg(long)]
    pub skip_http_interface: bool,
    /// Maximum accepted age for /status air_quality.last_update.
    #[arg(long, default_value_t = 300)]
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct CutoverValidationArgs {
    /// Local Rust server URL, for example http://127.0.0.1:9001.
    #[arg(long, env = "OFFICE_AUTOMATE_CUTOVER_BASE_URL")]
    pub base_url: Option<String>,
    /// Public Cloudflare Tunnel URL for the Rust server.
    #[arg(long, env = "OFFICE_AUTOMATE_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Optional legacy backend URL; validation fails if it still responds.
    #[arg(long, env = "OFFICE_AUTOMATE_LEGACY_BASE_URL")]
    pub legacy_base_url: Option<String>,
    /// Operator-recorded timestamp proving Python active control was stopped.
    #[arg(long, env = "OFFICE_AUTOMATE_LEGACY_STOPPED_AT")]
    pub legacy_controller_stopped_at: String,
    /// Qingping feed cutover strategy used for this window.
    #[arg(long, env = "OFFICE_AUTOMATE_MQTT_CUTOVER_STRATEGY", value_enum)]
    pub mqtt_strategy: MqttCutoverStrategyArg,
    /// Pre-cutover rollback snapshot directory from ticket #76.
    #[arg(long, env = "OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR")]
    pub snapshot_dir: PathBuf,
    /// Markdown log file to write with timestamps, checks, and rollback point.
    #[arg(long, env = "OFFICE_AUTOMATE_CUTOVER_LOG")]
    pub cutover_log: PathBuf,
    /// Manual browser/mobile Cloudflare Access plus Office auth verification timestamp.
    #[arg(long, env = "OFFICE_AUTOMATE_MANUAL_PUBLIC_OAUTH_VERIFIED_AT")]
    pub manual_public_oauth_verified_at: Option<String>,
    /// Local cloudflared config to validate for exact hostname and final deny ingress.
    #[arg(long, env = "CLOUDFLARED_CONFIG")]
    pub cloudflared_config: Option<PathBuf>,
    /// Sanitized Cloudflare API/Terraform/dashboard evidence JSON.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE")]
    pub cloudflare_evidence: Option<PathBuf>,
    /// Cloudflare Access service-token client id for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID")]
    pub cloudflare_access_client_id: Option<String>,
    /// Cloudflare Access service-token client secret for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET")]
    pub cloudflare_access_client_secret: Option<String>,
    /// Maximum accepted age for /status air_quality.last_update.
    #[arg(long, default_value_t = 300)]
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RollbackValidationArgs {
    /// Local legacy backend URL after rollback, for example http://legacy-host:9001.
    #[arg(long, env = "OFFICE_AUTOMATE_LEGACY_BASE_URL")]
    pub legacy_base_url: Option<String>,
    /// Public Cloudflare URL after rollback routes to the legacy backend.
    #[arg(long, env = "OFFICE_AUTOMATE_LEGACY_PUBLIC_URL")]
    pub legacy_public_url: Option<String>,
    /// Optional local Rust server URL; validation fails if it still responds.
    #[arg(long, env = "OFFICE_AUTOMATE_CUTOVER_BASE_URL")]
    pub rust_base_url: Option<String>,
    /// Optional primary-host public URL; validation fails if it still responds.
    #[arg(long, env = "OFFICE_AUTOMATE_RUST_PUBLIC_URL")]
    pub rust_public_url: Option<String>,
    /// Operator-recorded timestamp proving Rust active control was stopped.
    #[arg(long, env = "OFFICE_AUTOMATE_RUST_STOPPED_AT")]
    pub rust_stopped_at: String,
    /// Operator-recorded timestamp proving the legacy backend/tunnel started.
    #[arg(long, env = "OFFICE_AUTOMATE_LEGACY_STARTED_AT")]
    pub legacy_started_at: String,
    /// Qingping feed state after rollback.
    #[arg(long, env = "OFFICE_AUTOMATE_MQTT_ROLLBACK_STATE", value_enum)]
    pub mqtt_rollback_state: MqttRollbackStateArg,
    /// Pre-cutover rollback snapshot directory from ticket #76.
    #[arg(long, env = "OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR")]
    pub snapshot_dir: PathBuf,
    /// Restore verification result for copied state from the snapshot.
    #[arg(long, env = "OFFICE_AUTOMATE_RESTORE_VERIFICATION", value_enum)]
    pub restore_verification: RestoreVerificationArg,
    /// Markdown log file to write with rollback checks and restore decision.
    #[arg(long, env = "OFFICE_AUTOMATE_ROLLBACK_LOG")]
    pub rollback_log: PathBuf,
    /// Manual browser/mobile public legacy verification timestamp when no validation JWT can be minted.
    #[arg(long, env = "OFFICE_AUTOMATE_MANUAL_LEGACY_PUBLIC_VERIFIED_AT")]
    pub manual_legacy_public_verified_at: Option<String>,
    /// Maximum accepted age for legacy /status air_quality.last_update.
    #[arg(long, default_value_t = 300)]
    pub max_air_quality_age_seconds: u64,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct SecurityValidationArgs {
    /// Public Cloudflare URL for the production Office hostname.
    #[arg(long, env = "OFFICE_AUTOMATE_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Local cloudflared config to validate for exact hostname and final deny ingress.
    #[arg(long, env = "CLOUDFLARED_CONFIG")]
    pub cloudflared_config: Option<PathBuf>,
    /// Sanitized Cloudflare API/Terraform/dashboard evidence JSON.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE")]
    pub cloudflare_evidence: Option<PathBuf>,
    /// Cloudflare Access service-token client id for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID")]
    pub cloudflare_access_client_id: Option<String>,
    /// Cloudflare Access service-token client secret for authenticated public validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET")]
    pub cloudflare_access_client_secret: Option<String>,
    /// Public edge config file.
    #[arg(long, env = "OFFICE_AUTOMATE_EDGE_CONFIG")]
    pub edge_config: Option<PathBuf>,
    /// Controller launchd plist to inspect.
    #[arg(long, env = "OFFICE_AUTOMATE_SERVER_PLIST")]
    pub server_launchd_plist: Option<PathBuf>,
    /// Public edge launchd plist to inspect.
    #[arg(long, env = "OFFICE_AUTOMATE_EDGE_PLIST")]
    pub edge_launchd_plist: Option<PathBuf>,
    /// Cloudflared launchd plist to inspect.
    #[arg(long, env = "OFFICE_AUTOMATE_TUNNEL_PLIST")]
    pub tunnel_launchd_plist: Option<PathBuf>,
    /// Dedicated cloudflared tunnel user for sudo-based boundary probes.
    #[arg(long, env = "OFFICE_AUTOMATE_TUNNEL_USER")]
    pub tunnel_user: Option<String>,
    /// Dedicated public edge user for sudo-based boundary probes.
    #[arg(long, env = "OFFICE_AUTOMATE_EDGE_USER")]
    pub edge_user: Option<String>,
    /// Dedicated controller/device-ingress user for launchd and containment validation.
    #[arg(long, env = "OFFICE_AUTOMATE_CONTROLLER_USER")]
    pub controller_user: Option<String>,
    /// Additional secret/config file that must be owner-only. Repeatable.
    #[arg(long = "secret-file")]
    pub secret_files: Vec<PathBuf>,
    /// Path tunnel/edge users must not read or traverse. Repeatable.
    #[arg(long = "protected-path")]
    pub protected_paths: Vec<PathBuf>,
    /// Path tunnel user must be able to read, usually cloudflared config/creds. Repeatable.
    #[arg(long = "tunnel-readable")]
    pub tunnel_readable_paths: Vec<PathBuf>,
    /// Path edge user must not read, usually tunnel-only credentials. Repeatable.
    #[arg(long = "tunnel-private")]
    pub tunnel_private_paths: Vec<PathBuf>,
    /// Path edge user must be able to read, usually edge config/static assets. Repeatable.
    #[arg(long = "edge-readable")]
    pub edge_readable_paths: Vec<PathBuf>,
    /// Path tunnel user must not read, usually edge-only config. Repeatable.
    #[arg(long = "edge-private")]
    pub edge_private_paths: Vec<PathBuf>,
    /// Loopback origin endpoint tunnel user may reach, formatted HOST:PORT. Repeatable.
    #[arg(long = "tunnel-origin-probe")]
    pub tunnel_origin_probes: Vec<String>,
    /// Loopback controller IPC endpoint edge user may reach, formatted HOST:PORT. Repeatable.
    #[arg(long = "edge-ipc-probe")]
    pub edge_ipc_probes: Vec<String>,
    /// LAN/RFC1918 endpoint tunnel and edge users must not reach, formatted HOST:PORT. Repeatable.
    #[arg(long = "denied-lan-probe")]
    pub denied_lan_probes: Vec<String>,
    /// Optional user that must be able to reach denied LAN probes before denial checks.
    #[arg(long, env = "OFFICE_AUTOMATE_LAN_CONTROL_USER")]
    pub lan_control_user: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum MqttCutoverStrategyArg {
    AtomicSwitch,
}

impl From<MqttCutoverStrategyArg> for MqttCutoverStrategy {
    fn from(value: MqttCutoverStrategyArg) -> Self {
        match value {
            MqttCutoverStrategyArg::AtomicSwitch => Self::AtomicSwitch,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum MqttRollbackStateArg {
    /// Qingping never moved off the legacy-compatible MQTT path.
    NotMoved,
    /// Qingping device was repointed to the legacy broker.
    RepointedLegacy,
}

impl From<MqttRollbackStateArg> for MqttRollbackState {
    fn from(value: MqttRollbackStateArg) -> Self {
        match value {
            MqttRollbackStateArg::NotMoved => Self::NotMoved,
            MqttRollbackStateArg::RepointedLegacy => Self::RepointedLegacy,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum RestoreVerificationArg {
    /// State was restored from the pre-cutover snapshot.
    RestoredFromSnapshot,
    /// Rust-written state was reviewed and no restore was required.
    VerifiedSafeNoRestore,
}

impl From<RestoreVerificationArg> for RestoreVerification {
    fn from(value: RestoreVerificationArg) -> Self {
        match value {
            RestoreVerificationArg::RestoredFromSnapshot => Self::RestoredFromSnapshot,
            RestoreVerificationArg::VerifiedSafeNoRestore => Self::VerifiedSafeNoRestore,
        }
    }
}

#[derive(Debug, Args, Clone, Copy, PartialEq, Eq)]
pub struct CollectTelemetryArgs {
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run_cli() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "office_automate_server=info,tower_http=info".into()),
        )
        .init();

    run(Cli::parse()).await
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve(args) => {
            let config = AppConfig::load(&args.config)?;
            http::serve(config).await
        }
        Command::ServeEdge(args) => {
            let config = edge::PublicEdgeConfig::load(&args.config)?;
            edge::serve(config).await
        }
        Command::Migrate(args) => {
            let config = AppConfig::load(&args.config)?;
            db::migrate(&config)?;
            Ok(())
        }
        Command::RegisterDevice(args) => {
            let config = AppConfig::load(&args.config)?;
            let device = device::register_device(
                &config.runtime.database_path,
                &args.device_name,
                args.expires_in_minutes,
            )?;
            println!(
                "device registered: device_id={} pairing_code={} expires_at={} listen={}",
                device.device_id, device.pairing_code, device.expires_at, args.listen
            );
            device::serve_pairing_listener(
                config.runtime.database_path,
                args.listen,
                args.device_ca_cert,
                args.device_ca_key,
                config.cloudflare_access,
            )
            .await
        }
        Command::ListDevices(args) => {
            let config = AppConfig::load(&args.config)?;
            let devices = device::list_devices(&config.runtime.database_path)?;
            if devices.is_empty() {
                println!("no device registrations found");
            } else {
                for device in devices {
                    println!(
                        "device_id={} name={} code={} paired_at={} revoked_at={} expires_at={} fingerprint={} failed_attempts={}/{} last_failed_at={}",
                        device.device_id,
                        device.device_name,
                        device.pairing_code,
                        device.paired_at.as_deref().unwrap_or("-"),
                        device.revoked_at.as_deref().unwrap_or("-"),
                        device.expires_at,
                        device.public_key_fingerprint.as_deref().unwrap_or("-"),
                        device.failed_attempts,
                        db::DEVICE_PAIRING_MAX_FAILED_ATTEMPTS,
                        device.last_failed_attempt_at.as_deref().unwrap_or("-"),
                    );
                }
            }
            Ok(())
        }
        Command::RevokeDevice(args) => {
            let config = AppConfig::load(&args.config)?;
            let device_id = args
                .device_id
                .or(args.device_id_flag)
                .context("revoke-device requires DEVICE_ID or --device-id")?;
            let revoked = device::revoke_device(
                &config.runtime.database_path,
                &config.cloudflare_access,
                &device_id,
            )
            .await?;
            if revoked {
                println!("device revoked: {device_id}");
            } else {
                println!("device not found or already revoked: {device_id}");
            }
            Ok(())
        }
        Command::RevokeArtifact(args) => {
            let config = AppConfig::load(&args.config)?;
            let store = artifact_store(&config);
            match store
                .revoke_current(
                    &args.app,
                    &args.reason,
                    args.replacement_artifact_hash.as_deref(),
                )
                .await?
            {
                Some(metadata) => {
                    println!(
                        "artifact revoked: app={} artifact_hash={} revoked_at={} replacement={}",
                        args.app,
                        metadata.artifact_hash,
                        metadata.revoked_at.as_deref().unwrap_or("-"),
                        metadata.replacement_artifact_hash.as_deref().unwrap_or("-")
                    );
                }
                None => println!("artifact metadata not found: app={}", args.app),
            }
            Ok(())
        }
        Command::RollbackArtifact(args) => {
            let config = AppConfig::load(&args.config)?;
            let store = artifact_store(&config);
            match store
                .rollback_to(
                    &args.app,
                    &args.artifact_hash,
                    &args.reason,
                    "local_operator",
                )
                .await?
            {
                Some(metadata) => println!(
                    "artifact rolled back: app={} artifact_hash={} previous={}",
                    args.app,
                    metadata.artifact_hash,
                    metadata
                        .rolled_back_from_artifact_hash
                        .as_deref()
                        .unwrap_or("-")
                ),
                None => println!(
                    "rollback artifact not found: app={} artifact_hash={}",
                    args.app, args.artifact_hash
                ),
            }
            Ok(())
        }
        Command::Smoke(args) => {
            let config = AppConfig::load(&args.config)?;
            run_smoke(&config, args.target).await
        }
        Command::Collect(args) => {
            let config_path = args
                .config
                .context("collect requires --config or OFFICE_AUTOMATE_CONFIG")?;
            let config = AppConfig::load(&config_path)?;
            run_collect(&config, args.target)
        }
        Command::Snapshot(args) => {
            let config = AppConfig::load(&args.config)?;
            let report = migration::create_pre_cutover_snapshot(
                &config,
                &args.config,
                &args.output_dir,
                args.cloudflared_config.as_deref(),
            )?;
            println!(
                "Pre-cutover snapshot complete: snapshot_dir={} files_copied={} validations={}",
                report.snapshot_dir.display(),
                report.files_copied,
                report.validations.len()
            );
            Ok(())
        }
        Command::Validate(args) => {
            let config = AppConfig::load(&args.config)?;
            run_validate(&config, args.target).await
        }
    }
}

fn artifact_store(config: &AppConfig) -> ArtifactStore {
    ArtifactStore::with_upload_policy(
        config.runtime.artifacts_dir.clone(),
        config.runtime.legacy_apk_path.clone(),
        ARTIFACT_MAX_SIZE_BYTES,
        ArtifactUploadPolicy {
            expected_office_climate_signing_cert_sha256: config
                .artifacts
                .office_climate_signing_cert_sha256
                .clone(),
            apksigner_path: config.artifacts.apksigner_path.clone(),
        },
    )
}

async fn run_smoke(config: &AppConfig, target: Option<SmokeTarget>) -> Result<()> {
    for smoke_target in smoke_targets(target) {
        match smoke_target {
            SmokeTarget::Erv => {
                let status = erv::smoke_erv(config).await?;
                println!(
                    "ERV local status OK: running={} speed={}",
                    status.power,
                    status
                        .fan_speed
                        .map(|speed| speed.as_str())
                        .unwrap_or("unknown")
                );
            }
            SmokeTarget::Hvac => {
                let status = hvac::smoke_hvac(config).await?;
                println!(
                    "HVAC Kumo status OK: mode={} setpoint_c={:.1}",
                    status.mode, status.setpoint_c
                );
            }
            SmokeTarget::Presence => {
                let status = presence::smoke_presence(config).await?;
                println!(
                    "Presence signals OK: idle_seconds={:.1} external_monitor={} display_count={}",
                    status.idle_seconds, status.external_monitor, status.display_count
                );
            }
        }
    }

    Ok(())
}

fn smoke_targets(target: Option<SmokeTarget>) -> &'static [SmokeTarget] {
    match target {
        Some(SmokeTarget::Erv) => &[SmokeTarget::Erv],
        Some(SmokeTarget::Hvac) => &[SmokeTarget::Hvac],
        Some(SmokeTarget::Presence) => &[SmokeTarget::Presence],
        None => &[SmokeTarget::Erv, SmokeTarget::Hvac, SmokeTarget::Presence],
    }
}

fn run_collect(config: &AppConfig, target: CollectTarget) -> Result<()> {
    match target {
        CollectTarget::Telemetry(args) => {
            let stats = telemetry::collect_telemetry(config, args.dry_run)?;
            println!(
                "Telemetry collection complete: sessions={} rows_written={} synthetic_rows={} matched_commits={}",
                stats.sessions, stats.rows_written, stats.synthetic_rows, stats.matched_commits
            );
        }
        CollectTarget::Leverage => {
            let rows = telemetry::collect_project_leverage(config)?;
            println!("Project leverage collection complete: rows_written={rows}");
        }
    }
    Ok(())
}

async fn run_validate(config: &AppConfig, target: ValidateTarget) -> Result<()> {
    match target {
        ValidateTarget::Shadow(args) => {
            let report = validation::run_shadow_validation(
                config,
                ShadowValidationOptions {
                    base_url: args.base_url,
                    public_url: args.public_url,
                    cloudflared_config: args.cloudflared_config,
                    cloudflare_evidence: args.cloudflare_evidence,
                    cloudflare_access_client_id: args.cloudflare_access_client_id,
                    cloudflare_access_client_secret: args.cloudflare_access_client_secret,
                    manual_public_access_verified_at: args.manual_public_access_verified_at,
                    skip_live_devices: args.skip_live_devices,
                    skip_http_interface: args.skip_http_interface,
                    max_air_quality_age_seconds: args.max_air_quality_age_seconds,
                },
            )
            .await?;
            println!("Shadow validation complete: checks={}", report.len());
            for check in report.checks {
                println!("- {:?}: {} - {}", check.status, check.name, check.detail);
            }
        }
        ValidateTarget::Cutover(args) => {
            let report = validation::run_cutover_validation(
                config,
                CutoverValidationOptions {
                    base_url: args.base_url,
                    public_url: args.public_url,
                    legacy_base_url: args.legacy_base_url,
                    legacy_controller_stopped_at: args.legacy_controller_stopped_at,
                    mqtt_strategy: args.mqtt_strategy.into(),
                    snapshot_dir: args.snapshot_dir,
                    cutover_log: args.cutover_log,
                    manual_public_oauth_verified_at: args.manual_public_oauth_verified_at,
                    cloudflared_config: args.cloudflared_config,
                    cloudflare_evidence: args.cloudflare_evidence,
                    cloudflare_access_client_id: args.cloudflare_access_client_id,
                    cloudflare_access_client_secret: args.cloudflare_access_client_secret,
                    max_air_quality_age_seconds: args.max_air_quality_age_seconds,
                },
            )
            .await?;
            println!("Cutover validation complete: checks={}", report.len());
            for check in report.checks {
                println!("- {:?}: {} - {}", check.status, check.name, check.detail);
            }
        }
        ValidateTarget::Rollback(args) => {
            let report = validation::run_rollback_validation(
                config,
                RollbackValidationOptions {
                    legacy_base_url: args.legacy_base_url,
                    legacy_public_url: args.legacy_public_url,
                    rust_base_url: args.rust_base_url,
                    rust_public_url: args.rust_public_url,
                    rust_stopped_at: args.rust_stopped_at,
                    legacy_started_at: args.legacy_started_at,
                    mqtt_rollback_state: args.mqtt_rollback_state.into(),
                    snapshot_dir: args.snapshot_dir,
                    restore_verification: args.restore_verification.into(),
                    rollback_log: args.rollback_log,
                    manual_legacy_public_verified_at: args.manual_legacy_public_verified_at,
                    max_air_quality_age_seconds: args.max_air_quality_age_seconds,
                },
            )
            .await?;
            println!("Rollback validation complete: checks={}", report.len());
            for check in report.checks {
                println!("- {:?}: {} - {}", check.status, check.name, check.detail);
            }
        }
        ValidateTarget::Security(args) => {
            let report = validation::run_security_validation(
                config,
                SecurityValidationOptions {
                    public_url: args.public_url,
                    cloudflared_config: args.cloudflared_config,
                    cloudflare_evidence: args.cloudflare_evidence,
                    cloudflare_access_client_id: args.cloudflare_access_client_id,
                    cloudflare_access_client_secret: args.cloudflare_access_client_secret,
                    edge_config: args.edge_config,
                    server_launchd_plist: args.server_launchd_plist,
                    edge_launchd_plist: args.edge_launchd_plist,
                    tunnel_launchd_plist: args.tunnel_launchd_plist,
                    tunnel_user: args.tunnel_user,
                    edge_user: args.edge_user,
                    controller_user: args.controller_user,
                    secret_files: args.secret_files,
                    protected_paths: args.protected_paths,
                    tunnel_readable_paths: args.tunnel_readable_paths,
                    tunnel_private_paths: args.tunnel_private_paths,
                    edge_readable_paths: args.edge_readable_paths,
                    edge_private_paths: args.edge_private_paths,
                    tunnel_origin_probes: args.tunnel_origin_probes,
                    edge_ipc_probes: args.edge_ipc_probes,
                    denied_lan_probes: args.denied_lan_probes,
                    lan_control_user: args.lan_control_user,
                },
            )
            .await?;
            println!("Security validation complete: checks={}", report.len());
            for check in report.checks {
                println!("- {:?}: {} - {}", check.status, check.name, check.detail);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_serve_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "serve",
            "--config",
            "/tmp/office.yaml",
        ])
        .expect("serve command should parse");

        match cli.command {
            Command::Serve(args) => assert_eq!(args.config, PathBuf::from("/tmp/office.yaml")),
            other => panic!("expected serve command, got {other:?}"),
        }
    }

    #[test]
    fn parses_serve_edge_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "serve-edge",
            "--config",
            "/tmp/office-edge.yaml",
        ])
        .expect("serve-edge command should parse");

        match cli.command {
            Command::ServeEdge(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office-edge.yaml"));
            }
            other => panic!("expected serve-edge command, got {other:?}"),
        }
    }

    #[test]
    fn parses_migrate_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "migrate",
            "--config",
            "/tmp/office.yaml",
        ])
        .expect("migrate command should parse");

        match cli.command {
            Command::Migrate(args) => assert_eq!(args.config, PathBuf::from("/tmp/office.yaml")),
            other => panic!("expected migrate command, got {other:?}"),
        }
    }

    #[test]
    fn parses_register_device_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "register-device",
            "--config",
            "/tmp/office.yaml",
            "--device-name",
            "phone",
        ])
        .expect("register-device command should parse");

        match cli.command {
            Command::RegisterDevice(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.device_name, "phone");
                assert_eq!(args.expires_in_minutes, 15);
            }
            other => panic!("expected register-device command, got {other:?}"),
        }
    }

    #[test]
    fn parses_revoke_device_positional_and_flag_forms() {
        let positional = Cli::try_parse_from([
            "office-automate-server",
            "revoke-device",
            "--config",
            "/tmp/office.yaml",
            "device-1",
        ])
        .expect("revoke-device positional command should parse");

        match positional.command {
            Command::RevokeDevice(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.device_id.as_deref(), Some("device-1"));
                assert_eq!(args.device_id_flag, None);
            }
            other => panic!("expected revoke-device command, got {other:?}"),
        }

        let flagged = Cli::try_parse_from([
            "office-automate-server",
            "revoke-device",
            "--config",
            "/tmp/office.yaml",
            "--device-id",
            "device-2",
        ])
        .expect("revoke-device flag command should parse");

        match flagged.command {
            Command::RevokeDevice(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.device_id, None);
                assert_eq!(args.device_id_flag.as_deref(), Some("device-2"));
            }
            other => panic!("expected revoke-device command, got {other:?}"),
        }
    }

    #[test]
    fn parses_smoke_erv_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "smoke",
            "--config",
            "/tmp/office.yaml",
            "erv",
        ])
        .expect("smoke command should parse");

        match cli.command {
            Command::Smoke(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.target, Some(SmokeTarget::Erv));
            }
            other => panic!("expected smoke command, got {other:?}"),
        }
    }

    #[test]
    fn parses_smoke_hvac_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "smoke",
            "--config",
            "/tmp/office.yaml",
            "hvac",
        ])
        .expect("smoke command should parse");

        match cli.command {
            Command::Smoke(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.target, Some(SmokeTarget::Hvac));
            }
            other => panic!("expected smoke command, got {other:?}"),
        }
    }

    #[test]
    fn bare_smoke_runs_all_dependency_checks() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "smoke",
            "--config",
            "/tmp/office.yaml",
        ])
        .expect("smoke command should parse");

        match cli.command {
            Command::Smoke(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(
                    smoke_targets(args.target),
                    &[SmokeTarget::Erv, SmokeTarget::Hvac, SmokeTarget::Presence]
                );
            }
            other => panic!("expected smoke command, got {other:?}"),
        }
    }

    #[test]
    fn parses_smoke_presence_command_with_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "smoke",
            "--config",
            "/tmp/office.yaml",
            "presence",
        ])
        .expect("smoke command should parse");

        match cli.command {
            Command::Smoke(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.target, Some(SmokeTarget::Presence));
            }
            other => panic!("expected smoke command, got {other:?}"),
        }
    }

    #[test]
    fn parses_collect_telemetry_command_with_dry_run() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "collect",
            "--config",
            "/tmp/office.yaml",
            "telemetry",
            "--dry-run",
        ])
        .expect("collect command should parse");

        match cli.command {
            Command::Collect(args) => {
                assert_eq!(args.config, Some(PathBuf::from("/tmp/office.yaml")));
                assert_eq!(
                    args.target,
                    CollectTarget::Telemetry(CollectTelemetryArgs { dry_run: true })
                );
            }
            other => panic!("expected collect command, got {other:?}"),
        }
    }

    #[test]
    fn parses_collect_telemetry_command_with_config_after_target() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "collect",
            "telemetry",
            "--config",
            "/tmp/office.yaml",
            "--dry-run",
        ])
        .expect("collect command should parse");

        match cli.command {
            Command::Collect(args) => {
                assert_eq!(args.config, Some(PathBuf::from("/tmp/office.yaml")));
                assert_eq!(
                    args.target,
                    CollectTarget::Telemetry(CollectTelemetryArgs { dry_run: true })
                );
            }
            other => panic!("expected collect command, got {other:?}"),
        }
    }

    #[test]
    fn parses_collect_leverage_command() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "collect",
            "--config",
            "/tmp/office.yaml",
            "leverage",
        ])
        .expect("collect command should parse");

        match cli.command {
            Command::Collect(args) => {
                assert_eq!(args.config, Some(PathBuf::from("/tmp/office.yaml")));
                assert_eq!(args.target, CollectTarget::Leverage);
            }
            other => panic!("expected collect command, got {other:?}"),
        }
    }

    #[test]
    fn parses_snapshot_command_with_output_dir() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "snapshot",
            "--config",
            "/tmp/office.yaml",
            "--output-dir",
            "/tmp/snapshots",
        ])
        .expect("snapshot command should parse");

        match cli.command {
            Command::Snapshot(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.output_dir, PathBuf::from("/tmp/snapshots"));
                assert_eq!(args.cloudflared_config, None);
            }
            other => panic!("expected snapshot command, got {other:?}"),
        }
    }

    #[test]
    fn parses_snapshot_command_with_cloudflared_config() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "snapshot",
            "--config",
            "/tmp/office.yaml",
            "--output-dir",
            "/tmp/snapshots",
            "--cloudflared-config",
            "/tmp/cloudflared/config.yml",
        ])
        .expect("snapshot command should parse");

        match cli.command {
            Command::Snapshot(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                assert_eq!(args.output_dir, PathBuf::from("/tmp/snapshots"));
                assert_eq!(
                    args.cloudflared_config,
                    Some(PathBuf::from("/tmp/cloudflared/config.yml"))
                );
            }
            other => panic!("expected snapshot command, got {other:?}"),
        }
    }

    #[test]
    fn parses_validate_shadow_command() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "validate",
            "--config",
            "/tmp/office.yaml",
            "shadow",
            "--base-url",
            "http://127.0.0.1:9001",
            "--public-url",
            "https://office.example.test",
            "--cloudflared-config",
            "/tmp/cloudflared.yml",
            "--cloudflare-evidence",
            "/tmp/cloudflare-evidence.json",
            "--cloudflare-access-client-id",
            "access-id",
            "--cloudflare-access-client-secret",
            "access-secret",
            "--manual-public-access-verified-at",
            "2026-06-06T12:00:00-07:00",
            "--max-air-quality-age-seconds",
            "120",
        ])
        .expect("validate shadow command should parse");

        match cli.command {
            Command::Validate(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                match args.target {
                    ValidateTarget::Shadow(shadow) => {
                        assert_eq!(shadow.base_url.as_deref(), Some("http://127.0.0.1:9001"));
                        assert_eq!(
                            shadow.public_url.as_deref(),
                            Some("https://office.example.test")
                        );
                        assert_eq!(
                            shadow.cloudflared_config,
                            Some(PathBuf::from("/tmp/cloudflared.yml"))
                        );
                        assert_eq!(
                            shadow.cloudflare_evidence,
                            Some(PathBuf::from("/tmp/cloudflare-evidence.json"))
                        );
                        assert_eq!(
                            shadow.cloudflare_access_client_id.as_deref(),
                            Some("access-id")
                        );
                        assert_eq!(
                            shadow.cloudflare_access_client_secret.as_deref(),
                            Some("access-secret")
                        );
                        assert_eq!(
                            shadow.manual_public_access_verified_at.as_deref(),
                            Some("2026-06-06T12:00:00-07:00")
                        );
                        assert_eq!(shadow.max_air_quality_age_seconds, 120);
                    }
                    other => panic!("expected shadow validation target, got {other:?}"),
                }
            }
            other => panic!("expected validate command, got {other:?}"),
        }
    }

    #[test]
    fn parses_validate_cutover_command() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "validate",
            "--config",
            "/tmp/office.yaml",
            "cutover",
            "--base-url",
            "http://127.0.0.1:9001",
            "--public-url",
            "https://office.example.test",
            "--legacy-controller-stopped-at",
            "2026-06-06T03:00:00-07:00",
            "--mqtt-strategy",
            "atomic-switch",
            "--snapshot-dir",
            "/tmp/snapshot",
            "--cutover-log",
            "/tmp/cutover.md",
            "--cloudflared-config",
            "/tmp/cloudflared.yml",
            "--cloudflare-evidence",
            "/tmp/cloudflare-evidence.json",
            "--cloudflare-access-client-id",
            "access-id",
            "--cloudflare-access-client-secret",
            "access-secret",
        ])
        .expect("validate cutover command should parse");

        match cli.command {
            Command::Validate(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                match args.target {
                    ValidateTarget::Cutover(cutover) => {
                        assert_eq!(cutover.base_url.as_deref(), Some("http://127.0.0.1:9001"));
                        assert_eq!(
                            cutover.public_url.as_deref(),
                            Some("https://office.example.test")
                        );
                        assert_eq!(cutover.mqtt_strategy, MqttCutoverStrategyArg::AtomicSwitch);
                        assert_eq!(cutover.snapshot_dir, PathBuf::from("/tmp/snapshot"));
                        assert_eq!(cutover.cutover_log, PathBuf::from("/tmp/cutover.md"));
                        assert_eq!(
                            cutover.cloudflared_config,
                            Some(PathBuf::from("/tmp/cloudflared.yml"))
                        );
                        assert_eq!(
                            cutover.cloudflare_evidence,
                            Some(PathBuf::from("/tmp/cloudflare-evidence.json"))
                        );
                        assert_eq!(
                            cutover.cloudflare_access_client_id.as_deref(),
                            Some("access-id")
                        );
                        assert_eq!(
                            cutover.cloudflare_access_client_secret.as_deref(),
                            Some("access-secret")
                        );
                    }
                    other => panic!("expected cutover validation target, got {other:?}"),
                }
            }
            other => panic!("expected validate command, got {other:?}"),
        }
    }

    #[test]
    fn parses_validate_rollback_command() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "validate",
            "--config",
            "/tmp/office.yaml",
            "rollback",
            "--legacy-base-url",
            "http://legacy-host:9001",
            "--legacy-public-url",
            "https://office.example.test",
            "--rust-base-url",
            "http://127.0.0.1:9001",
            "--rust-stopped-at",
            "2026-06-06T04:00:00-07:00",
            "--legacy-started-at",
            "2026-06-06T04:05:00-07:00",
            "--mqtt-rollback-state",
            "repointed-legacy",
            "--snapshot-dir",
            "/tmp/snapshot",
            "--restore-verification",
            "restored-from-snapshot",
            "--rollback-log",
            "/tmp/rollback.md",
            "--max-air-quality-age-seconds",
            "120",
        ])
        .expect("validate rollback command should parse");

        match cli.command {
            Command::Validate(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                match args.target {
                    ValidateTarget::Rollback(rollback) => {
                        assert_eq!(
                            rollback.legacy_base_url.as_deref(),
                            Some("http://legacy-host:9001")
                        );
                        assert_eq!(
                            rollback.legacy_public_url.as_deref(),
                            Some("https://office.example.test")
                        );
                        assert_eq!(
                            rollback.rust_base_url.as_deref(),
                            Some("http://127.0.0.1:9001")
                        );
                        assert_eq!(
                            rollback.mqtt_rollback_state,
                            MqttRollbackStateArg::RepointedLegacy
                        );
                        assert_eq!(
                            rollback.restore_verification,
                            RestoreVerificationArg::RestoredFromSnapshot
                        );
                        assert_eq!(rollback.snapshot_dir, PathBuf::from("/tmp/snapshot"));
                        assert_eq!(rollback.rollback_log, PathBuf::from("/tmp/rollback.md"));
                        assert_eq!(rollback.max_air_quality_age_seconds, 120);
                    }
                    other => panic!("expected rollback validation target, got {other:?}"),
                }
            }
            other => panic!("expected validate command, got {other:?}"),
        }
    }

    #[test]
    fn parses_validate_security_command() {
        let cli = Cli::try_parse_from([
            "office-automate-server",
            "validate",
            "--config",
            "/tmp/office.yaml",
            "security",
            "--public-url",
            "https://office.example.test",
            "--cloudflared-config",
            "/tmp/cloudflared.yml",
            "--cloudflare-evidence",
            "/tmp/cloudflare-evidence.json",
            "--cloudflare-access-client-id",
            "access-client-id",
            "--cloudflare-access-client-secret",
            "access-client-secret",
            "--edge-config",
            "/tmp/edge.yaml",
            "--server-launchd-plist",
            "/tmp/server.plist",
            "--edge-launchd-plist",
            "/tmp/edge.plist",
            "--tunnel-launchd-plist",
            "/tmp/tunnel.plist",
            "--tunnel-user",
            "_office_tunnel",
            "--edge-user",
            "_office_edge",
            "--secret-file",
            "/tmp/secret.yaml",
            "--protected-path",
            "/tmp/controller-data",
            "--tunnel-readable",
            "/tmp/tunnel-credentials.json",
            "--tunnel-private",
            "/tmp/tunnel-credentials.json",
            "--edge-readable",
            "/tmp/edge.yaml",
            "--edge-private",
            "/tmp/edge.yaml",
            "--tunnel-origin-probe",
            "127.0.0.1:19190",
            "--edge-ipc-probe",
            "127.0.0.1:19191",
            "--denied-lan-probe",
            "192.168.5.1:80",
            "--lan-control-user",
            "rajesh",
        ])
        .expect("validate security command should parse");

        match cli.command {
            Command::Validate(args) => {
                assert_eq!(args.config, PathBuf::from("/tmp/office.yaml"));
                match args.target {
                    ValidateTarget::Security(security) => {
                        assert_eq!(
                            security.public_url.as_deref(),
                            Some("https://office.example.test")
                        );
                        assert_eq!(
                            security.cloudflared_config,
                            Some(PathBuf::from("/tmp/cloudflared.yml"))
                        );
                        assert_eq!(
                            security.cloudflare_evidence,
                            Some(PathBuf::from("/tmp/cloudflare-evidence.json"))
                        );
                        assert_eq!(
                            security.cloudflare_access_client_id.as_deref(),
                            Some("access-client-id")
                        );
                        assert_eq!(
                            security.cloudflare_access_client_secret.as_deref(),
                            Some("access-client-secret")
                        );
                        assert_eq!(security.edge_config, Some(PathBuf::from("/tmp/edge.yaml")));
                        assert_eq!(security.tunnel_user.as_deref(), Some("_office_tunnel"));
                        assert_eq!(security.edge_user.as_deref(), Some("_office_edge"));
                        assert_eq!(
                            security.tunnel_origin_probes,
                            vec!["127.0.0.1:19190".to_string()]
                        );
                        assert_eq!(
                            security.edge_ipc_probes,
                            vec!["127.0.0.1:19191".to_string()]
                        );
                        assert_eq!(
                            security.denied_lan_probes,
                            vec!["192.168.5.1:80".to_string()]
                        );
                        assert_eq!(security.lan_control_user.as_deref(), Some("rajesh"));
                    }
                    other => panic!("expected security validation target, got {other:?}"),
                }
            }
            other => panic!("expected validate command, got {other:?}"),
        }
    }
}
