use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use rumqttc::{AsyncClient, Event, MqttOptions, Outgoing, QoS};
use rumqttd::{
    Broker, Config as BrokerConfig, ConnectionSettings, Notification, RouterConfig, ServerSettings,
};
use tokio::time;

use crate::{
    config::{AppConfig, QingpingConfig},
    db,
    qingping::{
        MAX_QINGPING_PAYLOAD_BYTES, QingpingReading, QingpingState, parse_qingping_payload,
        qingping_down_topic, qingping_interval_payload, qingping_up_topic,
    },
};

const QINGPING_MIN_ACCEPT_INTERVAL: Duration = Duration::from_secs(2);
const QINGPING_MAX_CO2_DELTA: i64 = 4_000;
const QINGPING_MAX_TVOC_DELTA: i64 = 500;
const QINGPING_MAX_PM_DELTA: i64 = 500;
const QINGPING_MAX_TEMP_DELTA: f64 = 12.0;
const QINGPING_MAX_HUMIDITY_DELTA: f64 = 50.0;

pub struct MqttRuntime {
    _broker_thread: JoinHandle<()>,
    _ingest_thread: JoinHandle<()>,
}

pub type SensorIngressHook = Arc<dyn Fn() + Send + Sync + 'static>;

pub async fn publish_qingping_interval(config: &AppConfig, interval_seconds: i64) -> Result<()> {
    let device_mac = config
        .qingping
        .device_mac
        .as_deref()
        .context("qingping.device_mac is required to publish interval commands")?;
    let topic = qingping_down_topic(device_mac);
    let payload = serde_json::to_vec(&qingping_interval_payload(interval_seconds))
        .context("failed to serialize Qingping interval command")?;
    publish_local_qingping_command(
        &config.runtime.mqtt_host,
        config.runtime.mqtt_port,
        &config.qingping,
        &topic,
        payload,
    )
    .await
    .with_context(|| format!("failed to publish Qingping interval command to {topic}"))?;
    Ok(())
}

async fn publish_local_qingping_command(
    host: &str,
    port: u16,
    qingping: &QingpingConfig,
    topic: &str,
    payload: Vec<u8>,
) -> Result<()> {
    let client_id = format!(
        "office-automate-qingping-command-{}",
        chrono::Local::now().timestamp_millis()
    );
    let mut options = MqttOptions::new(client_id, (host.trim(), port));
    options.set_keep_alive(5);
    if let Some((username, password)) = qingping_mqtt_credentials(qingping)? {
        options.set_credentials(username, password);
    }

    let (client, mut event_loop) = AsyncClient::builder(options).capacity(10).build();
    client
        .publish(topic, QoS::AtMostOnce, false, payload)
        .await
        .context("failed to queue MQTT publish")?;

    time::timeout(Duration::from_secs(5), async {
        loop {
            match event_loop
                .poll()
                .await
                .context("MQTT publish poll failed")?
            {
                Event::Outgoing(Outgoing::Publish(_)) => return Ok::<_, anyhow::Error>(()),
                Event::Incoming(_) | Event::Outgoing(_) => {}
            }
        }
    })
    .await
    .context("timed out waiting for MQTT publish")??;

    let _ = client.disconnect().await;
    Ok(())
}

pub fn start_qingping_ingress(
    config: &AppConfig,
    qingping: QingpingState,
    reading_hook: Option<SensorIngressHook>,
) -> Result<Option<MqttRuntime>> {
    let Some(device_mac) = config.qingping.device_mac.as_deref() else {
        tracing::warn!("Qingping device_mac is not configured; MQTT broker not started");
        return Ok(None);
    };

    let topic = qingping_up_topic(device_mac);
    let broker_config = build_broker_config(
        &config.runtime.mqtt_host,
        config.runtime.mqtt_port,
        &config.qingping,
    )?;
    let database_path = config.runtime.database_path.clone();
    let configured_mac = device_mac.to_string();
    let office_trusted = config.room_mode.air_quality_sensors_enabled;
    let guard = Arc::new(Mutex::new(QingpingIngressGuard::default()));

    let mut broker = Broker::new(broker_config);
    let (mut link_tx, mut link_rx) = broker
        .link("office-automate-qingping")
        .context("failed to create internal MQTT link")?;
    link_tx
        .subscribe(topic.clone())
        .with_context(|| format!("failed to subscribe to {topic}"))?;

    let ingest_thread = thread::Builder::new()
        .name("qingping-mqtt-ingest".to_string())
        .spawn(move || {
            let _link_tx = link_tx;
            ingest_loop(
                &mut link_rx,
                &topic,
                &configured_mac,
                database_path,
                qingping,
                guard,
                reading_hook,
                office_trusted,
            );
        })
        .context("failed to spawn Qingping MQTT ingest thread")?;

    let broker_thread = thread::Builder::new()
        .name("qingping-mqtt-broker".to_string())
        .spawn(move || {
            if let Err(error) = broker.start() {
                tracing::error!("Qingping MQTT broker stopped: {error:#}");
            }
        })
        .context("failed to spawn Qingping MQTT broker thread")?;

    Ok(Some(MqttRuntime {
        _broker_thread: broker_thread,
        _ingest_thread: ingest_thread,
    }))
}

