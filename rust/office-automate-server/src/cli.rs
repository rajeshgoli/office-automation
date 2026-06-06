use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::{config::AppConfig, db, erv, http, hvac, presence, telemetry};

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
}
