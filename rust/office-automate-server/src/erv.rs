use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    str::FromStr,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use serde_json::{Map, Value, json};
use tokio::{sync::broadcast, task::JoinHandle, time};

use crate::{
    config::{AppConfig, ErvConfig},
    db,
    status::{AppNotification, ErvControlStatus, Status},
};

const DP_POWER: &str = "1";
const DP_SUPPLY_SPEED: &str = "101";
const DP_EXHAUST_SPEED: &str = "102";
const LOCAL_KEY_ERROR_THRESHOLD: u64 = 5;
pub const ERV_MANUAL_OVERRIDE_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErvFanSpeed {
    Off,
    Quiet,
    Medium,
    Turbo,
}

impl ErvFanSpeed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quiet => "quiet",
            Self::Medium => "medium",
            Self::Turbo => "turbo",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "quiet" => Some(Self::Quiet),
            "medium" => Some(Self::Medium),
            "turbo" => Some(Self::Turbo),
            _ => None,
        }
    }

    fn speed_preset(self) -> Option<(i64, i64)> {
        match self {
            Self::Off => None,
            Self::Quiet => Some((1, 1)),
            Self::Medium => Some((3, 2)),
            Self::Turbo => Some((8, 8)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErvDeviceStatus {
    pub power: bool,
    pub fan_speed: Option<ErvFanSpeed>,
    pub supply_speed: Option<i64>,
    pub exhaust_speed: Option<i64>,
    pub raw_dps: Value,
}

pub type BoxFutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait ErvStatusReader: Send + Sync {
    fn read_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus>;
}

pub trait ErvSpeedWriter: Send + Sync {
    fn smoke_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus>;

    fn set_speed<'a>(
        &'a self,
        config: &'a ErvConfig,
        speed: ErvFanSpeed,
    ) -> BoxFutureResult<'a, ErvDeviceStatus>;
}

#[derive(Debug, Clone, Default)]
pub struct RustuyaErvStatusReader;

impl ErvStatusReader for RustuyaErvStatusReader {
    fn read_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            if !config.is_configured() {
                bail!("ERV local Tuya config is incomplete");
            }

            let device = build_rustuya_device(config)?;
            let result = device.status().await;
            device.close().await;
            let payload = result
                .context("failed to read ERV local Tuya status")?
                .ok_or_else(|| anyhow!("ERV local Tuya status returned no payload"))?;
            parse_erv_status_payload(&payload)
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RustuyaErvSpeedWriter;

impl ErvSpeedWriter for RustuyaErvSpeedWriter {
    fn smoke_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus> {
        RustuyaErvStatusReader.read_status(config)
    }

    fn set_speed<'a>(
        &'a self,
        config: &'a ErvConfig,
        speed: ErvFanSpeed,
    ) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            if !config.is_configured() {
                bail!("ERV local Tuya config is incomplete");
            }

            let device = build_rustuya_device(config)?;
            let result = set_rustuya_speed(&device, config, speed).await;
            device.close().await;
            result
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErvRuntimeSnapshot {
    pub status_known: bool,
    pub running: bool,
    pub speed: ErvFanSpeed,
    pub last_speed_changed_at: Option<f64>,
    pub local_key_invalid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ErvManualOverride {
    speed: ErvFanSpeed,
    expires_at: f64,
}

#[derive(Debug, Clone)]
pub struct ErvState {
    inner: Arc<RwLock<ErvInner>>,
    database_path: PathBuf,
    status_broadcast: Arc<RwLock<Option<broadcast::Sender<()>>>>,
}

#[derive(Debug, Default)]
struct ErvInner {
    latest_status: Option<ErvDeviceStatus>,
    control: ErvControlStatus,
    notification: Option<AppNotification>,
    last_speed_changed_at: Option<f64>,
    manual_override: Option<ErvManualOverride>,
}

impl ErvState {
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
            .expect("ERV broadcast lock poisoned") = Some(sender);
    }

    pub async fn refresh_with<R>(&self, config: &ErvConfig, reader: &R) -> Result<ErvDeviceStatus>
    where
        R: ErvStatusReader + ?Sized,
    {
        match reader.read_status(config).await {
            Ok(status) => {
                if self.record_local_success(status.clone()) {
                    self.notify_status();
                }
                Ok(status)
            }
            Err(error) => {
                let message = format!("{error:#}");
                if self.record_local_failure(&message) {
                    self.notify_status();
                }
                Err(error)
            }
        }
    }

    pub async fn set_speed_with<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.active_control_enabled {
            bail!("ERV active control is disabled");
        }
        if !config.is_configured() {
            bail!("ERV local Tuya config is incomplete");
        }
        if self.snapshot().local_key_invalid {
            bail!("ERV local Tuya key is invalid");
        }

        let smoked_status = self
            .smoke_status_with(config, writer)
            .await
            .context("ERV smoke check failed before active write")?;

        if device_status_matches_speed(&smoked_status, speed) {
            return Ok(smoked_status);
        }

        self.write_speed_after_gate(config, writer, speed, reason, co2_ppm)
            .await
    }

    pub(crate) async fn set_speed_after_smoke_with<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.active_control_enabled {
            bail!("ERV active control is disabled");
        }
        if !config.is_configured() {
            bail!("ERV local Tuya config is incomplete");
        }
        if self.snapshot().local_key_invalid {
            bail!("ERV local Tuya key is invalid");
        }

        self.write_speed_after_gate(config, writer, speed, reason, co2_ppm)
            .await
    }

    async fn write_speed_after_gate<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        match writer.set_speed(config, speed).await {
            Ok(status) => {
                self.record_speed_success(status.clone(), speed, reason, co2_ppm);
                Ok(status)
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.record_local_failure(&message);
                Err(error)
            }
        }
    }

    pub fn snapshot(&self) -> ErvRuntimeSnapshot {
        let inner = self.inner.read().expect("ERV state lock poisoned");
        let status_known = inner.latest_status.is_some();
        let running = inner
            .latest_status
            .as_ref()
            .is_some_and(|status| status.power);
        let speed = inner
            .latest_status
            .as_ref()
            .and_then(|status| status.fan_speed)
            .unwrap_or(ErvFanSpeed::Off);

        ErvRuntimeSnapshot {
            status_known,
            running,
            speed,
            last_speed_changed_at: inner.last_speed_changed_at,
            local_key_invalid: inner.control.local_key_invalid,
        }
    }

    pub async fn smoke_status_with<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.is_configured() {
            bail!("ERV local Tuya config is incomplete");
        }
        if self.snapshot().local_key_invalid {
            bail!("ERV local Tuya key is invalid");
        }

        match writer.smoke_status(config).await {
            Ok(status) => {
                if self.record_local_success(status.clone()) {
                    self.notify_status();
                }
                Ok(status)
            }
            Err(error) => {
                let message = format!("{error:#}");
                if self.record_local_failure(&message) {
                    self.notify_status();
                }
                Err(error)
            }
        }
    }

    pub fn set_manual_override(&self, speed: ErvFanSpeed, now: f64) {
        self.replace_manual_override(speed, now);
    }

    pub(crate) fn replace_manual_override(
        &self,
        speed: ErvFanSpeed,
        now: f64,
    ) -> Option<(ErvFanSpeed, f64)> {
        let manual_override = ErvManualOverride {
            speed,
            expires_at: now + ERV_MANUAL_OVERRIDE_SECONDS as f64,
        };
        let mut inner = self.inner.write().expect("ERV state lock poisoned");
        let previous = inner
            .manual_override
            .map(|manual_override| (manual_override.speed, manual_override.expires_at));
        inner.manual_override = Some(manual_override);
        previous
    }

    pub(crate) fn restore_manual_override(&self, previous: Option<(ErvFanSpeed, f64)>) {
        let restored = previous.map(|(speed, expires_at)| ErvManualOverride { speed, expires_at });
        self.inner
            .write()
            .expect("ERV state lock poisoned")
            .manual_override = restored;
    }

    pub fn active_manual_override_speed(&self, now: f64) -> Option<ErvFanSpeed> {
        let (manual_override, expired) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            match inner.manual_override {
                Some(manual_override) if manual_override.expires_at > now => {
                    (Some(manual_override), false)
                }
                Some(_) => {
                    inner.manual_override = None;
                    (None, true)
                }
                None => (None, false),
            }
        };

        if expired {
            self.notify_status();
        }

        manual_override.map(|manual_override| manual_override.speed)
    }

    pub fn overlay_status(&self, status: &mut Status) {
        let now = unix_timestamp_now();
        let (manual_override, expired) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let (manual_override, expired) = match inner.manual_override {
                Some(manual_override) if manual_override.expires_at > now => {
                    (Some(manual_override), false)
                }
                Some(_) => {
                    inner.manual_override = None;
                    (None, true)
                }
                None => (None, false),
            };

            status.erv.control = inner.control.clone();

            if let Some(device_status) = &inner.latest_status {
                status.erv.running = device_status.power;
                status.erv.speed = device_status
                    .fan_speed
                    .map(ErvFanSpeed::as_str)
                    .unwrap_or("unknown")
                    .to_string();
            }

            if let Some(notification) = &inner.notification {
                status.notifications.push(notification.clone());
            }

            (manual_override, expired)
        };

        if let Some(manual_override) = manual_override {
            status.manual_override.erv = true;
            status.manual_override.erv_speed = Some(manual_override.speed.as_str().to_string());
            status.manual_override.erv_expires_in =
                Some((manual_override.expires_at - now).ceil().max(0.0) as i64);
        }

        if expired {
            self.notify_status();
        }
    }

    fn record_speed_success(
        &self,
        device_status: ErvDeviceStatus,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) {
        self.record_local_success(device_status);
        self.inner
            .write()
            .expect("ERV state lock poisoned")
            .last_speed_changed_at = Some(unix_timestamp_now());

        if let Err(error) = db::log_climate_action(
            &self.database_path,
            "erv",
            speed.as_str(),
            None,
            co2_ppm,
            Some(reason),
        ) {
            tracing::warn!("failed to log ERV climate action: {error:#}");
        }
    }

    fn record_local_success(&self, device_status: ErvDeviceStatus) -> bool {
        let now = local_iso_now();
        let (status_changed, was_invalid, invalid_since) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let status_changed = inner.latest_status.as_ref() != Some(&device_status);
            let was_invalid = inner.control.local_key_invalid;
            let invalid_since = inner.control.local_key_invalid_since.clone();

            inner.latest_status = Some(device_status);
            inner.control.last_ok_at = Some(now.clone());
            inner.control.last_local_ok_at = Some(now.clone());
            inner.control.last_error = None;
            inner.control.using_cloud = false;
            inner.control.local_key_invalid = false;
            inner.control.local_key_invalid_since = None;
            inner.control.consecutive_local_key_errors = 0;

            if was_invalid {
                inner.notification = Some(recovered_notification(&now));
            }

            (status_changed, was_invalid, invalid_since)
        };

        if was_invalid {
            self.log_health_event(
                "local_key_recovered",
                json!({
                    "type": "erv_local_key_recovered",
                    "recovered_at": now,
                    "invalid_since": invalid_since,
                }),
            );
        }
        status_changed || was_invalid
    }

    fn record_local_failure(&self, message: &str) -> bool {
        let now = local_iso_now();
        let mut invalid_event = None;
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            inner.control.last_error = Some(format!("Local status failed: {message}"));
            inner.control.last_error_at = Some(now.clone());
            inner.control.using_cloud = false;

            if !is_local_key_error(message) {
                inner.control.consecutive_local_key_errors = 0;
                return false;
            }

            inner.control.consecutive_local_key_errors += 1;
            if inner.control.consecutive_local_key_errors >= LOCAL_KEY_ERROR_THRESHOLD
                && !inner.control.local_key_invalid
            {
                inner.control.local_key_invalid = true;
                inner.control.local_key_invalid_since = Some(now.clone());
                inner.notification = Some(invalid_key_notification(&now));
                invalid_event = Some(json!({
                    "type": "erv_local_key_invalid",
                    "started_at": now,
                    "consecutive_errors": inner.control.consecutive_local_key_errors,
                    "last_local_ok_at": inner.control.last_local_ok_at,
                    "last_error": inner.control.last_error,
                }));
            }
        }

        if let Some(event) = invalid_event {
            self.log_health_event("local_key_invalid", event);
            return true;
        }
        false
    }

    fn notify_status(&self) {
        let Some(sender) = self
            .status_broadcast
            .read()
            .expect("ERV broadcast lock poisoned")
            .clone()
        else {
            return;
        };
        let _ = sender.send(());
    }

    fn log_health_event(&self, event: &str, details: Value) {
        if let Err(error) = db::log_device_event(
            &self.database_path,
            "erv",
            event,
            Some("Pioneer ECOasis 150"),
            Some(&details),
        ) {
            tracing::warn!("failed to log ERV health event: {error:#}");
        }
    }
}

