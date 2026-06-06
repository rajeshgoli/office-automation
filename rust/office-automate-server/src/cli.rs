use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{config::AppConfig, db, erv, http, hvac};

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

#[derive(Debug, Subcommand, Clone, Copy, PartialEq, Eq)]
pub enum SmokeTarget {
    /// Verify local ERV read credential and connectivity.
    Erv,
    /// Verify Mitsubishi Kumo HVAC status read.
    Hvac,
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
        }
    }

    Ok(())
}

fn smoke_targets(target: Option<SmokeTarget>) -> &'static [SmokeTarget] {
    match target {
        Some(SmokeTarget::Erv) => &[SmokeTarget::Erv],
        Some(SmokeTarget::Hvac) => &[SmokeTarget::Hvac],
        None => &[SmokeTarget::Erv, SmokeTarget::Hvac],
    }
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
                    &[SmokeTarget::Erv, SmokeTarget::Hvac]
                );
            }
            other => panic!("expected smoke command, got {other:?}"),
        }
    }
}