fn ingest_loop(
    link_rx: &mut rumqttd::local::LinkRx,
    topic: &str,
    configured_mac: &str,
    database_path: PathBuf,
    qingping: QingpingState,
    guard: Arc<Mutex<QingpingIngressGuard>>,
    reading_hook: Option<SensorIngressHook>,
    office_trusted: bool,
) {
    loop {
        match link_rx.recv() {
            Ok(Some(Notification::Forward(forward))) => {
                if forward.publish.topic.as_ref() != topic.as_bytes() {
                    continue;
                }
                handle_qingping_publish(
                    &forward.publish.payload,
                    configured_mac,
                    &database_path,
                    &qingping,
                    &guard,
                    reading_hook.as_ref(),
                    office_trusted,
                );
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                tracing::warn!("Qingping MQTT internal link stopped: {error}");
                break;
            }
        }
    }
}

fn handle_qingping_publish(
    payload: &[u8],
    configured_mac: &str,
    database_path: &Path,
    qingping: &QingpingState,
    guard: &Arc<Mutex<QingpingIngressGuard>>,
    reading_hook: Option<&SensorIngressHook>,
    office_trusted: bool,
) {
    match parse_qingping_payload(payload, configured_mac) {
        Ok(Some(reading)) => {
            if let Err(error) = guard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .accept(&reading)
            {
                tracing::warn!("rejected Qingping MQTT reading: {error}");
                return;
            }
            let ignored_reason = (!office_trusted).then_some("renovation_air_quality_sensor_moved");
            if let Err(error) = db::insert_sensor_reading_with_trust(
                database_path,
                &reading,
                office_trusted,
                ignored_reason,
            ) {
                tracing::warn!("failed to store Qingping reading: {error:#}");
            }
            qingping.apply_reading(reading);
            if let Some(hook) = reading_hook {
                hook();
            }
        }
        Ok(None) => {
            tracing::debug!("ignoring Qingping MQTT message without sensor data");
        }
        Err(error) => {
            tracing::warn!("failed to parse Qingping MQTT payload: {error}");
        }
    }
}

#[derive(Default)]
struct QingpingIngressGuard {
    last_accepted_at: Option<Instant>,
    last_reading: Option<QingpingReading>,
}

impl QingpingIngressGuard {
    fn accept(&mut self, reading: &QingpingReading) -> Result<()> {
        let now = Instant::now();
        if let Some(last_accepted_at) = self.last_accepted_at {
            let elapsed = now.saturating_duration_since(last_accepted_at);
            if elapsed < QINGPING_MIN_ACCEPT_INTERVAL {
                anyhow::bail!(
                    "Qingping payload rate limit exceeded: accepted interval {:?} < {:?}",
                    elapsed,
                    QINGPING_MIN_ACCEPT_INTERVAL
                );
            }
        }
        if let Some(previous) = &self.last_reading {
            validate_delta_i64(
                "co2",
                previous.co2_ppm,
                reading.co2_ppm,
                QINGPING_MAX_CO2_DELTA,
            )?;
            validate_delta_i64("tvoc", previous.tvoc, reading.tvoc, QINGPING_MAX_TVOC_DELTA)?;
            validate_delta_i64("pm25", previous.pm25, reading.pm25, QINGPING_MAX_PM_DELTA)?;
            validate_delta_i64("pm10", previous.pm10, reading.pm10, QINGPING_MAX_PM_DELTA)?;
            validate_delta_f64(
                "temperature",
                previous.temp_c,
                reading.temp_c,
                QINGPING_MAX_TEMP_DELTA,
            )?;
            validate_delta_f64(
                "humidity",
                previous.humidity,
                reading.humidity,
                QINGPING_MAX_HUMIDITY_DELTA,
            )?;
        }
        self.last_accepted_at = Some(now);
        self.last_reading = Some(reading.clone());
        Ok(())
    }
}