pub fn start_erv_status_poll(config: &AppConfig, erv: ErvState) -> Option<JoinHandle<()>> {
    if !config.erv.is_configured() {
        tracing::info!("ERV local Tuya config is incomplete; read-only ERV polling disabled");
        return None;
    }

    let config = config.erv.clone();
    Some(tokio::spawn(async move {
        let reader = RustuyaErvStatusReader;
        let mut interval = time::interval(Duration::from_secs(config.poll_interval_seconds.max(5)));
        loop {
            interval.tick().await;
            if let Err(error) = erv.refresh_with(&config, &reader).await {
                tracing::warn!("ERV read-only status poll failed: {error:#}");
            }
        }
    }))
}

pub async fn smoke_erv(config: &AppConfig) -> Result<ErvDeviceStatus> {
    let reader = RustuyaErvStatusReader;
    reader.read_status(&config.erv).await
}

fn build_rustuya_device(config: &ErvConfig) -> Result<rustuya::Device> {
    let version = rustuya::Version::from_str(&config.version)
        .map_err(|error| anyhow!("invalid ERV Tuya protocol version: {error}"))?;
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

async fn set_rustuya_speed(
    device: &rustuya::Device,
    config: &ErvConfig,
    speed: ErvFanSpeed,
) -> Result<ErvDeviceStatus> {
    if speed == ErvFanSpeed::Off {
        let result = device
            .set_value(DP_POWER, false)
            .await
            .context("failed to set ERV power off")?;
        ensure_tuya_command_ok("Local set power off failed", result.as_deref())?;
    } else {
        let (supply, exhaust) = speed.speed_preset().expect("non-off speed has preset");
        let result = device
            .set_value(DP_POWER, true)
            .await
            .context("failed to set ERV power on")?;
        ensure_tuya_command_ok("Local set power on failed", result.as_deref())?;
        let result = device
            .set_value(DP_SUPPLY_SPEED, supply)
            .await
            .context("failed to set ERV supply speed")?;
        ensure_tuya_command_ok("Local set supply speed failed", result.as_deref())?;
        let result = device
            .set_value(DP_EXHAUST_SPEED, exhaust)
            .await
            .context("failed to set ERV exhaust speed")?;
        ensure_tuya_command_ok("Local set exhaust speed failed", result.as_deref())?;
    }

    time::sleep(Duration::from_secs(config.verify_delay_seconds)).await;
    let payload = device
        .status()
        .await
        .context("failed to verify ERV local Tuya status")?
        .ok_or_else(|| anyhow!("ERV local Tuya verification returned no payload"))?;
    let status = parse_erv_status_payload(&payload)?;
    verify_speed(speed, &status)?;
    Ok(status)
}

fn ensure_tuya_command_ok(context: &str, payload: Option<&str>) -> Result<()> {
    if payload.is_some_and(is_local_key_error) || payload.is_some_and(looks_like_tuya_error) {
        bail!("{context}: {}", payload.expect("payload checked"));
    }
    Ok(())
}

fn verify_speed(expected: ErvFanSpeed, actual: &ErvDeviceStatus) -> Result<()> {
    if expected == ErvFanSpeed::Off {
        if actual.power {
            bail!("ERV verification failed: expected power OFF, got ON");
        }
        return Ok(());
    }

    if !actual.power {
        bail!("ERV verification failed: expected power ON, got OFF");
    }
    let (expected_supply, expected_exhaust) =
        expected.speed_preset().expect("non-off speed has preset");
    if actual.supply_speed != Some(expected_supply)
        || actual.exhaust_speed != Some(expected_exhaust)
    {
        bail!(
            "ERV verification failed: expected SA={expected_supply}/EA={expected_exhaust}, got SA={:?}/EA={:?}",
            actual.supply_speed,
            actual.exhaust_speed
        );
    }

    Ok(())
}

pub fn parse_erv_status_payload(payload: &str) -> Result<ErvDeviceStatus> {
    let value: Value = serde_json::from_str(payload).context("ERV status payload is not JSON")?;
    parse_erv_status_value(&value)
}

fn parse_erv_status_value(value: &Value) -> Result<ErvDeviceStatus> {
    let dps = dps_object(value).ok_or_else(|| anyhow!("ERV status payload missing dps object"))?;
    let power = dps.get(DP_POWER).and_then(value_as_bool).unwrap_or(false);
    let supply_speed = dps.get(DP_SUPPLY_SPEED).and_then(value_as_i64);
    let exhaust_speed = dps.get(DP_EXHAUST_SPEED).and_then(value_as_i64);
    let fan_speed = fan_speed(power, supply_speed, exhaust_speed);

    Ok(ErvDeviceStatus {
        power,
        fan_speed,
        supply_speed,
        exhaust_speed,
        raw_dps: Value::Object(dps.clone()),
    })
}

fn dps_object(value: &Value) -> Option<&Map<String, Value>> {
    value
        .get("dps")
        .and_then(Value::as_object)
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| data.get("dps"))
                .and_then(Value::as_object)
        })
        .or_else(|| {
            value
                .as_object()
                .and_then(|object| object.contains_key(DP_POWER).then_some(object))
        })
}

