use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::status::Status;

pub(crate) const MAX_QINGPING_PAYLOAD_BYTES: usize = 20 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QingpingReading {
    pub device_name: String,
    pub mac_hint: String,
    pub temp_c: Option<f64>,
    pub humidity: Option<f64>,
    pub co2_ppm: Option<i64>,
    pub pm25: Option<i64>,
    pub pm10: Option<i64>,
    pub tvoc: Option<i64>,
    pub noise_db: Option<i64>,
    pub timestamp: String,
    pub raw_data: String,
}

impl QingpingReading {
    pub fn database_timestamp(&self) -> String {
        self.timestamp.replace('T', " ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct QingpingState {
    latest: Arc<RwLock<Option<QingpingReading>>>,
    interval_configured: Arc<AtomicBool>,
}

impl QingpingState {
    pub fn apply_reading(&self, reading: QingpingReading) {
        *self.latest.write().expect("qingping state lock poisoned") = Some(reading);
    }

    pub fn latest(&self) -> Option<QingpingReading> {
        self.latest
            .read()
            .expect("qingping state lock poisoned")
            .clone()
    }

    pub fn mark_interval_configured(&self) {
        self.interval_configured.store(true, Ordering::SeqCst);
    }

    pub fn overlay_status(&self, status: &mut Status) {
        status.air_quality.interval_configured = self.interval_configured.load(Ordering::SeqCst);

        let Some(reading) = self.latest() else {
            return;
        };

        status.air_quality.co2_ppm = reading.co2_ppm;
        status.air_quality.temp_c = reading.temp_c;
        status.air_quality.humidity = reading.humidity;
        status.air_quality.pm25 = reading.pm25.map(|value| value as f64);
        status.air_quality.pm10 = reading.pm10.map(|value| value as f64);
        status.air_quality.tvoc = reading.tvoc;
        status.air_quality.noise_db = reading.noise_db.map(|value| value as f64);
        status.air_quality.last_update = Some(reading.timestamp);

        if let Some(co2_ppm) = status.air_quality.co2_ppm {
            status.sensors.co2_ppm = co2_ppm;
        }
    }
}

pub fn normalize_device_mac(device_mac: &str) -> String {
    device_mac
        .trim()
        .chars()
        .filter(|character| *character != ':' && *character != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

pub fn qingping_up_topic(device_mac: &str) -> String {
    format!("qingping/{}/up", normalize_device_mac(device_mac))
}

pub fn qingping_down_topic(device_mac: &str) -> String {
    format!("qingping/{}/down", normalize_device_mac(device_mac))
}

pub fn qingping_interval_payload(interval_seconds: i64) -> Value {
    json!({
        "id": 1,
        "need_ack": 1,
        "type": "17",
        "setting": {
            "report_interval": interval_seconds,
            "collect_interval": interval_seconds,
            "co2_sampling_interval": interval_seconds,
            "pm_sampling_interval": interval_seconds,
        }
    })
}

pub fn parse_qingping_payload(
    payload: &[u8],
    configured_mac: &str,
) -> anyhow::Result<Option<QingpingReading>> {
    if payload.len() > MAX_QINGPING_PAYLOAD_BYTES {
        anyhow::bail!(
            "Qingping payload exceeds {} byte limit",
            MAX_QINGPING_PAYLOAD_BYTES
        );
    }

    let value: Value = serde_json::from_slice(payload)?;
    let Some(sensor_data) = sensor_data(&value) else {
        return Ok(None);
    };

    let Some(sensor_object) = sensor_data.as_object() else {
        return Ok(None);
    };

    if sensor_object.is_empty() {
        return Ok(None);
    }

    let configured_mac = normalize_device_mac(configured_mac);
    let mac_hint = value
        .get("mac")
        .and_then(Value::as_str)
        .map(normalize_device_mac)
        .filter(|mac| !mac.is_empty())
        .unwrap_or_else(|| configured_mac.clone());
    if mac_hint != configured_mac {
        anyhow::bail!(
            "Qingping payload MAC {mac_hint} does not match configured MAC {configured_mac}"
        );
    }
    let tvoc = nested_i64(sensor_data, "tvoc_index").or_else(|| nested_i64(sensor_data, "tvoc"));
    let reading = QingpingReading {
        device_name: "Qingping Air Monitor".to_string(),
        mac_hint,
        temp_c: nested_f64(sensor_data, "temperature"),
        humidity: nested_f64(sensor_data, "humidity"),
        co2_ppm: nested_i64(sensor_data, "co2"),
        pm25: nested_i64(sensor_data, "pm25"),
        pm10: nested_i64(sensor_data, "pm10"),
        tvoc,
        noise_db: nested_i64(sensor_data, "noise"),
        timestamp: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        raw_data: String::from_utf8_lossy(payload).to_string(),
    };
    validate_qingping_reading(&reading)?;

    Ok(Some(reading))
}

fn sensor_data(value: &Value) -> Option<&Value> {
    if let Some(local_sensor_data) = value.get("sensorData").and_then(Value::as_array) {
        return local_sensor_data.first();
    }

    let is_cloud_sensor_payload = value
        .get("type")
        .is_some_and(|payload_type| payload_type.as_i64() == Some(17));
    if is_cloud_sensor_payload {
        value.get("sensor_data")
    } else {
        None
    }
}

fn nested_value<'a>(sensor_data: &'a Value, name: &str) -> Option<&'a Value> {
    sensor_data.get(name)?.get("value")
}

fn nested_f64(sensor_data: &Value, name: &str) -> Option<f64> {
    nested_value(sensor_data, name).and_then(Value::as_f64)
}

fn nested_i64(sensor_data: &Value, name: &str) -> Option<i64> {
    let value = nested_value(sensor_data, name)?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number.round() as i64))
}

