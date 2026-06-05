use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::{
    config::{AppConfig, YoLinkConfig},
    db,
    state::{StateMachine, StateTransition},
    status::Status,
};

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
        }
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

        db::log_device_event(
            &self.database_path,
            device_type,
            event,
            Some(&device_name),
            Some(&event_data),
        )?;

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
            .json(&json!({
                "grant_type": "client_credentials",
                "client_id": self.config.uaid,
                "client_secret": self.config.secret_key,
            }))
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
    let Some(device_id) = value.get("deviceId").and_then(Value::as_str) else {
        return Ok(None);
    };
    let data = value.get("data").cloned().unwrap_or_else(|| json!({}));
    Ok(Some(YoLinkReport {
        device_id: device_id.to_string(),
        data,
    }))
}

pub fn reconnect_delay(config: &YoLinkConfig) -> Duration {
    Duration::from_secs(config.reconnect_delay_seconds.max(1))
}

pub fn start_yolink_client(config: &AppConfig, yolink: YoLinkState) -> Option<JoinHandle<()>> {
    if !config.yolink.is_configured() {
        tracing::warn!("YoLink credentials are not configured; client not started");
        return None;
    }

    let config = config.clone();
    Some(tokio::spawn(async move {
        loop {
            if let Err(error) = run_yolink_client_once(&config, yolink.clone()).await {
                tracing::warn!("YoLink client stopped: {error:#}");
            }
            tokio::time::sleep(reconnect_delay(&config.yolink)).await;
        }
    }))
}

async fn run_yolink_client_once(config: &AppConfig, yolink: YoLinkState) -> Result<()> {
    let cloud = YoLinkCloudClient::new(config.yolink.clone());
    let (access_token, home_id) = initialize_yolink_inventory(&cloud, &yolink).await?;
    yolink.restore_from_database(chrono::Local::now().timestamp_millis() as f64 / 1_000.0)?;
    listen_yolink_mqtt(&config.yolink, &access_token, &home_id, yolink).await
}

async fn listen_yolink_mqtt(
    config: &YoLinkConfig,
    access_token: &str,
    home_id: &str,
    yolink: YoLinkState,
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
                        if let Err(error) = yolink.apply_report(report, now) {
                            tracing::warn!("failed to apply YoLink report: {error:#}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ThresholdsConfig,
        db::migrate_database,
        state::{OccupancyState, StateConfig},
    };

    fn test_state(database_path: PathBuf) -> YoLinkState {
        YoLinkState::new(
            Arc::new(RwLock::new(StateMachine::new(
                StateConfig::from_thresholds(&ThresholdsConfig::default()),
                1_000.0,
            ))),
            database_path,
        )
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