fn validate_delta_i64(
    field: &str,
    previous: Option<i64>,
    current: Option<i64>,
    max_delta: i64,
) -> Result<()> {
    if let (Some(previous), Some(current)) = (previous, current) {
        let delta = previous.abs_diff(current);
        if delta > max_delta as u64 {
            anyhow::bail!("Qingping {field} delta {delta} exceeds accepted maximum {max_delta}");
        }
    }
    Ok(())
}

fn validate_delta_f64(
    field: &str,
    previous: Option<f64>,
    current: Option<f64>,
    max_delta: f64,
) -> Result<()> {
    if let (Some(previous), Some(current)) = (previous, current) {
        let delta = (previous - current).abs();
        if delta > max_delta {
            anyhow::bail!(
                "Qingping {field} delta {delta:.1} exceeds accepted maximum {max_delta:.1}"
            );
        }
    }
    Ok(())
}

pub(crate) fn build_broker_config(
    host: &str,
    port: u16,
    qingping: &QingpingConfig,
) -> Result<BrokerConfig> {
    let listen = resolve_listen_address(host, port)?;
    let up_topic = qingping
        .device_mac
        .as_deref()
        .map(qingping_up_topic)
        .unwrap_or_else(|| "qingping/unconfigured/up".to_string());
    let down_topic = qingping
        .device_mac
        .as_deref()
        .map(qingping_down_topic)
        .unwrap_or_else(|| "qingping/unconfigured/down".to_string());
    let auth = qingping_mqtt_auth(qingping)?;
    let mut v4 = HashMap::new();
    v4.insert(
        "qingping".to_string(),
        ServerSettings {
            name: "qingping-v4".to_string(),
            listen,
            tls: None,
            next_connection_delay_ms: 1,
            connections: ConnectionSettings {
                connection_timeout_ms: 60_000,
                max_payload_size: MAX_QINGPING_PAYLOAD_BYTES,
                max_inflight_count: 4,
                auth,
                external_auth: None,
                dynamic_filters: false,
            },
        },
    );

    Ok(BrokerConfig {
        id: 0,
        router: RouterConfig {
            max_connections: 8,
            max_outgoing_packet_count: 200,
            max_segment_size: 1024 * 1024,
            max_segment_count: 10,
            custom_segment: None,
            initialized_filters: Some(vec![up_topic, down_topic]),
            shared_subscriptions_strategy: rumqttd::Strategy::RoundRobin,
        },
        v4: Some(v4),
        v5: None,
        ws: None,
        cluster: None,
        console: None,
        bridge: None,
        prometheus: None,
        metrics: None,
    })
}

fn qingping_mqtt_auth(qingping: &QingpingConfig) -> Result<Option<HashMap<String, String>>> {
    match qingping_mqtt_credentials(qingping)? {
        Some((username, password)) => Ok(Some(HashMap::from([(username, password)]))),
        None => Ok(None),
    }
}

fn qingping_mqtt_credentials(qingping: &QingpingConfig) -> Result<Option<(String, String)>> {
    match (
        qingping.mqtt_username.as_deref().map(str::trim),
        qingping.mqtt_password.as_deref().map(str::trim),
    ) {
        (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
            Ok(Some((username.to_string(), password.to_string())))
        }
        (None, None) => Ok(None),
        (Some(""), None) | (None, Some("")) | (Some(""), Some("")) => Ok(None),
        _ => anyhow::bail!(
            "Qingping MQTT auth requires both qingping.mqtt_username and qingping.mqtt_password"
        ),
    }
}

