use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, Timelike};
use reqwest::{Client, header};
use serde_json::{Value, json};
use tokio::{task::JoinHandle, time};

use crate::{
    config::{AppConfig, MitsubishiConfig},
    status::Status,
};

const KUMO_APP_VERSION: &str = "1297";

#[derive(Debug, Clone, PartialEq)]
pub struct HvacDeviceStatus {
    pub power: bool,
    pub mode: String,
    pub setpoint_c: f64,
    pub heat_setpoint_c: Option<f64>,
    pub cool_setpoint_c: Option<f64>,
    pub raw_adapter: Value,
}

pub type BoxFutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait HvacStatusReader: Send + Sync {
    fn read_status<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
    ) -> BoxFutureResult<'a, HvacDeviceStatus>;
}

#[derive(Debug, Clone, Default)]
pub struct KumoHvacStatusReader;

impl HvacStatusReader for KumoHvacStatusReader {
    fn read_status<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
    ) -> BoxFutureResult<'a, HvacDeviceStatus> {
        Box::pin(async move {
            let client = KumoClient::new(config)?;
            client.get_full_status().await
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct HvacState {
    inner: Arc<RwLock<Option<HvacDeviceStatus>>>,
}

impl HvacState {
    pub async fn refresh_with<R>(
        &self,
        config: &MitsubishiConfig,
        reader: &R,
    ) -> Result<HvacDeviceStatus>
    where
        R: HvacStatusReader + ?Sized,
    {
        let status = reader.read_status(config).await?;
        self.record_status(status.clone());
        Ok(status)
    }

    pub fn record_status(&self, status: HvacDeviceStatus) {
        *self.inner.write().expect("HVAC state lock poisoned") = Some(status);
    }

    pub fn latest(&self) -> Option<HvacDeviceStatus> {
        self.inner.read().expect("HVAC state lock poisoned").clone()
    }

    pub fn overlay_status(&self, status: &mut Status) {
        let Some(device_status) = self.latest() else {
            return;
        };

        status.hvac.mode = device_status.mode;
        status.hvac.setpoint_c = device_status.setpoint_c;
    }
}

#[derive(Debug, Clone)]
struct KumoClient {
    http: Client,
    base_url: String,
    username: String,
    password: String,
    device_serial: String,
}

impl KumoClient {
    fn new(config: &MitsubishiConfig) -> Result<Self> {
        if !config.is_configured() {
            bail!("Mitsubishi Kumo config is incomplete");
        }

        let http = Client::builder()
            .timeout(Duration::from_secs(config.status_timeout_seconds.max(1)))
            .build()
            .context("failed to build Kumo HTTP client")?;

        Ok(Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            username: config
                .username
                .as_ref()
                .expect("checked configured username")
                .clone(),
            password: config
                .password
                .as_ref()
                .expect("checked configured password")
                .clone(),
            device_serial: config
                .device_serial
                .as_ref()
                .expect("checked configured serial")
                .clone(),
        })
    }

    async fn get_full_status(&self) -> Result<HvacDeviceStatus> {
        let token = self.login().await?;
        let sites = self.get_json("/v3/sites", &token).await?;
        let Some(sites) = sites.as_array() else {
            bail!("Kumo sites response is not an array");
        };

        for site in sites {
            let Some(site_id) = site.get("id").and_then(Value::as_str) else {
                continue;
            };
            let zones = self
                .get_json(&format!("/v3/sites/{site_id}/zones"), &token)
                .await?;
            let Some(zones) = zones.as_array() else {
                bail!("Kumo zones response for site {site_id} is not an array");
            };

            for zone in zones {
                let Some(adapter) = zone.get("adapter") else {
                    continue;
                };
                if adapter.get("deviceSerial").and_then(Value::as_str)
                    == Some(self.device_serial.as_str())
                {
                    return parse_kumo_adapter_status(adapter);
                }
            }
        }

        bail!("Kumo device {} not found in any zone", self.device_serial)
    }

    async fn login(&self) -> Result<String> {
        let response = self
            .http
            .post(self.url("/v3/login"))
            .headers(kumo_headers())
            .json(&json!({
                "username": self.username,
                "password": self.password,
                "appVersion": KUMO_APP_VERSION,
            }))
            .send()
            .await
            .context("failed to send Kumo login request")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Kumo login failed: {status} - {text}");
        }

        let payload: Value = response
            .json()
            .await
            .context("Kumo login response is not JSON")?;
        payload
            .get("token")
            .and_then(|token| token.get("access"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Kumo login response missing token.access"))
    }

    async fn get_json(&self, path: &str, token: &str) -> Result<Value> {
        let response = self
            .http
            .get(self.url(path))
            .headers(kumo_headers())
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| format!("failed to send Kumo GET {path}"))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Kumo GET {path} failed: {status} - {text}");
        }

        response
            .json()
            .await
            .with_context(|| format!("Kumo GET {path} response is not JSON"))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

fn kumo_headers() -> header::HeaderMap {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_static("kumocloud/1297 CFNetwork/3826.600.41 Darwin/24.6.0"),
    );
    headers.insert(
        "X-App-Version",
        header::HeaderValue::from_static(KUMO_APP_VERSION),
    );
    headers.insert("X-App-Platform", header::HeaderValue::from_static("ios"));
    headers
}

pub fn parse_kumo_adapter_status(adapter: &Value) -> Result<HvacDeviceStatus> {
    let power = adapter.get("power").and_then(value_as_i64).unwrap_or(0) == 1;
    let raw_mode = adapter
        .get("operationMode")
        .and_then(Value::as_str)
        .unwrap_or("off");
    let mode = if power {
        normalize_mode(raw_mode)
    } else {
        "off".to_string()
    };
    let heat_setpoint_c = adapter.get("spHeat").and_then(value_as_f64);
    let cool_setpoint_c = adapter.get("spCool").and_then(value_as_f64);
    let setpoint_c = match mode.as_str() {
        "heat" => heat_setpoint_c.unwrap_or(22.0),
        "cool" => cool_setpoint_c.unwrap_or(22.0),
        "auto" => heat_setpoint_c.or(cool_setpoint_c).unwrap_or(22.0),
        _ => 22.0,
    };

    Ok(HvacDeviceStatus {
        power,
        mode,
        setpoint_c,
        heat_setpoint_c,
        cool_setpoint_c,
        raw_adapter: adapter.clone(),
    })
}

fn normalize_mode(value: &str) -> String {
    match value {
        "off" | "heat" | "cool" | "auto" | "dry" | "vent" => value.to_string(),
        _ => "off".to_string(),
    }
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

pub fn start_hvac_status_poll(config: &AppConfig, hvac: HvacState) -> Option<JoinHandle<()>> {
    if !config.mitsubishi.is_configured() {
        tracing::info!("Mitsubishi Kumo config is incomplete; read-only HVAC polling disabled");
        return None;
    }

    let config = config.mitsubishi.clone();
    Some(tokio::spawn(async move {
        let reader = KumoHvacStatusReader;
        let mut interval =
            time::interval(Duration::from_secs(config.poll_interval_seconds.max(60)));
        loop {
            interval.tick().await;
            if hvac_poll_paused_now() {
                tracing::debug!("HVAC polling paused during night hours");
                continue;
            }
            if let Err(error) = hvac.refresh_with(&config, &reader).await {
                tracing::warn!("HVAC read-only status poll failed: {error:#}");
            }
        }
    }))
}

pub async fn smoke_hvac(config: &AppConfig) -> Result<HvacDeviceStatus> {
    let reader = KumoHvacStatusReader;
    reader.read_status(&config.mitsubishi).await
}

fn hvac_poll_paused_now() -> bool {
    let hour = Local::now().hour();
    hour >= 23 || hour < 6
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ThresholdsConfig, status::Status};

    #[test]
    fn parses_kumo_adapter_status_for_active_heat_and_cool() {
        let heat = parse_kumo_adapter_status(&json!({
            "deviceSerial": "serial",
            "power": 1,
            "operationMode": "heat",
            "spHeat": 21.5,
            "spCool": 25.0
        }))
        .expect("heat status");
        assert!(heat.power);
        assert_eq!(heat.mode, "heat");
        assert_eq!(heat.setpoint_c, 21.5);
        assert_eq!(heat.heat_setpoint_c, Some(21.5));
        assert_eq!(heat.cool_setpoint_c, Some(25.0));

        let cool = parse_kumo_adapter_status(&json!({
            "power": "1",
            "operationMode": "cool",
            "spHeat": "21.0",
            "spCool": "25.5"
        }))
        .expect("cool status");
        assert_eq!(cool.mode, "cool");
        assert_eq!(cool.setpoint_c, 25.5);
    }

    #[test]
    fn treats_powered_off_kumo_adapter_as_off() {
        let status = parse_kumo_adapter_status(&json!({
            "power": 0,
            "operationMode": "heat",
            "spHeat": 21.0,
            "spCool": 25.0
        }))
        .expect("status");

        assert!(!status.power);
        assert_eq!(status.mode, "off");
        assert_eq!(status.setpoint_c, 22.0);
    }

    #[test]
    fn hvac_state_overlays_cached_status() {
        let hvac = HvacState::default();
        hvac.record_status(
            parse_kumo_adapter_status(&json!({
                "power": 1,
                "operationMode": "cool",
                "spCool": 24.5
            }))
            .expect("status"),
        );

        let mut status = Status::read_only_default(&crate::config::AppConfig {
            orchestrator: crate::config::OrchestratorConfig::default(),
            qingping: crate::config::QingpingConfig::default(),
            yolink: crate::config::YoLinkConfig::default(),
            erv: crate::config::ErvConfig::default(),
            mitsubishi: crate::config::MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            runtime: crate::config::RuntimeConfig {
                root: "/tmp/office".into(),
                config_path: "/tmp/office/config.yaml".into(),
                data_dir: "/tmp/office/data".into(),
                database_path: "/tmp/office/data/office_climate.db".into(),
                frontend_dist: "/tmp/office/frontend/dist".into(),
                artifacts_dir: "/tmp/office/data/apps".into(),
                legacy_apk_path: "/tmp/office/data/app-debug.apk".into(),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
            },
        });
        hvac.overlay_status(&mut status);

        assert_eq!(status.hvac.mode, "cool");
        assert_eq!(status.hvac.setpoint_c, 24.5);
    }
}
