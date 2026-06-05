use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::broadcast, task::JoinHandle};

use crate::{
    automation::ErvPolicyCoordinator,
    config::{AppConfig, YoLinkConfig},
    db,
    state::{StateMachine, StateTransition},
    status::Status,
};

pub type DeviceIngressHook = Arc<dyn Fn(Option<StateTransition>) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    DoorSensor,
    MotionSensor,
    ContactSensor,
    Unknown,
}

impl DeviceType {
    fn from_api(value: &str) -> Self {
        match value {
            "DoorSensor" => Self::DoorSensor,
            "MotionSensor" => Self::MotionSensor,
            "ContactSensor" => Self::ContactSensor,
            _ => Self::Unknown,
        }
    }

    pub fn as_api_method_prefix(self) -> &'static str {
        match self {
            Self::DoorSensor => "DoorSensor",
            Self::MotionSensor => "MotionSensor",
            Self::ContactSensor => "ContactSensor",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct YoLinkDevice {
    pub device_id: String,
    pub name: String,
    pub token: String,
    pub device_type: DeviceType,
    pub state: serde_json::Map<String, Value>,
}

impl YoLinkDevice {
    pub fn is_open(&self) -> Option<bool> {
        self.state
            .get("state")?
            .as_str()
            .map(|state| state == "open")
    }

    pub fn motion_detected(&self) -> Option<bool> {
        self.state
            .get("state")?
            .as_str()
            .map(|state| state == "alert")
    }

    pub fn is_online(&self) -> bool {
        self.state
            .get("online")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn merge_state(&mut self, event_data: &Value) {
        let Some(object) = event_data.as_object() else {
            return;
        };

        for (key, value) in object {
            self.state.insert(key.clone(), value.clone());
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct YoLinkReport {
    pub device_id: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoLinkAppliedEvent {
    pub device_type: String,
    pub event: String,
    pub device_name: String,
    pub transition: Option<StateTransition>,
}

#[derive(Debug, Clone)]
pub struct YoLinkState {
    inner: Arc<RwLock<YoLinkInner>>,
    state_machine: Arc<RwLock<StateMachine>>,
    database_path: PathBuf,
    status_broadcast: Arc<RwLock<Option<broadcast::Sender<()>>>>,
}

#[derive(Debug, Default)]
struct YoLinkInner {
    devices: HashMap<String, YoLinkDevice>,
    door_device_id: Option<String>,
    window_device_id: Option<String>,
    motion_device_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceRole {
    Door,
    Window,
    Motion,
}

impl YoLinkState {
    pub fn new(state_machine: Arc<RwLock<StateMachine>>, database_path: PathBuf) -> Self {
        Self {
            inner: Arc::default(),
            state_machine,
            database_path,
            status_broadcast: Arc::default(),
        }
    }

    pub fn set_status_broadcast(&self, sender: broadcast::Sender<()>) {
        *self
            .status_broadcast
            .write()
            .expect("yolink broadcast lock poisoned") = Some(sender);
    }

    pub fn apply_devices(&self, devices: Vec<YoLinkDevice>) {
        let mut inner = self.inner.write().expect("yolink state lock poisoned");
        inner.devices.clear();
        inner.door_device_id = None;
        inner.window_device_id = None;
        inner.motion_device_id = None;

        for device in devices {
            let name_lower = device.name.to_ascii_lowercase();
            match device.device_type {
                DeviceType::MotionSensor => inner.motion_device_id = Some(device.device_id.clone()),
                DeviceType::DoorSensor if name_lower.contains("door") => {
                    inner.door_device_id = Some(device.device_id.clone());
                }
                DeviceType::DoorSensor if name_lower.contains("window") => {
                    inner.window_device_id = Some(device.device_id.clone());
                }
                _ => {}
            }
            inner.devices.insert(device.device_id.clone(), device);
        }
    }

    pub fn device(&self, device_id: &str) -> Option<YoLinkDevice> {
        self.inner
            .read()
            .expect("yolink state lock poisoned")
            .devices
            .get(device_id)
            .cloned()
    }

    pub fn classified_device_ids(&self) -> (Option<String>, Option<String>, Option<String>) {
        let inner = self.inner.read().expect("yolink state lock poisoned");
        (
            inner.door_device_id.clone(),
            inner.window_device_id.clone(),
            inner.motion_device_id.clone(),
        )
    }

    pub fn apply_report(
        &self,
        report: YoLinkReport,
        now: f64,
    ) -> Result<Option<YoLinkAppliedEvent>> {
        self.apply_event(&report.device_id, report.data, now)
    }

    pub fn apply_event(
        &self,
        device_id: &str,
        event_data: Value,
        now: f64,
    ) -> Result<Option<YoLinkAppliedEvent>> {
        let Some((role, device_name)) = self.record_device_event(device_id, &event_data) else {
            return Ok(None);
        };

        let Some(state_value) = event_data.get("state").and_then(Value::as_str) else {
            return Ok(None);
        };

        let (device_type, event, transition) = match role {
            DeviceRole::Door => {
                let is_open = state_value == "open";
                let transition = self
                    .state_machine
                    .write()
                    .expect("state machine lock poisoned")
                    .update_door(is_open, now);
                ("door", if is_open { "open" } else { "closed" }, transition)
            }
            DeviceRole::Window => {
                let is_open = state_value == "open";
                let transition = self
                    .state_machine
                    .write()
                    .expect("state machine lock poisoned")
                    .update_window(is_open, now);
                (
                    "window",
                    if is_open { "open" } else { "closed" },
                    transition,
                )
            }
            DeviceRole::Motion => {
                let detected = state_value == "alert";
                let transition = self
                    .state_machine
                    .write()
                    .expect("state machine lock poisoned")
                    .update_motion(detected, now);
                (
                    "motion",
                    if detected { "detected" } else { "clear" },
                    transition,
                )
            }
        };

        if let Err(error) = db::log_device_event(
            &self.database_path,
            device_type,
            event,
            Some(&device_name),
            Some(&event_data),
        ) {
            tracing::warn!(
                "failed to persist YoLink {device_type} event after state update: {error:#}"
            );
        }
        self.notify_status();

        Ok(Some(YoLinkAppliedEvent {
            device_type: device_type.to_string(),
            event: event.to_string(),
            device_name,
            transition,
        }))
    }

    fn record_device_event(
        &self,
        device_id: &str,
        event_data: &Value,
    ) -> Option<(DeviceRole, String)> {
        let mut inner = self.inner.write().expect("yolink state lock poisoned");
        let role = if inner.door_device_id.as_deref() == Some(device_id) {
            DeviceRole::Door
        } else if inner.window_device_id.as_deref() == Some(device_id) {
            DeviceRole::Window
        } else if inner.motion_device_id.as_deref() == Some(device_id) {
            DeviceRole::Motion
        } else {
            return None;
        };
        let device = inner.devices.get_mut(device_id)?;
        device.merge_state(event_data);
        Some((role, device.name.clone()))
    }

    pub fn restore_from_database(&self, now: f64) -> Result<()> {
        if let Some(state) = db::get_latest_device_state(&self.database_path, "door")? {
            self.state_machine
                .write()
                .expect("state machine lock poisoned")
                .update_door(state == "open", now);
        }
        if let Some(state) = db::get_latest_device_state(&self.database_path, "window")? {
            self.state_machine
                .write()
                .expect("state machine lock poisoned")
                .update_window(state == "open", now);
        }
        if let Some(state) = db::get_latest_device_state(&self.database_path, "motion")? {
            self.state_machine
                .write()
                .expect("state machine lock poisoned")
                .update_motion(state == "detected", now);
        }
        Ok(())
    }

    pub fn overlay_status(&self, status: &mut Status, now: f64) {
        let machine_status = self
            .state_machine
            .read()
            .expect("state machine lock poisoned")
            .status_at(now);
        status.state = machine_status.state;
        status.is_present = machine_status.is_present;
        status.presence_signal_active = machine_status.presence_signal_active;
        status.safety_interlock = machine_status.safety_interlock;
        status.erv_should_run = machine_status.erv_should_run;
        status.verifying_departure = machine_status.verifying_departure;
        status.in_door_open_mode = machine_status.in_door_open_mode;
        status.sensors.motion_detected = machine_status.sensors.motion_detected;
        status.sensors.door_open = machine_status.sensors.door_open;
        status.sensors.window_open = machine_status.sensors.window_open;
    }

    fn notify_status(&self) {
        let Some(sender) = self
            .status_broadcast
            .read()
            .expect("yolink broadcast lock poisoned")
            .clone()
        else {
            return;
        };
        let _ = sender.send(());
    }
}

pub struct YoLinkCloudClient {
    config: YoLinkConfig,
    http: reqwest::Client,
}

type BoxFutureResult<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait YoLinkApi: Send + Sync {
    fn authenticate(&self) -> BoxFutureResult<'_, String>;
    fn get_home_id<'a>(&'a self, access_token: &'a str) -> BoxFutureResult<'a, String>;
    fn get_devices<'a>(&'a self, access_token: &'a str) -> BoxFutureResult<'a, Vec<YoLinkDevice>>;
}

impl YoLinkCloudClient {
    pub fn new(config: YoLinkConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub async fn authenticate(&self) -> Result<String> {
        let response: Value = self
            .http
            .post(format!("{}/open/yolink/token", self.config.http_url))
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.config.uaid.as_str()),
                ("client_secret", self.config.secret_key.as_str()),
            ])
            .send()
            .await
            .context("failed to call YoLink auth endpoint")?
            .error_for_status()
            .context("YoLink auth endpoint returned an error")?
            .json()
            .await
            .context("failed to decode YoLink auth response")?;

        response
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("YoLink auth response missing access_token"))
    }

    async fn api_call(&self, access_token: &str, method: &str) -> Result<Value> {
        let response: Value = self
            .http
            .post(format!("{}/open/yolink/v2/api", self.config.http_url))
            .bearer_auth(access_token)
            .json(&json!({
                "method": method,
                "time": chrono::Utc::now().timestamp_millis(),
            }))
            .send()
            .await
            .with_context(|| format!("failed to call YoLink API method {method}"))?
            .error_for_status()
            .with_context(|| format!("YoLink API method {method} returned an HTTP error"))?
            .json()
            .await
            .with_context(|| format!("failed to decode YoLink API method {method} response"))?;

        if response.get("code").and_then(Value::as_str) != Some("000000") {
            bail!("YoLink API method {method} returned error: {response}");
        }

        Ok(response)
    }

    pub async fn get_home_id(&self, access_token: &str) -> Result<String> {
        parse_home_id(&self.api_call(access_token, "Home.getGeneralInfo").await?)
    }

    pub async fn get_devices(&self, access_token: &str) -> Result<Vec<YoLinkDevice>> {
        parse_devices(&self.api_call(access_token, "Home.getDeviceList").await?)
    }
}

impl YoLinkApi for YoLinkCloudClient {
    fn authenticate(&self) -> BoxFutureResult<'_, String> {
        Box::pin(async { YoLinkCloudClient::authenticate(self).await })
    }

    fn get_home_id<'a>(&'a self, access_token: &'a str) -> BoxFutureResult<'a, String> {
        Box::pin(async move { YoLinkCloudClient::get_home_id(self, access_token).await })
    }

    fn get_devices<'a>(&'a self, access_token: &'a str) -> BoxFutureResult<'a, Vec<YoLinkDevice>> {
        Box::pin(async move { YoLinkCloudClient::get_devices(self, access_token).await })
    }
}

pub async fn initialize_yolink_inventory(
    client: &dyn YoLinkApi,
    yolink: &YoLinkState,
) -> Result<(String, String)> {
    let access_token = client.authenticate().await?;
    let home_id = client.get_home_id(&access_token).await?;
    let devices = client.get_devices(&access_token).await?;
    yolink.apply_devices(devices);
    Ok((access_token, home_id))
}

pub fn parse_home_id(value: &Value) -> Result<String> {
    value
        .get("data")
        .and_then(|data| data.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("YoLink home response missing data.id"))
}

pub fn parse_devices(value: &Value) -> Result<Vec<YoLinkDevice>> {
    let Some(devices) = value
        .get("data")
        .and_then(|data| data.get("devices"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };

    devices
        .iter()
        .map(|device| {
            let device_id = device
                .get("deviceId")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("YoLink device missing deviceId"))?;
            let device_type =
                DeviceType::from_api(device.get("type").and_then(Value::as_str).unwrap_or(""));
            Ok(YoLinkDevice {
                device_id: device_id.to_string(),
                name: device
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(device_id)
                    .to_string(),
                token: device
                    .get("token")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                device_type,
                state: serde_json::Map::new(),
            })
        })
        .collect()
}

pub fn parse_mqtt_report_payload(payload: &[u8]) -> serde_json::Result<Option<YoLinkReport>> {
    let value: Value = serde_json::from_slice(payload)?;
    let data = value.get("data").cloned().unwrap_or_else(|| json!({}));
    if let Some(device_id) = value.get("deviceId").and_then(Value::as_str) {
        return Ok(Some(YoLinkReport {
            device_id: device_id.to_string(),
            data,
        }));
    }

    let Some(device_id) = data.get("deviceId").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(Some(YoLinkReport {
        device_id: device_id.to_string(),
        data: normalize_report_data(data),
    }))
}

fn normalize_report_data(data: Value) -> Value {
    let Some(state) = data.get("state") else {
        return data;
    };
    if state.as_str().is_some() {
        return data;
    }
    state.clone()
}

pub fn reconnect_delay(config: &YoLinkConfig) -> Duration {
    Duration::from_secs(config.reconnect_delay_seconds.max(1))
}

pub fn start_yolink_client(
    config: &AppConfig,
    yolink: YoLinkState,
    erv_automation: Option<ErvPolicyCoordinator>,
    device_hook: Option<DeviceIngressHook>,
) -> Option<JoinHandle<()>> {
    if !config.yolink.is_configured() {
        tracing::warn!("YoLink credentials are not configured; client not started");
        return None;
    }

    let config = config.clone();
    Some(tokio::spawn(async move {
        loop {
            if let Err(error) = run_yolink_client_once(
                &config,
                yolink.clone(),
                erv_automation.clone(),
                device_hook.clone(),
            )
            .await
            {
                tracing::warn!("YoLink client stopped: {error:#}");
            }
            tokio::time::sleep(reconnect_delay(&config.yolink)).await;
        }
    }))
}

async fn run_yolink_client_once(
    config: &AppConfig,
    yolink: YoLinkState,
    erv_automation: Option<ErvPolicyCoordinator>,
    device_hook: Option<DeviceIngressHook>,
) -> Result<()> {
    let cloud = YoLinkCloudClient::new(config.yolink.clone());
    let (access_token, home_id) = initialize_yolink_inventory(&cloud, &yolink).await?;
    yolink.restore_from_database(chrono::Local::now().timestamp_millis() as f64 / 1_000.0)?;
    listen_yolink_mqtt(
        &config.yolink,
        &access_token,
        &home_id,
        yolink,
        erv_automation,
        device_hook,
    )
    .await
}

async fn listen_yolink_mqtt(
    config: &YoLinkConfig,
    access_token: &str,
    home_id: &str,
    yolink: YoLinkState,
    erv_automation: Option<ErvPolicyCoordinator>,
    device_hook: Option<DeviceIngressHook>,
) -> Result<()> {
    let mut options = MqttOptions::new(
        "office-automate-yolink",
        config.mqtt_host.clone(),
        config.mqtt_port,
    );
    options.set_credentials(access_token, "");
    options.set_keep_alive(Duration::from_secs(30));

    let (client, mut event_loop) = AsyncClient::new(options, 10);
    let topic = format!("yl-home/{home_id}/+/report");
    client
        .subscribe(topic.clone(), QoS::AtLeastOnce)
        .await
        .with_context(|| format!("failed to subscribe to YoLink MQTT topic {topic}"))?;

    loop {
        match event_loop.poll().await.context("YoLink MQTT poll failed")? {
            Event::Incoming(Incoming::Publish(publish)) => {
                match parse_mqtt_report_payload(&publish.payload) {
                    Ok(Some(report)) => {
                        let now = chrono::Local::now().timestamp_millis() as f64 / 1_000.0;
                        if let Err(error) = apply_yolink_report_with_policy(
                            &yolink,
                            report,
                            now,
                            erv_automation.as_ref(),
                            device_hook.as_ref(),
                        )
                        .await
                        {
                            if error.to_string().contains("failed to apply YoLink report") {
                                tracing::warn!("failed to apply YoLink report: {error:#}")
                            } else {
                                tracing::warn!(
                                    "ERV automated policy apply failed after YoLink update: {error:#}"
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => tracing::warn!("failed to parse YoLink MQTT report: {error}"),
                }
            }
            Event::Incoming(_) | Event::Outgoing(_) => {}
        }
    }
}

async fn apply_yolink_report_with_policy(
    yolink: &YoLinkState,
    report: YoLinkReport,
    now: f64,
    erv_automation: Option<&ErvPolicyCoordinator>,
    device_hook: Option<&DeviceIngressHook>,
) -> Result<()> {
    if let Some(erv_automation) = erv_automation {
        let report_transition = Arc::new(Mutex::new(None::<Option<StateTransition>>));
        let report_transition_for_hook = report_transition.clone();
        let policy_result = erv_automation
            .update_state_and_maybe_evaluate(|| {
                let applied = yolink
                    .apply_report(report, now)
                    .context("failed to apply YoLink report")?;
                let Some(applied) = applied else {
                    return Ok((None, None, now, false, false));
                };
                let transition = applied.transition;
                *report_transition_for_hook
                    .lock()
                    .expect("report transition lock") = Some(transition);
                let bypass_dwell = applied.transition.is_some();
                Ok((
                    Some((applied.device_type, transition)),
                    transition,
                    now,
                    bypass_dwell,
                    true,
                ))
            })
            .await;
        match policy_result {
            Ok(applied) => {
                if let Some((_device, transition)) = applied {
                    if let Some(hook) = device_hook {
                        hook(transition);
                    }
                }
            }
            Err(error) => {
                let transition = *report_transition.lock().expect("report transition lock");
                if let Some(transition) = transition {
                    if let Some(hook) = device_hook {
                        hook(transition);
                    }
                }
                return Err(error);
            }
        }
        return Ok(());
    }

    if let Some(applied) = yolink
        .apply_report(report, now)
        .context("failed to apply YoLink report")?
    {
        if let Some(hook) = device_hook {
            hook(applied.transition);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use crate::{
        config::{
            ErvConfig, MitsubishiConfig, OrchestratorConfig, PresenceConfig, QingpingConfig,
            RuntimeConfig, TelemetryConfig, ThresholdsConfig,
        },
        db::migrate_database,
        erv::{ErvDeviceStatus, ErvFanSpeed, ErvSpeedWriter, ErvState},
        policy::ErvPolicyState,
        qingping::QingpingState,
        state::{OccupancyState, StateConfig},
    };
    use anyhow::Result;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct FakeErvWriter {
        smoke_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        write_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        write_speeds: Mutex<Vec<ErvFanSpeed>>,
    }

    impl FakeErvWriter {
        fn new(smoke_results: Vec<Result<ErvDeviceStatus>>) -> Self {
            Self::with_write_results(smoke_results, Vec::new())
        }

        fn with_write_results(
            smoke_results: Vec<Result<ErvDeviceStatus>>,
            write_results: Vec<Result<ErvDeviceStatus>>,
        ) -> Self {
            Self {
                smoke_results: Mutex::new(smoke_results.into()),
                write_results: Mutex::new(write_results.into()),
                write_speeds: Mutex::new(Vec::new()),
            }
        }

        fn write_speeds(&self) -> Vec<ErvFanSpeed> {
            self.write_speeds.lock().expect("write speeds lock").clone()
        }
    }

    impl ErvSpeedWriter for FakeErvWriter {
        fn smoke_status<'a>(
            &'a self,
            _config: &'a ErvConfig,
        ) -> crate::erv::BoxFutureResult<'a, ErvDeviceStatus> {
            let result = self
                .smoke_results
                .lock()
                .expect("smoke results lock")
                .pop_front()
                .unwrap_or_else(|| Ok(erv_status(ErvFanSpeed::Off)));
            Box::pin(async move { result })
        }

        fn set_speed<'a>(
            &'a self,
            _config: &'a ErvConfig,
            speed: ErvFanSpeed,
        ) -> crate::erv::BoxFutureResult<'a, ErvDeviceStatus> {
            self.write_speeds
                .lock()
                .expect("write speeds lock")
                .push(speed);
            let result = self
                .write_results
                .lock()
                .expect("write results lock")
                .pop_front()
                .unwrap_or_else(|| Ok(erv_status(speed)));
            Box::pin(async move { result })
        }
    }

    fn test_state(database_path: PathBuf) -> YoLinkState {
        YoLinkState::new(
            Arc::new(RwLock::new(StateMachine::new(
                StateConfig::from_thresholds(&ThresholdsConfig::default()),
                1_000.0,
            ))),
            database_path,
        )
    }

    fn test_config(database_path: PathBuf) -> AppConfig {
        let root = database_path
            .parent()
            .expect("database parent")
            .to_path_buf();
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            presence: PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            erv: ErvConfig {
                device_type: "tuya".to_string(),
                ip: "192.0.2.10".to_string(),
                device_id: "device-id".to_string(),
                local_key: "local-key".to_string(),
                active_control_enabled: true,
                verify_delay_seconds: 0,
                ..ErvConfig::default()
            },
            mitsubishi: MitsubishiConfig::default(),
            thresholds: ThresholdsConfig {
                erv_min_dwell_seconds: 0,
                ..ThresholdsConfig::default()
            },
            telemetry: TelemetryConfig::default(),
            runtime: RuntimeConfig {
                root: root.clone(),
                config_path: root.join("config.yaml"),
                data_dir: root.clone(),
                database_path,
                frontend_dist: root.join("frontend/dist"),
                artifacts_dir: root.join("apps"),
                legacy_apk_path: root.join("app-debug.apk"),
                base_url: None,
                public_url: None,
                mqtt_host: "127.0.0.1".to_string(),
                mqtt_port: 1883,
                telemetry_db_path: root.join("telemetry.db"),
                tool_usage_db_path: root.join("tool-usage.db"),
                engram_db_path: root.join("engram.db"),
                engram_registry_path: root.join("engram-registry.json"),
            },
        }
    }

    fn erv_status(speed: ErvFanSpeed) -> ErvDeviceStatus {
        let (power, supply_speed, exhaust_speed) = match speed {
            ErvFanSpeed::Off => (false, Some(1), Some(1)),
            ErvFanSpeed::Quiet => (true, Some(1), Some(1)),
            ErvFanSpeed::Medium => (true, Some(3), Some(2)),
            ErvFanSpeed::Turbo => (true, Some(8), Some(8)),
        };
        ErvDeviceStatus {
            power,
            fan_speed: Some(speed),
            supply_speed,
            exhaust_speed,
            raw_dps: json!({}),
        }
    }

    fn sample_devices() -> Vec<YoLinkDevice> {
        parse_devices(&json!({
            "data": {
                "devices": [
                    {"deviceId": "door-1", "name": "Office Door", "token": "door-token", "type": "DoorSensor"},
                    {"deviceId": "window-1", "name": "Office Window", "token": "window-token", "type": "DoorSensor"},
                    {"deviceId": "motion-1", "name": "Office Motion", "token": "motion-token", "type": "MotionSensor"}
                ]
            }
        }))
        .expect("devices")
    }

    struct FakeYoLinkApi;

    impl YoLinkApi for FakeYoLinkApi {
        fn authenticate(&self) -> BoxFutureResult<'_, String> {
            Box::pin(async { Ok("fake-token".to_string()) })
        }

        fn get_home_id<'a>(&'a self, access_token: &'a str) -> BoxFutureResult<'a, String> {
            Box::pin(async move {
                assert_eq!(access_token, "fake-token");
                Ok("home-123".to_string())
            })
        }

        fn get_devices<'a>(
            &'a self,
            access_token: &'a str,
        ) -> BoxFutureResult<'a, Vec<YoLinkDevice>> {
            Box::pin(async move {
                assert_eq!(access_token, "fake-token");
                Ok(sample_devices())
            })
        }
    }

    #[test]
    fn parses_home_and_device_inventory() {
        assert_eq!(
            parse_home_id(&json!({"data": {"id": "home-123"}})).expect("home id"),
            "home-123"
        );

        let devices = sample_devices();
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].device_id, "door-1");
        assert_eq!(devices[0].device_type, DeviceType::DoorSensor);
        assert_eq!(devices[2].device_type, DeviceType::MotionSensor);
    }