fn resolve_listen_address(host: &str, port: u16) -> Result<SocketAddr> {
    format!("{}:{port}", host.trim())
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve MQTT listen address {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow!("no MQTT listen address resolved for {host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::db::migrate_database;
    use crate::qingping::{normalize_device_mac, qingping_up_topic};

    #[test]
    fn broker_config_uses_configured_listener() {
        let qingping = QingpingConfig {
            device_mac: Some("aa:bb:cc:dd:ee:ff".to_string()),
            ..QingpingConfig::default()
        };
        let config = build_broker_config("127.0.0.1", 2883, &qingping).expect("broker config");
        let v4 = config.v4.expect("v4 config");
        let server = v4.get("qingping").expect("qingping server");

        assert_eq!(server.listen, "127.0.0.1:2883".parse().expect("addr"));
        assert_eq!(
            server.connections.max_payload_size,
            MAX_QINGPING_PAYLOAD_BYTES
        );
        assert_eq!(config.router.max_connections, 8);
        assert_eq!(
            config.router.initialized_filters.expect("filters"),
            vec![
                "qingping/AABBCCDDEEFF/up".to_string(),
                "qingping/AABBCCDDEEFF/down".to_string()
            ]
        );
        assert!(!server.connections.dynamic_filters);
    }

    #[test]
    fn qingping_topic_normalizes_device_mac() {
        assert_eq!(normalize_device_mac("aa:bb-cc"), "AABBCC");
        assert_eq!(
            qingping_up_topic("aa:bb:cc:dd:ee:ff"),
            "qingping/AABBCCDDEEFF/up"
        );
    }

    #[test]
    fn qingping_publish_hook_runs_after_valid_sensor_reading() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        migrate_database(&database_path).expect("migration");
        let qingping = QingpingState::default();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let hook: SensorIngressHook = Arc::new({
            let hook_calls = hook_calls.clone();
            move || {
                hook_calls.fetch_add(1, Ordering::SeqCst);
            }
        });

        handle_qingping_publish(
            br#"{"sensorData":[{"co2":{"value":2100},"temperature":{"value":22.5},"tvoc":{"value":25}}]}"#,
            "aa:bb:cc:dd:ee:ff",
            &database_path,
            &qingping,
            &Arc::new(Mutex::new(QingpingIngressGuard::default())),
            Some(&hook),
            true,
        );

        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
        assert_eq!(qingping.latest().expect("reading").co2_ppm, Some(2100));
    }

    #[test]
    fn qingping_publish_marks_untrusted_air_quality_rows() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        migrate_database(&database_path).expect("migration");
        let qingping = QingpingState::default();

        handle_qingping_publish(
            br#"{"sensorData":[{"co2":{"value":900},"temperature":{"value":23.5},"tvoc":{"value":30}}]}"#,
            "aa:bb:cc:dd:ee:ff",
            &database_path,
            &qingping,
            &Arc::new(Mutex::new(QingpingIngressGuard::default())),
            None,
            false,
        );

        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert_eq!(history.sensor_readings.len(), 1);
        assert_eq!(history.sensor_readings[0]["co2_ppm"], 900);
        assert_eq!(history.sensor_readings[0]["office_trusted"], 0);
        assert_eq!(
            history.sensor_readings[0]["ignored_reason"],
            "renovation_air_quality_sensor_moved"
        );
    }

    #[test]
    fn mqtt_config_requires_auth_pair_when_partially_configured() {
        let qingping = QingpingConfig {
            mqtt_username: Some("device".to_string()),
            ..QingpingConfig::default()
        };

        let error = build_broker_config("127.0.0.1", 2883, &qingping)
            .expect_err("partial auth should fail");

        assert!(error.to_string().contains("requires both"));
    }

    #[test]
    fn mqtt_command_publisher_uses_configured_credentials() {
        let qingping = QingpingConfig {
            mqtt_username: Some(" device ".to_string()),
            mqtt_password: Some(" secret ".to_string()),
            ..QingpingConfig::default()
        };

        assert_eq!(
            qingping_mqtt_credentials(&qingping).expect("credentials"),
            Some(("device".to_string(), "secret".to_string()))
        );
    }

    #[test]
    fn ingress_guard_rejects_bursts_and_large_deltas() {
        let mut guard = QingpingIngressGuard::default();
        let first = QingpingReading {
            device_name: "Qingping Air Monitor".to_string(),
            mac_hint: "AABBCCDDEEFF".to_string(),
            temp_c: Some(22.0),
            humidity: Some(40.0),
            co2_ppm: Some(700),
            pm25: Some(1),
            pm10: Some(1),
            tvoc: Some(10),
            noise_db: None,
            timestamp: "2026-06-07T00:00:00".to_string(),
            raw_data: "{}".to_string(),
        };
        guard.accept(&first).expect("first reading accepted");
        assert!(
            guard
                .accept(&first)
                .expect_err("burst rejected")
                .to_string()
                .contains("rate limit")
        );

        guard.last_accepted_at = Some(Instant::now() - Duration::from_secs(60));
        let mut bad_delta = first.clone();
        bad_delta.co2_ppm = Some(6_000);
        assert!(
            guard
                .accept(&bad_delta)
                .expect_err("large delta rejected")
                .to_string()
                .contains("co2 delta")
        );
    }
}