fn validate_qingping_reading(reading: &QingpingReading) -> anyhow::Result<()> {
    validate_optional_f64("temperature", reading.temp_c, -20.0, 60.0)?;
    validate_optional_f64("humidity", reading.humidity, 0.0, 100.0)?;
    validate_optional_i64("co2", reading.co2_ppm, 0, 10_000)?;
    validate_optional_i64("pm25", reading.pm25, 0, 1_000)?;
    validate_optional_i64("pm10", reading.pm10, 0, 1_000)?;
    validate_optional_i64("tvoc", reading.tvoc, 0, 1_000)?;
    validate_optional_i64("noise", reading.noise_db, 0, 130)?;
    Ok(())
}

fn validate_optional_f64(
    field: &str,
    value: Option<f64>,
    min: f64,
    max: f64,
) -> anyhow::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if !(min..=max).contains(&value) {
        anyhow::bail!("Qingping {field} value {value} outside expected range {min}..={max}");
    }
    Ok(())
}

fn validate_optional_i64(
    field: &str,
    value: Option<i64>,
    min: i64,
    max: i64,
) -> anyhow::Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if !(min..=max).contains(&value) {
        anyhow::bail!("Qingping {field} value {value} outside expected range {min}..={max}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_sensor_data_payload() {
        let payload = br#"{
            "mac": "aa:bb:cc:dd:ee:ff",
            "sensorData": [{
                "temperature": {"value": 22.5},
                "humidity": {"value": 47.1},
                "co2": {"value": 612},
                "pm25": {"value": 3},
                "pm10": {"value": 4},
                "tvoc_index": {"value": 28},
                "noise": {"value": 36}
            }]
        }"#;

        let reading = parse_qingping_payload(payload, "aa:bb:cc:dd:ee:ff")
            .expect("parse")
            .expect("reading");

        assert_eq!(reading.mac_hint, "AABBCCDDEEFF");
        assert_eq!(reading.temp_c, Some(22.5));
        assert_eq!(reading.humidity, Some(47.1));
        assert_eq!(reading.co2_ppm, Some(612));
        assert_eq!(reading.pm25, Some(3));
        assert_eq!(reading.pm10, Some(4));
        assert_eq!(reading.tvoc, Some(28));
        assert_eq!(reading.noise_db, Some(36));
        assert!(reading.raw_data.contains("sensorData"));
    }

    #[test]
    fn parses_cloud_sensor_data_payload_with_tvoc_fallback() {
        let payload = br#"{
            "type": 17,
            "sensor_data": {
                "temperature": {"value": 21.0},
                "humidity": {"value": 41.0},
                "co2": {"value": 505},
                "tvoc": {"value": 19}
            }
        }"#;

        let reading = parse_qingping_payload(payload, "aa:bb:cc:dd:ee:ff")
            .expect("parse")
            .expect("reading");

        assert_eq!(reading.mac_hint, "AABBCCDDEEFF");
        assert_eq!(reading.temp_c, Some(21.0));
        assert_eq!(reading.humidity, Some(41.0));
        assert_eq!(reading.co2_ppm, Some(505));
        assert_eq!(reading.tvoc, Some(19));
        assert_eq!(reading.pm25, None);
    }

    #[test]
    fn ignores_payload_without_sensor_data() {
        let payload = br#"{"sensorData": []}"#;

        let reading = parse_qingping_payload(payload, "aa:bb").expect("parse empty sensor payload");

        assert_eq!(reading, None);
    }

    #[test]
    fn malformed_payload_returns_error_without_state_change() {
        let state = QingpingState::default();
        let existing = parse_qingping_payload(
            br#"{"sensorData":[{"co2":{"value":700}}]}"#,
            "aa:bb:cc:dd:ee:ff",
        )
        .expect("parse")
        .expect("reading");
        state.apply_reading(existing.clone());

        let error = parse_qingping_payload(b"{not-json", "aa:bb").expect_err("malformed payload");

        assert!(error.downcast_ref::<serde_json::Error>().is_some());
        assert_eq!(state.latest(), Some(existing));
    }

    #[test]
    fn rejects_mismatched_payload_mac() {
        let payload = br#"{
            "mac": "aa:bb:cc:dd:ee:ff",
            "sensorData": [{"co2": {"value": 612}}]
        }"#;

        let error = parse_qingping_payload(payload, "11:22:33:44:55:66")
            .expect_err("mismatched mac rejected");

        assert!(error.to_string().contains("does not match configured MAC"));
    }

    #[test]
    fn rejects_oversized_payload_before_storage() {
        let payload = vec![b' '; MAX_QINGPING_PAYLOAD_BYTES + 1];

        let error = parse_qingping_payload(&payload, "aa:bb:cc:dd:ee:ff")
            .expect_err("oversized payload rejected");

        assert!(error.to_string().contains("payload exceeds"));
    }

    #[test]
    fn accepts_current_qingping_sized_payloads() {
        let filler = "x".repeat(16 * 1024);
        let payload = serde_json::json!({
            "mac": "aa:bb:cc:dd:ee:ff",
            "sensorData": [{
                "temperature": {"value": 22.5},
                "humidity": {"value": 47.0},
                "co2": {"value": 413},
                "tvoc": {"value": 18}
            }],
            "padding": filler
        })
        .to_string();

        assert!(payload.len() > 16 * 1024);
        assert!(payload.len() <= MAX_QINGPING_PAYLOAD_BYTES);
        let reading =
            parse_qingping_payload(payload.as_bytes(), "aa:bb:cc:dd:ee:ff").expect("parse");

        assert_eq!(reading.expect("reading").co2_ppm, Some(413));
    }

    #[test]
    fn rejects_out_of_range_sensor_values() {
        let payload = br#"{
            "sensorData": [{
                "temperature": {"value": 22.5},
                "humidity": {"value": 47.1},
                "co2": {"value": 50000}
            }]
        }"#;

        let error = parse_qingping_payload(payload, "aa:bb:cc:dd:ee:ff")
            .expect_err("out-of-range co2 rejected");

        assert!(error.to_string().contains("outside expected range"));
    }

    #[test]
    fn interval_configured_overlays_status_after_successful_command() {
        let state = QingpingState::default();
        let mut status = Status {
            state: "away".to_string(),
            is_present: false,
            presence_signal_active: false,
            safety_interlock: false,
            erv_should_run: false,
            verifying_departure: false,
            in_door_open_mode: false,
            sensors: crate::status::SensorsStatus::default(),
            air_quality: crate::status::AirQualityStatus::default(),
            erv: crate::status::ErvStatus::default(),
            hvac: crate::status::HvacStatus::default(),
            manual_override: crate::status::ManualOverrideStatus::default(),
            notifications: Vec::new(),
        };

        state.overlay_status(&mut status);
        assert!(!status.air_quality.interval_configured);

        state.mark_interval_configured();
        state.overlay_status(&mut status);
        assert!(status.air_quality.interval_configured);
    }

    #[test]
    fn interval_command_matches_qingping_down_topic_contract() {
        assert_eq!(
            qingping_down_topic("aa:bb-cc:dd:ee:ff"),
            "qingping/AABBCCDDEEFF/down"
        );

        let payload = qingping_interval_payload(30);

        assert_eq!(payload["id"], 1);
        assert_eq!(payload["need_ack"], 1);
        assert_eq!(payload["type"], "17");
        assert_eq!(payload["setting"]["report_interval"], 30);
        assert_eq!(payload["setting"]["collect_interval"], 30);
        assert_eq!(payload["setting"]["co2_sampling_interval"], 30);
        assert_eq!(payload["setting"]["pm_sampling_interval"], 30);
    }
}
