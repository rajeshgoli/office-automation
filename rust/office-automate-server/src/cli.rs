use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{config::AppConfig, db, http};

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
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct ConfigArgs {
    #[arg(long, env = "OFFICE_AUTOMATE_CONFIG")]
    pub config: PathBuf,
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
}
