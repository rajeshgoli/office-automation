use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Local, Timelike};
use reqwest::{Client, header};
use serde_json::{Value, json};
use tokio::{sync::broadcast, task::JoinHandle, time};

use crate::{
    config::{AppConfig, MitsubishiConfig},
    db,
    status::Status,
};

const KUMO_APP_VERSION: &str = "1297";
pub const HVAC_MANUAL_OVERRIDE_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacControlMode {
    Off,
    Heat,
    Cool,
    Auto,
}

impl HvacControlMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Heat => "heat",
            Self::Cool => "cool",
            Self::Auto => "auto",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "heat" => Some(Self::Heat),
            "cool" => Some(Self::Cool),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HvacModeCommand {
    pub mode: HvacControlMode,
    pub setpoint_c: Option<f64>,
    pub heat_setpoint_c: Option<f64>,
    pub cool_setpoint_c: Option<f64>,
}

impl HvacModeCommand {
    pub fn new(mode: HvacControlMode, setpoint_c: Option<f64>) -> Self {
        Self {
            mode,
            setpoint_c,
            heat_setpoint_c: None,
            cool_setpoint_c: None,
        }
    }

    pub fn auto(heat_setpoint_c: f64, cool_setpoint_c: f64) -> Self {
        Self {
            mode: HvacControlMode::Auto,
            setpoint_c: None,
            heat_setpoint_c: Some(heat_setpoint_c),
            cool_setpoint_c: Some(cool_setpoint_c),
        }
    }
}

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

pub trait HvacModeWriter: Send + Sync {
    fn smoke_status<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
    ) -> BoxFutureResult<'a, HvacDeviceStatus>;

    fn set_mode<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
        mode: HvacControlMode,
        setpoint_c: Option<f64>,
    ) -> BoxFutureResult<'a, HvacDeviceStatus>;

    fn set_mode_command<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
        command: HvacModeCommand,
    ) -> BoxFutureResult<'a, HvacDeviceStatus> {
        self.set_mode(config, command.mode, command.setpoint_c)
    }
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
pub struct KumoHvacModeWriter;

impl HvacModeWriter for KumoHvacModeWriter {
    fn smoke_status<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
    ) -> BoxFutureResult<'a, HvacDeviceStatus> {
        KumoHvacStatusReader.read_status(config)
    }

    fn set_mode<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
        mode: HvacControlMode,
        setpoint_c: Option<f64>,
    ) -> BoxFutureResult<'a, HvacDeviceStatus> {
        self.set_mode_command(config, HvacModeCommand::new(mode, setpoint_c))
    }

    fn set_mode_command<'a>(
        &'a self,
        config: &'a MitsubishiConfig,
        command: HvacModeCommand,
    ) -> BoxFutureResult<'a, HvacDeviceStatus> {
        Box::pin(async move {
            let client = KumoClient::new(config)?;
            client.send_mode_command(command).await?;
            client.get_full_status().await
        })
    }
}

#[derive(Debug, Clone)]
pub struct HvacState {
    inner: Arc<RwLock<HvacInner>>,
    database_path: PathBuf,
    status_broadcast: Arc<RwLock<Option<broadcast::Sender<()>>>>,
}

#[derive(Debug, Default)]
struct HvacInner {
    latest_status: Option<HvacDeviceStatus>,
    suspended: bool,
    last_mode: Option<String>,
    suspended_heat_setpoint_c: Option<f64>,
    suspended_cool_setpoint_c: Option<f64>,
    temp_band_mode: Option<String>,
    manual_override: Option<HvacManualOverride>,
}