    #[tokio::test]
    async fn initializes_inventory_through_testable_api_client() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let yolink = test_state(temp_dir.path().join("office_climate.db"));

        let (token, home_id) = initialize_yolink_inventory(&FakeYoLinkApi, &yolink)
            .await
            .expect("initialize");

        assert_eq!(token, "fake-token");
        assert_eq!(home_id, "home-123");
        assert_eq!(
            yolink.classified_device_ids(),
            (
                Some("door-1".to_string()),
                Some("window-1".to_string()),
                Some("motion-1".to_string())
            )
        );
    }

    #[tokio::test]
    async fn authenticates_with_form_encoded_token_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind auth server");
        let address = listener.local_addr().expect("auth server address");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept auth request");
            let mut buffer = vec![0_u8; 4096];
            let mut read = 0_usize;
            loop {
                let n = stream
                    .read(&mut buffer[read..])
                    .await
                    .expect("read auth request");
                if n == 0 {
                    break;
                }
                read += n;
                let request = String::from_utf8_lossy(&buffer[..read]);
                if let Some(header_end) = request.find("\r\n\r\n") {
                    let content_length = request
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or_default();
                    if read >= header_end + 4 + content_length {
                        break;
                    }
                }
                if read == buffer.len() {
                    buffer.resize(buffer.len() * 2, 0);
                }
            }

            let request = String::from_utf8(buffer[..read].to_vec()).expect("utf8 request");
            request_tx.send(request).expect("send captured request");
            let body = r#"{"access_token":"token-123"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write auth response");
        });

        let client = YoLinkCloudClient::new(YoLinkConfig {
            uaid: "client id".to_string(),
            secret_key: "secret/key".to_string(),
            http_url: format!("http://{address}"),
            ..YoLinkConfig::default()
        });

        let token = client.authenticate().await.expect("authenticate");
        assert_eq!(token, "token-123");
        let request = request_rx.await.expect("captured request");
        let lower_request = request.to_ascii_lowercase();
        assert!(request.starts_with("POST /open/yolink/token HTTP/1.1"));
        assert!(lower_request.contains("content-type: application/x-www-form-urlencoded"));
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        assert!(body.contains("grant_type=client_credentials"));
        assert!(body.contains("client_id=client+id"));
        assert!(body.contains("client_secret=secret%2Fkey"));

        server.await.expect("auth server task");
    }

    #[test]
    fn classifies_devices_by_python_name_rules() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let yolink = test_state(temp_dir.path().join("office_climate.db"));
        yolink.apply_devices(sample_devices());

        assert_eq!(
            yolink.classified_device_ids(),
            (
                Some("door-1".to_string()),
                Some("window-1".to_string()),
                Some("motion-1".to_string())
            )
        );
    }

    #[test]
    fn parses_mqtt_report_payload() {
        let report = parse_mqtt_report_payload(br#"{"deviceId":"door-1","data":{"state":"open"}}"#)
            .expect("parse")
            .expect("report");

        assert_eq!(report.device_id, "door-1");
        assert_eq!(report.data["state"], "open");

        let documented_report = parse_mqtt_report_payload(
            br#"{"event":"DoorSensor.Report","data":{"deviceId":"door-1","state":{"state":"open","battery":4}}}"#,
        )
        .expect("parse documented")
        .expect("documented report");
        assert_eq!(documented_report.device_id, "door-1");
        assert_eq!(documented_report.data["state"], "open");
        assert_eq!(documented_report.data["battery"], 4);

        assert_eq!(
            parse_mqtt_report_payload(br#"{"data":{"state":"open"}}"#).expect("parse"),
            None
        );
    }

    #[test]
    fn applies_door_window_and_motion_events_to_state_machine_and_database() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");
        let yolink = test_state(db_path.clone());
        yolink.apply_devices(sample_devices());

        let door = yolink
            .apply_event("door-1", json!({"state": "open"}), 1_010.0)
            .expect("door event")
            .expect("applied");
        let window = yolink
            .apply_event("window-1", json!({"state": "open"}), 1_011.0)
            .expect("window event")
            .expect("applied");
        let motion = yolink
            .apply_event("motion-1", json!({"state": "alert"}), 1_012.0)
            .expect("motion event")
            .expect("applied");

        assert_eq!(door.device_type, "door");
        assert_eq!(door.event, "open");
        assert_eq!(window.device_type, "window");
        assert_eq!(window.event, "open");
        assert_eq!(motion.device_type, "motion");
        assert_eq!(motion.event, "detected");

        let machine = yolink
            .state_machine
            .read()
            .expect("state machine lock poisoned");
        assert!(machine.sensors.door_open);
        assert!(machine.sensors.window_open);
        assert!(machine.sensors.motion_detected);
        drop(machine);

        assert_eq!(
            db::get_latest_device_state(&db_path, "door").expect("door state"),
            Some("open".to_string())
        );
        assert_eq!(
            db::get_latest_device_state(&db_path, "motion").expect("motion state"),
            Some("detected".to_string())
        );
    }

    #[test]
    fn event_logging_failure_still_returns_applied_event() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let yolink = test_state(temp_dir.path().to_path_buf());
        yolink.apply_devices(sample_devices());

        let applied = yolink
            .apply_event("door-1", json!({"state": "open"}), 1_010.0)
            .expect("state update survives logging failure")
            .expect("applied");

        assert_eq!(applied.device_type, "door");
        assert_eq!(applied.event, "open");
        assert!(
            yolink
                .state_machine
                .read()
                .expect("state machine lock poisoned")
                .sensors
                .door_open
        );
    }

    #[tokio::test]
    async fn applying_report_notifies_status_broadcast() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");
        let yolink = test_state(db_path);
        yolink.apply_devices(sample_devices());
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        yolink.set_status_broadcast(sender);

        let applied = yolink
            .apply_report(
                YoLinkReport {
                    device_id: "door-1".to_string(),
                    data: json!({"state": "open"}),
                },
                1_010.0,
            )
            .expect("apply report");

        assert!(applied.is_some());
        tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("broadcast timeout")
            .expect("broadcast received");
    }

    #[tokio::test]
    async fn device_hook_runs_after_erv_policy_update() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");
        let config = test_config(db_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::new(
            StateConfig::from_thresholds(&config.thresholds),
            1_000.0,
        )));
        let yolink = YoLinkState::new(state_machine.clone(), db_path.clone());
        yolink.apply_devices(sample_devices());
        let qingping = QingpingState::default();
        let erv = ErvState::new(db_path);
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(
            ErvFanSpeed::Medium,
        ))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = tokio::sync::broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv.clone(),
            policy,
            writer.clone(),
            status_broadcast,
        );
        let hook_observations = Arc::new(Mutex::new(Vec::new()));
        let hook: DeviceIngressHook = Arc::new({
            let erv = erv.clone();
            let hook_observations = hook_observations.clone();
            move |transition| {
                hook_observations
                    .lock()
                    .expect("hook observations lock")
                    .push((transition, erv.snapshot().running));
            }
        });

        apply_yolink_report_with_policy(
            &yolink,
            YoLinkReport {
                device_id: "motion-1".to_string(),
                data: json!({"state": "alert"}),
            },
            1_002.0,
            Some(&coordinator),
            Some(&hook),
        )
        .await
        .expect("report applies with policy");

        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Off]);
        assert_eq!(
            *hook_observations.lock().expect("hook observations lock"),
            vec![(
                Some(StateTransition {
                    old_state: OccupancyState::Away,
                    new_state: OccupancyState::Present,
                }),
                false,
            )]
        );
    }

    #[tokio::test]
    async fn device_hook_runs_when_erv_policy_write_fails_after_report_applies() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");
        let config = test_config(db_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::new(
            StateConfig::from_thresholds(&config.thresholds),
            1_000.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, 1_001.0);
        let yolink = YoLinkState::new(state_machine.clone(), db_path.clone());
        yolink.apply_devices(sample_devices());
        let qingping = QingpingState::default();
        let erv = ErvState::new(db_path);
        let writer = Arc::new(FakeErvWriter::with_write_results(
            vec![Ok(erv_status(ErvFanSpeed::Medium))],
            vec![Err(anyhow::anyhow!("ERV write failed"))],
        ));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = tokio::sync::broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine.clone(),
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );
        let hook_observations = Arc::new(Mutex::new(Vec::new()));
        let hook: DeviceIngressHook = Arc::new({
            let state_machine = state_machine.clone();
            let hook_observations = hook_observations.clone();
            move |_transition| {
                let safety_interlock = state_machine
                    .read()
                    .expect("state machine lock poisoned")
                    .status_at(1_002.0)
                    .safety_interlock;
                hook_observations
                    .lock()
                    .expect("hook observations lock")
                    .push(safety_interlock);
            }
        });

        let result = apply_yolink_report_with_policy(
            &yolink,
            YoLinkReport {
                device_id: "door-1".to_string(),
                data: json!({"state": "open"}),
            },
            1_002.0,
            Some(&coordinator),
            Some(&hook),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Off]);
        assert_eq!(
            *hook_observations.lock().expect("hook observations lock"),
            vec![true]
        );
    }

    #[test]
    fn restores_device_state_from_database() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");
        db::log_device_event(&db_path, "door", "open", Some("Office Door"), None)
            .expect("log door");
        db::log_device_event(&db_path, "window", "closed", Some("Office Window"), None)
            .expect("log window");
        db::log_device_event(&db_path, "motion", "detected", Some("Office Motion"), None)
            .expect("log motion");

        let yolink = test_state(db_path);
        yolink.restore_from_database(1_050.0).expect("restore");

        let machine = yolink
            .state_machine
            .read()
            .expect("state machine lock poisoned");
        assert!(machine.sensors.door_open);
        assert!(!machine.sensors.window_open);
        assert!(machine.sensors.motion_detected);
        assert_eq!(machine.state, OccupancyState::Away);
    }

    #[test]
    fn reconnect_delay_uses_configured_minimum() {
        assert_eq!(
            reconnect_delay(&YoLinkConfig {
                reconnect_delay_seconds: 10,
                ..YoLinkConfig::default()
            }),
            Duration::from_secs(10)
        );
        assert_eq!(
            reconnect_delay(&YoLinkConfig {
                reconnect_delay_seconds: 0,
                ..YoLinkConfig::default()
            }),
            Duration::from_secs(1)
        );
    }
}
