use std::sync::{Arc, RwLock};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::status::Status;

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

    pub fn overlay_status(&self, status: &mut Status) {
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

pub fn parse_qingping_payload(
    payload: &[u8],
    configured_mac: &str,
) -> Result<Option<QingpingReading>, serde_json::Error> {
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
        .unwrap_or(configured_mac);
    let tvoc = nested_i64(sensor_data, "tvoc_index").or_else(|| nested_i64(sensor_data, "tvoc"));

    Ok(Some(QingpingReading {
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
    }))
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

        let reading = parse_qingping_payload(payload, "112233445566")
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

        assert!(error.is_syntax());
        assert_eq!(state.latest(), Some(existing));
    }
}