fn fan_speed(
    power: bool,
    supply_speed: Option<i64>,
    exhaust_speed: Option<i64>,
) -> Option<ErvFanSpeed> {
    if !power {
        return Some(ErvFanSpeed::Off);
    }

    match (supply_speed, exhaust_speed) {
        (Some(1), Some(1)) => Some(ErvFanSpeed::Quiet),
        (Some(3), Some(2)) => Some(ErvFanSpeed::Medium),
        (Some(8), Some(8)) => Some(ErvFanSpeed::Turbo),
        _ => None,
    }
}

fn device_status_matches_speed(status: &ErvDeviceStatus, speed: ErvFanSpeed) -> bool {
    match speed {
        ErvFanSpeed::Off => !status.power,
        _ => status.power && status.fan_speed == Some(speed),
    }
}

fn value_as_bool(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .and_then(|value| match value.to_ascii_lowercase().as_str() {
                "true" | "1" | "on" => Some(true),
                "false" | "0" | "off" => Some(false),
                _ => None,
            })
    })
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn is_local_key_error(message: &str) -> bool {
    message.contains("Check device key or version")
        || (message.contains("914") && message.to_ascii_lowercase().contains("err"))
}

fn looks_like_tuya_error(message: &str) -> bool {
    message.contains("\"Error\"") || message.contains("\"Err\"") || message.contains("Error:")
}

