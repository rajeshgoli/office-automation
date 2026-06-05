use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub state: String,
    pub is_present: bool,
    pub presence_signal_active: bool,
    pub safety_interlock: bool,
    pub erv_should_run: bool,
    pub verifying_departure: bool,
    pub in_door_open_mode: bool,
    pub sensors: SensorsStatus,
    pub air_quality: AirQualityStatus,
    pub erv: ErvStatus,
    pub hvac: HvacStatus,
    pub manual_override: ManualOverrideStatus,
    pub notifications: Vec<AppNotification>,
}

impl Status {
    pub fn read_only_default(config: &AppConfig) -> Self {
        Self::read_only_with_temperature_bands(config, TemperatureBands::from_config(config))
    }

    pub fn read_only_with_temperature_bands(
        config: &AppConfig,
        temperature_bands: TemperatureBands,
    ) -> Self {
        let sensors = SensorsStatus::default();

        Self {
            state: "away".to_string(),
            is_present: false,
            presence_signal_active: false,
            safety_interlock: false,
            erv_should_run: sensors.co2_ppm > config.thresholds.co2_refresh_target_ppm,
            verifying_departure: false,
            in_door_open_mode: false,
            sensors,
            air_quality: AirQualityStatus {
                report_interval: config.qingping.report_interval,
                ..AirQualityStatus::default()
            },
            erv: ErvStatus {
                away_stale_flush_enabled: config.thresholds.away_stale_flush_enabled,
                ..ErvStatus::default()
            },
            hvac: HvacStatus {
                temperature_bands,
                temperature_band_defaults: TemperatureBands::from_config(config),
                ..HvacStatus::default()
            },
            manual_override: ManualOverrideStatus::default(),
            notifications: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SensorsStatus {
    pub mac_last_active: f64,
    pub mac_active: bool,
    pub external_monitor: bool,
    pub motion_detected: bool,
    pub door_open: bool,
    pub window_open: bool,
    pub co2_ppm: i64,
}

impl Default for SensorsStatus {
    fn default() -> Self {
        Self {
            mac_last_active: 0.0,
            mac_active: false,
            external_monitor: false,
            motion_detected: false,
            door_open: false,
            window_open: false,
            co2_ppm: 400,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AirQualityStatus {
    pub co2_ppm: Option<i64>,
    pub temp_c: Option<f64>,
    pub humidity: Option<f64>,
    pub pm25: Option<f64>,
    pub pm10: Option<f64>,
    pub tvoc: Option<i64>,
    pub noise_db: Option<f64>,
    pub last_update: Option<String>,
    pub report_interval: u64,
    pub interval_configured: bool,
}

impl Default for AirQualityStatus {
    fn default() -> Self {
        Self {
            co2_ppm: None,
            temp_c: None,
            humidity: None,
            pm25: None,
            pm10: None,
            tvoc: None,
            noise_db: None,
            last_update: None,
            report_interval: 60,
            interval_configured: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErvStatus {
    pub running: bool,
    pub tvoc_ventilation: bool,
    pub speed: String,
    pub tvoc_plateau: bool,
    pub tvoc_baseline: Option<i64>,
    pub away_stale_flush_enabled: bool,
    pub away_stale_flush_active: bool,
    pub away_stale_flush_active_until: Option<String>,
    pub away_stale_flush_next_due_at: Option<String>,
    pub room_closed_since: Option<String>,
    pub control: ErvControlStatus,
}

impl Default for ErvStatus {
    fn default() -> Self {
        Self {
            running: false,
            tvoc_ventilation: false,
            speed: "off".to_string(),
            tvoc_plateau: false,
            tvoc_baseline: None,
            away_stale_flush_enabled: true,
            away_stale_flush_active: false,
            away_stale_flush_active_until: None,
            away_stale_flush_next_due_at: None,
            room_closed_since: None,
            control: ErvControlStatus::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ErvControlStatus {
    pub last_ok_at: Option<String>,
    pub last_local_ok_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    pub using_cloud: bool,
    pub local_key_invalid: bool,
    pub local_key_invalid_since: Option<String>,
    pub consecutive_local_key_errors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HvacStatus {
    pub mode: String,
    pub setpoint_c: f64,
    pub suspended: bool,
    pub temperature_bands: TemperatureBands,
    pub temperature_band_defaults: TemperatureBands,
}

impl Default for HvacStatus {
    fn default() -> Self {
        Self {
            mode: "off".to_string(),
            setpoint_c: 22.0,
            suspended: false,
            temperature_bands: TemperatureBands::default(),
            temperature_band_defaults: TemperatureBands::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemperatureBands {
    pub heat_on_temp_f: i64,
    pub heat_off_temp_f: i64,
    pub cool_off_temp_f: i64,
    pub cool_on_temp_f: i64,
}

impl TemperatureBands {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            heat_on_temp_f: config.thresholds.hvac_heat_on_temp_f,
            heat_off_temp_f: config.thresholds.hvac_heat_off_temp_f,
            cool_off_temp_f: config.thresholds.hvac_cool_off_temp_f,
            cool_on_temp_f: config.thresholds.hvac_cool_on_temp_f,
        }
    }
}

impl Default for TemperatureBands {
    fn default() -> Self {
        Self {
            heat_on_temp_f: 71,
            heat_off_temp_f: 75,
            cool_off_temp_f: 78,
            cool_on_temp_f: 81,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ManualOverrideStatus {
    pub erv: bool,
    pub erv_speed: Option<String>,
    pub erv_expires_in: Option<i64>,
    pub hvac: bool,
    pub hvac_mode: Option<String>,
    pub hvac_setpoint_f: Option<f64>,
    pub hvac_expires_in: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppNotification {
    pub id: String,
    #[serde(rename = "type")]
    pub notification_type: String,
    pub severity: String,
    pub title: String,
    pub message: String,
    pub created_at: Option<String>,
    pub active: bool,
    pub runbook_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{
        OrchestratorConfig, QingpingConfig, RuntimeConfig, ThresholdsConfig, YoLinkConfig,
    };

    fn test_config() -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            qingping: QingpingConfig {
                report_interval: 45,
                ..QingpingConfig::default()
            },
            yolink: YoLinkConfig::default(),
            thresholds: ThresholdsConfig {
                hvac_heat_on_temp_f: 70,
                hvac_heat_off_temp_f: 74,
                hvac_cool_off_temp_f: 79,
                hvac_cool_on_temp_f: 82,
                away_stale_flush_enabled: false,
                ..ThresholdsConfig::default()
            },
            runtime: RuntimeConfig {
                root: PathBuf::from("/tmp/office"),
                config_path: PathBuf::from("/tmp/office/config.yaml"),
                data_dir: PathBuf::from("/tmp/office/data"),
                database_path: PathBuf::from("/tmp/office/data/office_climate.db"),
                frontend_dist: PathBuf::from("/tmp/office/frontend/dist"),
                artifacts_dir: PathBuf::from("/tmp/office/data/apps"),
                legacy_apk_path: PathBuf::from("/tmp/office/data/app-debug.apk"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
            },
        }
    }

    #[test]
    fn serializes_status_compatibility_shape() {
        let status = Status::read_only_default(&test_config());
        let value = serde_json::to_value(&status).expect("serialize status");

        for key in [
            "state",
            "is_present",
            "presence_signal_active",
            "safety_interlock",
            "erv_should_run",
            "verifying_departure",
            "in_door_open_mode",
            "sensors",
            "air_quality",
            "erv",
            "hvac",
            "manual_override",
            "notifications",
        ] {
            assert!(value.get(key).is_some(), "missing status key {key}");
        }

        assert_eq!(value["state"], "away");
        assert_eq!(value["sensors"]["mac_last_active"], 0.0);
        assert_eq!(value["sensors"]["mac_active"], false);
        assert_eq!(value["sensors"]["co2_ppm"], 400);
        assert_eq!(value["air_quality"]["report_interval"], 45);
        assert_eq!(value["air_quality"]["interval_configured"], false);
        assert_eq!(value["erv"]["speed"], "off");
        assert_eq!(value["erv"]["away_stale_flush_enabled"], false);
        assert!(value["erv"]["control"]["last_ok_at"].is_null());
        assert_eq!(value["erv"]["control"]["local_key_invalid"], false);
        assert_eq!(value["hvac"]["temperature_bands"]["heat_on_temp_f"], 70);
        assert_eq!(
            value["hvac"]["temperature_band_defaults"]["cool_on_temp_f"],
            82
        );
        assert_eq!(value["manual_override"]["erv"], false);
        assert_eq!(value["notifications"].as_array().expect("array").len(), 0);
    }
}