#[derive(Debug, Clone, PartialEq)]
struct HvacManualOverride {
    mode: String,
    setpoint_f: f64,
    started_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HvacRuntimeSnapshot {
    pub mode: String,
    pub setpoint_c: f64,
    pub heat_setpoint_c: Option<f64>,
    pub cool_setpoint_c: Option<f64>,
    pub suspended: bool,
    pub last_mode: Option<String>,
    pub suspended_heat_setpoint_c: Option<f64>,
    pub suspended_cool_setpoint_c: Option<f64>,
    pub temp_band_mode: Option<String>,
}

impl HvacState {
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            inner: Arc::default(),
            database_path,
            status_broadcast: Arc::default(),
        }
    }

    pub fn set_status_broadcast(&self, sender: broadcast::Sender<()>) {
        *self
            .status_broadcast
            .write()
            .expect("HVAC broadcast lock poisoned") = Some(sender);
    }

    pub async fn refresh_with<R>(
        &self,
        config: &MitsubishiConfig,
        reader: &R,
    ) -> Result<HvacDeviceStatus>
    where
        R: HvacStatusReader + ?Sized,
    {
        let status = reader.read_status(config).await?;
        if self.record_status(status.clone()) {
            self.notify_status();
        }
        Ok(status)
    }

    pub async fn set_mode_with<W>(
        &self,
        config: &MitsubishiConfig,
        writer: &W,
        mode: HvacControlMode,
        setpoint_c: Option<f64>,
        reason: &str,
    ) -> Result<HvacDeviceStatus>
    where
        W: HvacModeWriter + ?Sized,
    {
        self.set_mode_command_with(
            config,
            writer,
            HvacModeCommand::new(mode, setpoint_c),
            reason,
        )
        .await
    }

    pub async fn set_mode_command_with<W>(
        &self,
        config: &MitsubishiConfig,
        writer: &W,
        command: HvacModeCommand,
        reason: &str,
    ) -> Result<HvacDeviceStatus>
    where
        W: HvacModeWriter + ?Sized,
    {
        self.smoke_status_with(config, writer).await?;
        let status = writer.set_mode_command(config, command).await?;
        self.record_write_success(status.clone(), command.mode, command.setpoint_c, reason);
        Ok(status)
    }

    pub async fn smoke_status_with<W>(
        &self,
        config: &MitsubishiConfig,
        writer: &W,
    ) -> Result<HvacDeviceStatus>
    where
        W: HvacModeWriter + ?Sized,
    {
        validate_active_hvac_config(config)?;

        let smoke_status = writer
            .smoke_status(config)
            .await
            .context("HVAC smoke check failed before active write")?;
        if self.record_status(smoke_status.clone()) {
            self.notify_status();
        }
        Ok(smoke_status)
    }

    pub async fn set_mode_after_verified_status_with<W>(
        &self,
        config: &MitsubishiConfig,
        writer: &W,
        mode: HvacControlMode,
        setpoint_c: Option<f64>,
        reason: &str,
    ) -> Result<HvacDeviceStatus>
    where
        W: HvacModeWriter + ?Sized,
    {
        self.set_mode_command_after_verified_status_with(
            config,
            writer,
            HvacModeCommand::new(mode, setpoint_c),
            reason,
        )
        .await
    }

    pub async fn set_mode_command_after_verified_status_with<W>(
        &self,
        config: &MitsubishiConfig,
        writer: &W,
        command: HvacModeCommand,
        reason: &str,
    ) -> Result<HvacDeviceStatus>
    where
        W: HvacModeWriter + ?Sized,
    {
        validate_active_hvac_config(config)?;

        let status = writer.set_mode_command(config, command).await?;
        self.record_write_success(status.clone(), command.mode, command.setpoint_c, reason);
        Ok(status)
    }

    pub fn record_status(&self, status: HvacDeviceStatus) -> bool {
        let mut inner = self.inner.write().expect("HVAC state lock poisoned");
        let mut changed = inner.latest_status.as_ref() != Some(&status);
        if status.mode != "off" {
            if inner.last_mode.as_deref() != Some(status.mode.as_str()) {
                changed = true;
            }
            inner.last_mode = Some(status.mode.clone());
            if inner.suspended
                || inner.suspended_heat_setpoint_c.is_some()
                || inner.suspended_cool_setpoint_c.is_some()
            {
                changed = true;
            }
            inner.suspended = false;
            inner.suspended_heat_setpoint_c = None;
            inner.suspended_cool_setpoint_c = None;
        }
        inner.latest_status = Some(status);
        changed
    }

    pub fn latest(&self) -> Option<HvacDeviceStatus> {
        self.inner
            .read()
            .expect("HVAC state lock poisoned")
            .latest_status
            .clone()
    }

    pub fn snapshot(&self) -> HvacRuntimeSnapshot {
        let inner = self.inner.read().expect("HVAC state lock poisoned");
        let latest = inner.latest_status.clone();
        HvacRuntimeSnapshot {
            mode: latest
                .as_ref()
                .map(|status| status.mode.clone())
                .unwrap_or_else(|| "off".to_string()),
            setpoint_c: latest
                .as_ref()
                .map(|status| status.setpoint_c)
                .unwrap_or(22.0),
            heat_setpoint_c: latest.as_ref().and_then(|status| status.heat_setpoint_c),
            cool_setpoint_c: latest.as_ref().and_then(|status| status.cool_setpoint_c),
            suspended: inner.suspended,
            last_mode: inner.last_mode.clone(),
            suspended_heat_setpoint_c: inner.suspended_heat_setpoint_c,
            suspended_cool_setpoint_c: inner.suspended_cool_setpoint_c,
            temp_band_mode: inner.temp_band_mode.clone(),
        }
    }

    pub fn record_manual_override(&self, mode: HvacControlMode, setpoint_f: f64) {
        let mut inner = self.inner.write().expect("HVAC state lock poisoned");
        inner.manual_override = Some(HvacManualOverride {
            mode: mode.as_str().to_string(),
            setpoint_f,
            started_at: unix_timestamp_now(),
        });
        inner.suspended = false;
        inner.suspended_heat_setpoint_c = None;
        inner.suspended_cool_setpoint_c = None;
        inner.last_mode = if mode == HvacControlMode::Off {
            None
        } else {
            Some(mode.as_str().to_string())
        };
        inner.temp_band_mode = None;
    }

    pub fn manual_override_active(&self) -> bool {
        let now = unix_timestamp_now();
        let mut inner = self.inner.write().expect("HVAC state lock poisoned");
        prune_expired_manual_override(&mut inner, now);
        inner.manual_override.is_some()
    }

    pub fn clear_manual_override(&self) -> bool {
        self.inner
            .write()
            .expect("HVAC state lock poisoned")
            .manual_override
            .take()
            .is_some()
    }

    pub fn set_suspended(&self, suspended: bool, last_mode: Option<String>) {
        self.set_suspended_with_setpoints(suspended, last_mode, None, None);
    }

    pub fn set_suspended_with_setpoints(
        &self,
        suspended: bool,
        last_mode: Option<String>,
        heat_setpoint_c: Option<f64>,
        cool_setpoint_c: Option<f64>,
    ) {
        let mut inner = self.inner.write().expect("HVAC state lock poisoned");
        inner.suspended = suspended;
        if let Some(last_mode) = last_mode {
            inner.last_mode = Some(last_mode);
        }
        if suspended {
            inner.suspended_heat_setpoint_c = heat_setpoint_c;
            inner.suspended_cool_setpoint_c = cool_setpoint_c;
        } else {
            inner.suspended_heat_setpoint_c = None;
            inner.suspended_cool_setpoint_c = None;
        }
    }

    pub fn set_temp_band_mode(&self, mode: Option<HvacControlMode>) {
        self.inner
            .write()
            .expect("HVAC state lock poisoned")
            .temp_band_mode = mode.map(|mode| mode.as_str().to_string());
    }

    pub fn overlay_status(&self, status: &mut Status) {
        let now = unix_timestamp_now();
        let mut inner = self.inner.write().expect("HVAC state lock poisoned");
        prune_expired_manual_override(&mut inner, now);

        if let Some(device_status) = &inner.latest_status {
            status.hvac.mode = device_status.mode.clone();
            status.hvac.setpoint_c = device_status.setpoint_c;
        }
        status.hvac.suspended = inner.suspended;

        if let Some(manual_override) = &inner.manual_override {
            let expires_in =
                HVAC_MANUAL_OVERRIDE_SECONDS - (now - manual_override.started_at).floor() as i64;
            status.manual_override.hvac = true;
            status.manual_override.hvac_mode = Some(manual_override.mode.clone());
            status.manual_override.hvac_setpoint_f = Some(manual_override.setpoint_f);
            status.manual_override.hvac_expires_in = Some(expires_in.max(0));
        }
    }

    fn record_write_success(
        &self,
        device_status: HvacDeviceStatus,
        mode: HvacControlMode,
        setpoint_c: Option<f64>,
        reason: &str,
    ) {
        if self.record_status(device_status) {
            self.notify_status();
        }
        if let Err(error) = db::log_climate_action(
            &self.database_path,
            "hvac",
            mode.as_str(),
            setpoint_c,
            None,
            Some(reason),
        ) {
            tracing::warn!("failed to log HVAC climate action: {error:#}");
        }
    }
}