fn invalid_key_notification(created_at: &str) -> AppNotification {
    AppNotification {
        id: format!("erv_local_key_invalid:{created_at}"),
        notification_type: "erv_local_key_invalid".to_string(),
        severity: "critical".to_string(),
        title: "ERV local key rotated".to_string(),
        message:
            "Local Tuya control is failing with Err 914. Run docs/tuya-local-key.md to recover it."
                .to_string(),
        created_at: Some(created_at.to_string()),
        active: true,
        runbook_path: Some("docs/tuya-local-key.md".to_string()),
    }
}

fn recovered_notification(created_at: &str) -> AppNotification {
    AppNotification {
        id: format!("erv_local_key_recovered:{created_at}"),
        notification_type: "erv_local_key_recovered".to_string(),
        severity: "info".to_string(),
        title: "ERV local control recovered".to_string(),
        message: "Local Tuya control is working again.".to_string(),
        created_at: Some(created_at.to_string()),
        active: true,
        runbook_path: Some("docs/tuya-local-key.md".to_string()),
    }
}

fn local_iso_now() -> String {
    Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

fn unix_timestamp_now() -> f64 {
    Local::now().timestamp_millis() as f64 / 1_000.0
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{
        config::{
            AppConfig, ErvConfig, MitsubishiConfig, OrchestratorConfig, QingpingConfig,
            RuntimeConfig, ThresholdsConfig, YoLinkConfig,
        },
        db,
        status::Status,
    };

    struct FakeErvReader {
        results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
    }

    impl FakeErvReader {
        fn new(results: Vec<Result<ErvDeviceStatus>>) -> Self {
            Self {
                results: Mutex::new(results.into()),
            }
        }
    }

    impl ErvStatusReader for FakeErvReader {
        fn read_status<'a>(
            &'a self,
            _config: &'a ErvConfig,
        ) -> BoxFutureResult<'a, ErvDeviceStatus> {
            let result = self
                .results
                .lock()
                .expect("fake reader lock")
                .pop_front()
                .unwrap_or_else(|| bail!("no fake ERV result configured"));
            Box::pin(async move { result })
        }
    }

    struct FakeErvWriter {
        smoke_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        write_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        smoke_calls: AtomicUsize,
        write_speeds: Mutex<Vec<ErvFanSpeed>>,
    }

    impl FakeErvWriter {
        fn new(
            smoke_results: Vec<Result<ErvDeviceStatus>>,
            write_results: Vec<Result<ErvDeviceStatus>>,
        ) -> Self {
            Self {
                smoke_results: Mutex::new(smoke_results.into()),
                write_results: Mutex::new(write_results.into()),
                smoke_calls: AtomicUsize::new(0),
                write_speeds: Mutex::new(Vec::new()),
            }
        }

        fn smoke_calls(&self) -> usize {
            self.smoke_calls.load(Ordering::SeqCst)
        }

        fn write_speeds(&self) -> Vec<ErvFanSpeed> {
            self.write_speeds
                .lock()
                .expect("fake writer speeds lock")
                .clone()
        }
    }

    impl ErvSpeedWriter for FakeErvWriter {
        fn smoke_status<'a>(
            &'a self,
            _config: &'a ErvConfig,
        ) -> BoxFutureResult<'a, ErvDeviceStatus> {
            self.smoke_calls.fetch_add(1, Ordering::SeqCst);
            let result = self
                .smoke_results
                .lock()
                .expect("fake writer smoke lock")
                .pop_front()
                .unwrap_or_else(|| bail!("no fake ERV smoke result configured"));
            Box::pin(async move { result })
        }

        fn set_speed<'a>(
            &'a self,
            _config: &'a ErvConfig,
            speed: ErvFanSpeed,
        ) -> BoxFutureResult<'a, ErvDeviceStatus> {
            self.write_speeds
                .lock()
                .expect("fake writer speeds lock")
                .push(speed);
            let result = self
                .write_results
                .lock()
                .expect("fake writer write lock")
                .pop_front()
                .unwrap_or_else(|| bail!("no fake ERV write result configured"));
            Box::pin(async move { result })
        }
    }

    fn test_config() -> ErvConfig {
        ErvConfig {
            ip: "192.0.2.10".to_string(),
            device_id: "device-id".to_string(),
            local_key: "local-key".to_string(),
            ..ErvConfig::default()
        }
    }

    fn active_config() -> ErvConfig {
        ErvConfig {
            active_control_enabled: true,
            verify_delay_seconds: 0,
            ..test_config()
        }
    }

    fn app_config(erv: ErvConfig) -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            presence: crate::config::PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            erv,
            mitsubishi: MitsubishiConfig::default(),
            thresholds: ThresholdsConfig::default(),
            telemetry: crate::config::TelemetryConfig::default(),
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
                telemetry_db_path: PathBuf::from("/tmp/office/data/telemetry.db"),
                tool_usage_db_path: PathBuf::from("/tmp/office/data/tool_usage.db"),
                engram_db_path: PathBuf::from("/tmp/office/data/engram_state.db"),
                engram_registry_path: PathBuf::from("/tmp/office/data/engram_concept_registry.md"),
            },
        }
    }

    fn medium_status() -> ErvDeviceStatus {
        parse_erv_status_payload(r#"{"dps":{"1":true,"101":3,"102":2}}"#).expect("status")
    }

    fn turbo_status() -> ErvDeviceStatus {
        parse_erv_status_payload(r#"{"dps":{"1":true,"101":8,"102":8}}"#).expect("status")
    }

    #[test]
    fn parses_local_tuya_status_payload() {
        let status =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":8,"102":8}}"#).expect("status");

        assert!(status.power);
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));
        assert_eq!(status.supply_speed, Some(8));
        assert_eq!(status.exhaust_speed, Some(8));

        let status =
            parse_erv_status_payload(r#"{"dps":{"1":false,"101":"1","102":"1"}}"#).expect("status");
        assert!(!status.power);
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Off));
    }

    #[tokio::test]
    async fn local_key_error_threshold_sets_control_notification_and_logs_event() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let reader = FakeErvReader::new(
            (0..LOCAL_KEY_ERROR_THRESHOLD)
                .map(|_| bail!("Check device key or version (Error 914)"))
                .collect(),
        );

        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            assert!(state.refresh_with(&test_config(), &reader).await.is_err());
        }

        let mut status = Status::read_only_default(&app_config(test_config()));
        state.overlay_status(&mut status);

        assert!(status.erv.control.local_key_invalid);
        assert_eq!(
            status.erv.control.consecutive_local_key_errors,
            LOCAL_KEY_ERROR_THRESHOLD
        );
        assert_eq!(
            status.notifications[0].notification_type,
            "erv_local_key_invalid"
        );

        let latest = db::get_latest_device_state(&database_path, "erv")
            .expect("query")
            .expect("event");
        assert_eq!(latest, "local_key_invalid");
    }

    #[tokio::test]
    async fn local_status_change_notifies_status_broadcast() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        state.set_status_broadcast(sender);
        let reader = FakeErvReader::new(vec![Ok(medium_status())]);

        state
            .refresh_with(&test_config(), &reader)
            .await
            .expect("success");

        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("broadcast timeout")
            .expect("broadcast message");
    }

    #[tokio::test]
    async fn local_key_invalid_transition_notifies_status_broadcast() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        state.set_status_broadcast(sender);
        let reader = FakeErvReader::new(
            (0..LOCAL_KEY_ERROR_THRESHOLD)
                .map(|_| bail!("Check device key or version (Error 914)"))
                .collect(),
        );

        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            assert!(state.refresh_with(&test_config(), &reader).await.is_err());
        }

        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("broadcast timeout")
            .expect("broadcast message");
    }

    #[tokio::test]
    async fn local_success_updates_status_and_recovers_invalid_key_notification() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let mut results = (0..LOCAL_KEY_ERROR_THRESHOLD)
            .map(|_| bail!("Check device key or version (Error 914)"))
            .collect::<Vec<_>>();
        results.push(Ok(medium_status()));
        let reader = FakeErvReader::new(results);

        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            assert!(state.refresh_with(&test_config(), &reader).await.is_err());
        }
        let refreshed = state
            .refresh_with(&test_config(), &reader)
            .await
            .expect("success");

        assert_eq!(refreshed.fan_speed, Some(ErvFanSpeed::Medium));

        let mut status = Status::read_only_default(&app_config(test_config()));
        state.overlay_status(&mut status);

        assert!(status.erv.running);
        assert_eq!(status.erv.speed, "medium");
        assert!(!status.erv.control.local_key_invalid);
        assert_eq!(status.erv.control.consecutive_local_key_errors, 0);
        assert_eq!(
            status.notifications[0].notification_type,
            "erv_local_key_recovered"
        );
    }

    #[tokio::test]
    async fn active_control_disabled_skips_smoke_and_write() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(vec![Ok(medium_status())], vec![Ok(turbo_status())]);

        let error = state
            .set_speed_with(
                &test_config(),
                &writer,
                ErvFanSpeed::Turbo,
                "manual_override",
                Some(2100),
            )
            .await
            .expect_err("disabled write should fail");

        assert!(error.to_string().contains("active control is disabled"));
        assert_eq!(writer.smoke_calls(), 0);
        assert!(writer.write_speeds().is_empty());
    }

    #[tokio::test]
    async fn active_write_smokes_before_write_and_logs_action() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let writer = FakeErvWriter::new(vec![Ok(medium_status())], vec![Ok(turbo_status())]);

        let status = state
            .set_speed_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Turbo,
                "away_refresh_CO2=2100ppm",
                Some(2100),
            )
            .await
            .expect("write succeeds");

        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));
        assert_eq!(writer.smoke_calls(), 1);
        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Turbo]);

        let snapshot = state.snapshot();
        assert!(snapshot.running);
        assert_eq!(snapshot.speed, ErvFanSpeed::Turbo);
        assert!(snapshot.last_speed_changed_at.is_some());

        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert_eq!(history.climate_actions[0]["system"], "erv");
        assert_eq!(history.climate_actions[0]["action"], "turbo");
        assert_eq!(
            history.climate_actions[0]["reason"],
            "away_refresh_CO2=2100ppm"
        );
        assert_eq!(history.climate_actions[0]["co2_ppm"], 2100);
    }

    #[tokio::test]
    async fn active_write_skips_noop_after_smoke_status_matches_target() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let writer = FakeErvWriter::new(vec![Ok(turbo_status())], vec![Ok(turbo_status())]);

        let status = state
            .set_speed_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Turbo,
                "away_refresh_CO2=2100ppm",
                Some(2100),
            )
            .await
            .expect("smoke succeeds");

        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));
        assert_eq!(writer.smoke_calls(), 1);
        assert!(writer.write_speeds().is_empty());
        assert!(state.snapshot().last_speed_changed_at.is_none());

        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert!(history.climate_actions.is_empty());
    }

    #[tokio::test]
    async fn smoke_local_key_failures_close_active_write_gate() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            (0..LOCAL_KEY_ERROR_THRESHOLD)
                .map(|_| bail!("Check device key or version (Error 914)"))
                .collect(),
            vec![Ok(turbo_status())],
        );

        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            assert!(
                state
                    .set_speed_with(
                        &active_config(),
                        &writer,
                        ErvFanSpeed::Turbo,
                        "manual_override",
                        None,
                    )
                    .await
                    .is_err()
            );
        }

        let mut status = Status::read_only_default(&app_config(active_config()));
        state.overlay_status(&mut status);
        assert!(status.erv.control.local_key_invalid);
        assert_eq!(writer.smoke_calls(), LOCAL_KEY_ERROR_THRESHOLD as usize);
        assert!(writer.write_speeds().is_empty());

        let error = state
            .set_speed_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Quiet,
                "manual_override",
                None,
            )
            .await
            .expect_err("invalid local key should fail before smoke");
        assert!(error.to_string().contains("local Tuya key is invalid"));
        assert_eq!(writer.smoke_calls(), LOCAL_KEY_ERROR_THRESHOLD as usize);
        assert!(writer.write_speeds().is_empty());
    }
}
