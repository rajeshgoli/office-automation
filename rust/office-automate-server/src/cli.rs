use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    config::AppConfig,
    db, erv, http, hvac, migration, presence, telemetry,
    validation::{
        self, CutoverValidationOptions, MqttCutoverStrategy, MqttRollbackState,
        RestoreVerification, RollbackValidationOptions, ShadowValidationOptions,
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
    /// Create or upgrade the SQLite schema.
    Migrate(ConfigArgs),
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

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum MqttCutoverStrategyArg {
    BridgeMirror,
    AtomicSwitch,
}

impl From<MqttCutoverStrategyArg> for MqttCutoverStrategy {
    fn from(value: MqttCutoverStrategyArg) -> Self {
        match value {
            MqttCutoverStrategyArg::BridgeMirror => Self::BridgeMirror,
            MqttCutoverStrategyArg::AtomicSwitch => Self::AtomicSwitch,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum MqttRollbackStateArg {
    /// Qingping never moved off the legacy-compatible MQTT path.
    NotMoved,
    /// Qingping device or bridge was repointed to the legacy broker.
    RepointedLegacy,
    /// Bridge/mirror forwarding keeps the legacy controller receiving fresh reports.
    LegacyMirror,
}

impl From<MqttRollbackStateArg> for MqttRollbackState {
    fn from(value: MqttRollbackStateArg) -> Self {
        match value {
            MqttRollbackStateArg::NotMoved => Self::NotMoved,
            MqttRollbackStateArg::RepointedLegacy => Self::RepointedLegacy,
            MqttRollbackStateArg::LegacyMirror => Self::LegacyMirror,
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
        Command::Migrate(args) => {
            let config = AppConfig::load(&args.config)?;
            db::migrate(&config)?;
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
}
