use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    config::{AppConfig, BlindsConfig, SmartLifeConfig},
    smart_life::{SmartLifeClient, auth_file_or_default},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlindsCommand {
    Open,
    Close,
}

impl BlindsCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            _ => None,
        }
    }

    fn tuya_value<'a>(self, config: &'a BlindsConfig) -> &'a str {
        match self {
            Self::Open => &config.open_value,
            Self::Close => &config.close_value,
        }
    }

    fn scene_id<'a>(self, config: &'a BlindsConfig) -> Option<&'a str> {
        match self {
            Self::Open => config.open_scene_id.as_deref(),
            Self::Close => config.close_scene_id.as_deref(),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }
}

pub async fn set_blinds(config: &AppConfig, command: BlindsCommand) -> Result<()> {
    let blinds = &config.blinds;
    if !blinds.active_control_enabled {
        bail!("Blinds active control is disabled");
    }
    if !blinds.is_configured() {
        bail!("Blinds config is incomplete");
    }

    if blinds.smart_life_scene_configured() {
        trigger_smart_life_scene(blinds, &config.smart_life, command).await
    } else {
        set_local_tuya_blinds(blinds, command).await
    }
}

async fn set_local_tuya_blinds(config: &BlindsConfig, command: BlindsCommand) -> Result<()> {
    if !config.local_tuya_configured() {
        bail!("Blinds local Tuya config is incomplete");
    }

    let device = build_rustuya_device(config)?;
    let result = device
        .set_value(&config.control_dp, command.tuya_value(config))
        .await
        .with_context(|| format!("failed to send blinds {} command", command.as_str()));
    device.close().await;

    let payload = result?;
    ensure_tuya_command_ok("Local blinds command failed", payload.as_deref())
}

async fn trigger_smart_life_scene(
    config: &BlindsConfig,
    smart_life: &SmartLifeConfig,
    command: BlindsCommand,
) -> Result<()> {
    let home_id = config
        .smart_life_home_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Blinds Smart Life home id is missing"))?;
    let scene_id = command
        .scene_id(config)
        .ok_or_else(|| anyhow!("Blinds Smart Life {} scene id is missing", command.as_str()))?;

    SmartLifeClient::new(
        smart_life.client_id.clone(),
        auth_file_or_default(config.smart_life_auth_file.as_ref()),
    )
    .trigger_scene(home_id, scene_id)
    .await
    .with_context(|| {
        format!(
            "failed to trigger Smart Life {} blinds scene",
            command.as_str()
        )
    })
}

fn build_rustuya_device(config: &BlindsConfig) -> Result<rustuya::Device> {
    let version = rustuya::Version::from_str(&config.version)
        .map_err(|error| anyhow!("invalid blinds Tuya protocol version: {error}"))?;
    Ok(rustuya::Device::builder(
        config.device_id.clone(),
        config.local_key.as_bytes().to_vec(),
    )
    .address(config.ip.clone())
    .version(version)
    .port(config.port)
    .persist(false)
    .timeout(Duration::from_secs(config.status_timeout_seconds.max(1)))
    .build())
}

fn ensure_tuya_command_ok(context: &str, payload: Option<&str>) -> Result<()> {
    match payload {
        Some(raw) if raw.contains("\"Err\"") || raw.contains("\"Error\"") => {
            bail!("{context}: {raw}")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_commands() {
        assert_eq!(BlindsCommand::parse("open"), Some(BlindsCommand::Open));
        assert_eq!(BlindsCommand::parse("close"), Some(BlindsCommand::Close));
        assert_eq!(BlindsCommand::parse("stop"), None);
    }

    #[test]
    fn config_accepts_scene_or_local_tuya_identity() {
        let mut config = BlindsConfig::default();
        assert!(!config.is_configured());

        config.smart_life_home_id = Some("home-id".to_string());
        config.open_scene_id = Some("open-scene".to_string());
        config.close_scene_id = Some("close-scene".to_string());
        assert!(config.is_configured());
        assert!(config.smart_life_scene_configured());
        assert!(!config.local_tuya_configured());

        let mut config = BlindsConfig::default();
        config.ip = "192.168.1.55".to_string();
        config.device_id = "device-id".to_string();
        config.local_key = "local-key".to_string();
        assert!(config.is_configured());
        assert!(!config.smart_life_scene_configured());
        assert!(config.local_tuya_configured());
    }
}