fn validate_active_hvac_config(config: &MitsubishiConfig) -> Result<()> {
    if !config.active_control_enabled {
        bail!("HVAC active control is disabled");
    }
    if !config.is_configured() {
        bail!("Mitsubishi Kumo config is incomplete");
    }
    Ok(())
}

impl Default for HvacState {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

impl HvacState {
    fn notify_status(&self) {
        let Some(sender) = self
            .status_broadcast
            .read()
            .expect("HVAC broadcast lock poisoned")
            .clone()
        else {
            return;
        };
        let _ = sender.send(());
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

    async fn send_mode_command(&self, command: HvacModeCommand) -> Result<Value> {
        let token = self.login().await?;
        let mode = command.mode;
        let mut commands = serde_json::Map::new();
        commands.insert(
            "operationMode".to_string(),
            Value::String(mode.as_str().to_string()),
        );

        match mode {
            HvacControlMode::Off => {}
            HvacControlMode::Heat => {
                commands.insert(
                    "spHeat".to_string(),
                    json!(command.setpoint_c.unwrap_or(22.0)),
                );
            }
            HvacControlMode::Cool => {
                commands.insert(
                    "spCool".to_string(),
                    json!(command.setpoint_c.unwrap_or(22.0)),
                );
            }
            HvacControlMode::Auto => {
                if let Some(heat_setpoint_c) = command.heat_setpoint_c.or(command.setpoint_c) {
                    commands.insert("spHeat".to_string(), json!(heat_setpoint_c));
                }
                if let Some(cool_setpoint_c) = command.cool_setpoint_c.or(command.setpoint_c) {
                    commands.insert("spCool".to_string(), json!(cool_setpoint_c));
                }
            }
        }

        let response = self
            .http
            .post(self.url("/v3/devices/send-command"))
            .headers(kumo_headers())
            .bearer_auth(token)
            .json(&json!({
                "deviceSerial": self.device_serial,
                "commands": commands,
            }))
            .send()
            .await
            .context("failed to send Kumo command request")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Kumo command failed: {status} - {text}");
        }

        response
            .json()
            .await
            .context("Kumo command response is not JSON")
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

fn prune_expired_manual_override(inner: &mut HvacInner, now: f64) {
    let expired = inner
        .manual_override
        .as_ref()
        .is_some_and(|manual| now - manual.started_at > HVAC_MANUAL_OVERRIDE_SECONDS as f64);
    if expired {
        inner.manual_override = None;
    }
}

fn unix_timestamp_now() -> f64 {
    Local::now().timestamp_millis() as f64 / 1_000.0
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
        let mut initial_status_loaded = false;
        loop {
            interval.tick().await;
            if should_skip_hvac_poll(initial_status_loaded, Local::now().hour()) {
                tracing::debug!("HVAC polling paused during night hours");
                continue;
            }
            match hvac.refresh_with(&config, &reader).await {
                Ok(_) => initial_status_loaded = true,
                Err(error) => tracing::warn!("HVAC read-only status poll failed: {error:#}"),
            }
        }
    }))
}

