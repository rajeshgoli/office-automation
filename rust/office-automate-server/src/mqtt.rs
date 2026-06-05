use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use rumqttd::{
    Broker, Config as BrokerConfig, ConnectionSettings, Notification, RouterConfig, ServerSettings,
};

use crate::{
    config::AppConfig,
    db,
    qingping::{QingpingState, parse_qingping_payload, qingping_up_topic},
};

pub struct MqttRuntime {
    _broker_thread: JoinHandle<()>,
    _ingest_thread: JoinHandle<()>,
}

pub fn start_qingping_ingress(
    config: &AppConfig,
    qingping: QingpingState,
) -> Result<Option<MqttRuntime>> {
    let Some(device_mac) = config.qingping.device_mac.as_deref() else {
        tracing::warn!("Qingping device_mac is not configured; MQTT broker not started");
        return Ok(None);
    };

    let topic = qingping_up_topic(device_mac);
    let broker_config = build_broker_config(&config.runtime.mqtt_host, config.runtime.mqtt_port)?;
    let database_path = config.runtime.database_path.clone();
    let configured_mac = device_mac.to_string();

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
) {
    loop {
        match link_rx.recv() {
            Ok(Some(Notification::Forward(forward))) => {
                if forward.publish.topic.as_ref() != topic.as_bytes() {
                    continue;
                }
                match parse_qingping_payload(&forward.publish.payload, configured_mac) {
                    Ok(Some(reading)) => {
                        if let Err(error) = db::insert_sensor_reading(&database_path, &reading) {
                            tracing::warn!("failed to store Qingping reading: {error:#}");
                        }
                        qingping.apply_reading(reading);
                    }
                    Ok(None) => {
                        tracing::debug!("ignoring Qingping MQTT message without sensor data");
                    }
                    Err(error) => {
                        tracing::warn!("failed to parse Qingping MQTT payload: {error}");
                    }
                }
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                tracing::warn!("Qingping MQTT internal link stopped: {error}");
                break;
            }
        }
    }
}

pub(crate) fn build_broker_config(host: &str, port: u16) -> Result<BrokerConfig> {
    let listen = resolve_listen_address(host, port)?;
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
                max_payload_size: 20_480,
                max_inflight_count: 100,
                auth: None,
                external_auth: None,
                dynamic_filters: true,
            },
        },
    );

    Ok(BrokerConfig {
        id: 0,
        router: RouterConfig {
            max_connections: 128,
            max_outgoing_packet_count: 200,
            max_segment_size: 1024 * 1024,
            max_segment_count: 10,
            custom_segment: None,
            initialized_filters: None,
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
    use crate::qingping::{normalize_device_mac, qingping_up_topic};

    #[test]
    fn broker_config_uses_configured_listener() {
        let config = build_broker_config("127.0.0.1", 2883).expect("broker config");
        let v4 = config.v4.expect("v4 config");
        let server = v4.get("qingping").expect("qingping server");

        assert_eq!(server.listen, "127.0.0.1:2883".parse().expect("addr"));
        assert_eq!(server.connections.max_payload_size, 20_480);
        assert_eq!(config.router.max_connections, 128);
    }

    #[test]
    fn qingping_topic_normalizes_device_mac() {
        assert_eq!(normalize_device_mac("aa:bb-cc"), "AABBCC");
        assert_eq!(
            qingping_up_topic("aa:bb:cc:dd:ee:ff"),
            "qingping/AABBCCDDEEFF/up"
        );
    }
}
