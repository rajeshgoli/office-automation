use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub orchestrator: OrchestratorConfig,
    pub qingping: QingpingConfig,
    pub thresholds: ThresholdsConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OrchestratorConfig {
    pub host: String,
    pub port: u16,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct QingpingConfig {
    pub mqtt_broker: String,
    pub mqtt_port: u16,
    pub device_mac: Option<String>,
    pub report_interval: u64,
}

impl Default for QingpingConfig {
    fn default() -> Self {
        Self {
            mqtt_broker: "127.0.0.1".to_string(),
            mqtt_port: 1883,
            device_mac: None,
            report_interval: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThresholdsConfig {
    pub motion_timeout_seconds: u64,
    pub departure_verification_seconds: u64,
    pub door_open_threshold_minutes: u64,
    pub door_open_away_timeout_minutes: u64,
    pub co2_critical_ppm: i64,
    pub co2_critical_hysteresis_ppm: i64,
    pub co2_refresh_target_ppm: i64,
    pub co2_plateau_enabled: bool,
    pub co2_plateau_rate_threshold: f64,
    pub co2_plateau_window_minutes: u64,
    pub co2_plateau_min_co2: i64,
    pub co2_plateau_release_delta_ppm: i64,
    pub co2_history_size: usize,
    pub co2_adaptive_speed_enabled: bool,
    pub co2_rate_turbo_threshold: f64,
    pub co2_rate_medium_threshold: f64,
    pub co2_rate_quiet_threshold: f64,
    pub co2_turbo_duration_minutes: u64,
    pub min_away_seconds_before_erv: u64,
    pub erv_min_dwell_seconds: u64,
    pub tvoc_away_enabled: bool,
    pub tvoc_away_threshold: i64,
    pub tvoc_away_target: i64,
    pub tvoc_away_history_size: usize,
    pub tvoc_plateau_rate_threshold: f64,
    pub tvoc_rate_turbo_threshold: f64,
    pub tvoc_rate_medium_threshold: f64,
    pub tvoc_rate_quiet_threshold: f64,
    pub hvac_min_temp_f: i64,
    pub hvac_critical_temp_f: i64,
    pub hvac_heat_on_temp_f: i64,
    pub hvac_heat_off_temp_f: i64,
    pub hvac_cool_off_temp_f: i64,
    pub hvac_cool_on_temp_f: i64,
    pub away_stale_flush_enabled: bool,
    pub away_stale_flush_interval_hours: u64,
    pub away_stale_flush_duration_minutes: u64,
    pub away_stale_flush_speed: String,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            motion_timeout_seconds: 60,
            departure_verification_seconds: 10,
            door_open_threshold_minutes: 5,
            door_open_away_timeout_minutes: 5,
            co2_critical_ppm: 2000,
            co2_critical_hysteresis_ppm: 200,
            co2_refresh_target_ppm: 500,
            co2_plateau_enabled: true,
            co2_plateau_rate_threshold: 0.5,
            co2_plateau_window_minutes: 10,
            co2_plateau_min_co2: 600,
            co2_plateau_release_delta_ppm: 100,
            co2_history_size: 40,
            co2_adaptive_speed_enabled: true,
            co2_rate_turbo_threshold: 8.0,
            co2_rate_medium_threshold: 2.0,
            co2_rate_quiet_threshold: 0.5,
            co2_turbo_duration_minutes: 30,
            min_away_seconds_before_erv: 60,
            erv_min_dwell_seconds: 180,
            tvoc_away_enabled: true,
            tvoc_away_threshold: 200,
            tvoc_away_target: 40,
            tvoc_away_history_size: 40,
            tvoc_plateau_rate_threshold: 0.3,
            tvoc_rate_turbo_threshold: 5.0,
            tvoc_rate_medium_threshold: 1.5,
            tvoc_rate_quiet_threshold: 0.3,
            hvac_min_temp_f: 68,
            hvac_critical_temp_f: 55,
            hvac_heat_on_temp_f: 71,
            hvac_heat_off_temp_f: 75,
            hvac_cool_off_temp_f: 78,
            hvac_cool_on_temp_f: 81,
            away_stale_flush_enabled: true,
            away_stale_flush_interval_hours: 8,
            away_stale_flush_duration_minutes: 30,
            away_stale_flush_speed: "medium".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub base_url: Option<String>,
    pub public_url: Option<String>,
    pub mqtt_host: String,
    pub mqtt_port: u16,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct FileConfig {
    orchestrator: OrchestratorConfig,
    qingping: QingpingConfig,
    thresholds: ThresholdsConfig,
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_env(path, |key| env::var(key).ok())
    }

    pub fn load_with_env(
        path: impl AsRef<Path>,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let config_path = path.as_ref();
        let contents = fs::read_to_string(config_path)
            .with_context(|| format!("failed to read config {}", config_path.display()))?;
        let file_config: FileConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", config_path.display()))?;

        Self::from_file_config(config_path, file_config, env_lookup)
    }

    fn from_file_config(
        config_path: &Path,
        mut file_config: FileConfig,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let root = env_lookup("OFFICE_AUTOMATE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().context("failed to determine current directory")?);

        let data_dir = env_lookup("OFFICE_AUTOMATE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("data"));

        if let Some(host) = env_lookup("OFFICE_AUTOMATE_MQTT_HOST") {
            file_config.qingping.mqtt_broker = host;
        }

        if let Some(port) = env_lookup("OFFICE_AUTOMATE_MQTT_PORT") {
            file_config.qingping.mqtt_port = port
                .parse()
                .with_context(|| format!("invalid OFFICE_AUTOMATE_MQTT_PORT value {port:?}"))?;
        }

        let runtime = RuntimeConfig {
            root,
            config_path: config_path.to_path_buf(),
            database_path: data_dir.join("office_climate.db"),
            data_dir,
            base_url: env_lookup("OFFICE_AUTOMATE_BASE_URL"),
            public_url: env_lookup("OFFICE_AUTOMATE_PUBLIC_URL"),
            mqtt_host: file_config.qingping.mqtt_broker.clone(),
            mqtt_port: file_config.qingping.mqtt_port,
        };

        Ok(Self {
            orchestrator: file_config.orchestrator,
            qingping: file_config.qingping,
            thresholds: file_config.thresholds,
            runtime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_config_and_applies_environment_overrides() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_path = temp_dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
orchestrator:
  host: "127.0.0.1"
  port: 9001
qingping:
  mqtt_broker: "legacy-broker"
  mqtt_port: 1883
  device_mac: "AA:BB:CC:DD:EE:FF"
  report_interval: 30
thresholds:
  hvac_heat_on_temp_f: 70
  hvac_heat_off_temp_f: 74
  hvac_cool_off_temp_f: 79
  hvac_cool_on_temp_f: 82
"#,
        )
        .expect("write config");

        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "OFFICE_AUTOMATE_ROOT" => Some(temp_dir.path().join("root").display().to_string()),
            "OFFICE_AUTOMATE_DATA_DIR" => Some(temp_dir.path().join("db").display().to_string()),
            "OFFICE_AUTOMATE_MQTT_HOST" => Some("rust-broker".to_string()),
            "OFFICE_AUTOMATE_MQTT_PORT" => Some("2883".to_string()),
            "OFFICE_AUTOMATE_PUBLIC_URL" => Some("https://office.example.com".to_string()),
            _ => None,
        })
        .expect("load config");

        assert_eq!(config.orchestrator.host, "127.0.0.1");
        assert_eq!(config.orchestrator.port, 9001);
        assert_eq!(config.qingping.mqtt_broker, "rust-broker");
        assert_eq!(config.qingping.mqtt_port, 2883);
        assert_eq!(config.runtime.mqtt_host, "rust-broker");
        assert_eq!(config.runtime.mqtt_port, 2883);
        assert_eq!(config.runtime.data_dir, temp_dir.path().join("db"));
        assert_eq!(
            config.runtime.database_path,
            temp_dir.path().join("db").join("office_climate.db")
        );
        assert_eq!(
            config.runtime.public_url.as_deref(),
            Some("https://office.example.com")
        );
        assert_eq!(config.thresholds.hvac_heat_on_temp_f, 70);
        assert_eq!(config.thresholds.hvac_cool_on_temp_f, 82);
    }
}