pub async fn smoke_hvac(config: &AppConfig) -> Result<HvacDeviceStatus> {
    let reader = KumoHvacStatusReader;
    reader.read_status(&config.mitsubishi).await
}

fn should_skip_hvac_poll(initial_status_loaded: bool, hour: u32) -> bool {
    initial_status_loaded && (hour >= 23 || hour < 6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ThresholdsConfig, status::Status};

    struct FakeHvacReader {
        status: HvacDeviceStatus,
    }

    impl HvacStatusReader for FakeHvacReader {
        fn read_status<'a>(
            &'a self,
            _config: &'a MitsubishiConfig,
        ) -> BoxFutureResult<'a, HvacDeviceStatus> {
            let status = self.status.clone();
            Box::pin(async move { Ok(status) })
        }
    }

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
            presence: crate::config::PresenceConfig::default(),
            qingping: crate::config::QingpingConfig::default(),
            yolink: crate::config::YoLinkConfig::default(),
            artifacts: crate::config::ArtifactConfig::default(),
            cloudflare_access: crate::config::CloudflareAccessConfig::default(),
            erv: crate::config::ErvConfig::default(),
            mitsubishi: crate::config::MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            telemetry: crate::config::TelemetryConfig::default(),
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
                telemetry_db_path: "/tmp/office/data/telemetry.db".into(),
                session_tool_usage_db_path: "/tmp/office/data/claude_tool_usage.db".into(),
                tool_usage_db_path: "/tmp/office/data/tool_usage.db".into(),
                engram_db_path: "/tmp/office/data/engram_state.db".into(),
                engram_registry_path: "/tmp/office/data/engram_concept_registry.md".into(),
            },
        });
        hvac.overlay_status(&mut status);

        assert_eq!(status.hvac.mode, "cool");
        assert_eq!(status.hvac.setpoint_c, 24.5);
    }

    #[test]
    fn record_status_clears_stale_suspension_when_device_is_active() {
        let hvac = HvacState::default();
        let status = parse_kumo_adapter_status(&json!({
            "power": 1,
            "operationMode": "auto",
            "spHeat": 20.0,
            "spCool": 26.0
        }))
        .expect("status");
        hvac.record_status(status.clone());
        hvac.set_suspended_with_setpoints(true, Some("auto".to_string()), Some(20.0), Some(26.0));

        assert!(hvac.record_status(status));
        let snapshot = hvac.snapshot();
        assert!(!snapshot.suspended);
        assert_eq!(snapshot.last_mode.as_deref(), Some("auto"));
        assert_eq!(snapshot.suspended_heat_setpoint_c, None);
        assert_eq!(snapshot.suspended_cool_setpoint_c, None);
    }

    #[tokio::test]
    async fn hvac_status_change_notifies_status_broadcast() {
        let hvac = HvacState::default();
        let (sender, mut receiver) = broadcast::channel(4);
        hvac.set_status_broadcast(sender);
        let config = MitsubishiConfig::default();

        let cool = parse_kumo_adapter_status(&json!({
            "power": 1,
            "operationMode": "cool",
            "spCool": 24.5
        }))
        .expect("cool status");
        hvac.refresh_with(
            &config,
            &FakeHvacReader {
                status: cool.clone(),
            },
        )
        .await
        .expect("refresh succeeds");

        time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("broadcast timeout")
            .expect("broadcast message");

        hvac.refresh_with(&config, &FakeHvacReader { status: cool })
            .await
            .expect("refresh succeeds");

        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn hvac_night_pause_does_not_skip_initial_status_load() {
        assert!(!should_skip_hvac_poll(false, 23));
        assert!(!should_skip_hvac_poll(false, 5));
        assert!(should_skip_hvac_poll(true, 23));
        assert!(should_skip_hvac_poll(true, 5));
        assert!(!should_skip_hvac_poll(true, 6));
        assert!(!should_skip_hvac_poll(true, 22));
    }
}
