use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

use crate::{
    config::AppConfig,
    erv::{ErvDeviceStatus, ErvFanSpeed, ErvSpeedWriter, ErvState},
    policy::{AirQualityReading, ErvDecision, ErvPolicyInput, ErvPolicyState, VentilationSpeed},
    qingping::QingpingState,
    state::{OccupancyState, StateMachine, StateTransition},
};

#[derive(Clone)]
pub struct ErvPolicyCoordinator {
    config: AppConfig,
    state_machine: Arc<RwLock<StateMachine>>,
    qingping: QingpingState,
    erv: ErvState,
    policy: Arc<RwLock<ErvPolicyState>>,
    writer: Arc<dyn ErvSpeedWriter>,
    status_broadcast: broadcast::Sender<()>,
    apply_lock: Arc<AsyncMutex<()>>,
    last_recorded_qingping: Arc<Mutex<Option<QingpingReadingKey>>>,
}

impl ErvPolicyCoordinator {
    pub fn new(
        config: AppConfig,
        state_machine: Arc<RwLock<StateMachine>>,
        qingping: QingpingState,
        erv: ErvState,
        policy: Arc<RwLock<ErvPolicyState>>,
        writer: Arc<dyn ErvSpeedWriter>,
        status_broadcast: broadcast::Sender<()>,
    ) -> Self {
        Self {
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer,
            status_broadcast,
            apply_lock: Arc::new(AsyncMutex::new(())),
            last_recorded_qingping: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn evaluate_erv_policy(&self, bypass_dwell: bool) -> Result<()> {
        let _apply_guard = self.apply_lock.lock().await;
        self.evaluate_erv_policy_locked(bypass_dwell).await
    }

    pub async fn update_state_and_maybe_evaluate<T, F>(&self, update: F) -> Result<T>
    where
        F: FnOnce() -> Result<(T, Option<StateTransition>, f64, bool, bool)>,
    {
        let _apply_guard = self.apply_lock.lock().await;
        let (result, transition, now, bypass_dwell, should_evaluate) = update()?;
        if should_evaluate {
            self.record_occupancy_transition_locked(transition, now);
            self.evaluate_erv_policy_locked(bypass_dwell).await?;
        }
        Ok(result)
    }

    async fn evaluate_erv_policy_locked(&self, bypass_dwell: bool) -> Result<()> {
        if !self.config.room_mode.climate_automation_enabled {
            return Ok(());
        }

        let now = unix_timestamp_now();
        let state_status = {
            let machine = self
                .state_machine
                .read()
                .expect("state machine lock poisoned");
            machine.status_at(now)
        };
        let qingping_reading = self.trusted_qingping_reading();
        let manual_override = self
            .erv
            .active_manual_override_speed(now)
            .map(ventilation_speed_from_erv);
        let mut erv_snapshot = self.erv.snapshot();
        let mut fresh_status_checked = false;
        if self.should_refresh_unknown_erv_status(
            &erv_snapshot,
            &state_status,
            qingping_reading.as_ref(),
            manual_override,
        ) {
            self.erv
                .smoke_status_with(&self.config.erv, self.writer.as_ref())
                .await
                .context("ERV smoke check failed before policy evaluation")?;
            erv_snapshot = self.erv.snapshot();
            fresh_status_checked = true;
        }

        let decision = {
            let mut policy = self.policy.write().expect("ERV policy lock poisoned");
            if let Some(reading) = &qingping_reading {
                self.record_qingping_reading_once(&mut policy, now, reading);
            }

            policy.decide_erv(
                &self.config.thresholds,
                ErvPolicyInput {
                    occupancy: if state_status.is_present {
                        OccupancyState::Present
                    } else {
                        OccupancyState::Away
                    },
                    door_open: state_status.sensors.door_open,
                    door_open_seconds: state_status
                        .sensors
                        .door_open
                        .then_some(state_status.sensors.door_opened_at)
                        .filter(|opened_at| *opened_at > 0.0)
                        .map(|opened_at| (now - opened_at).max(0.0)),
                    door_closed_seconds: (!state_status.sensors.door_open
                        && state_status.sensors.door_closed_at > 0.0)
                        .then_some((now - state_status.sensors.door_closed_at).max(0.0)),
                    window_open: state_status.sensors.window_open,
                    co2_ppm: if self.config.room_mode.air_quality_sensors_enabled {
                        qingping_reading
                            .as_ref()
                            .and_then(|reading| reading.co2_ppm)
                            .or(Some(state_status.sensors.co2_ppm))
                    } else {
                        None
                    },
                    tvoc: qingping_reading.as_ref().and_then(|reading| reading.tvoc),
                    current_running: erv_snapshot.running,
                    current_speed: ventilation_speed_from_erv(erv_snapshot.speed),
                    manual_override,
                    last_speed_changed_at: erv_snapshot.last_speed_changed_at,
                    bypass_dwell,
                },
                now,
            )
        };

        match decision {
            ErvDecision::NoChange | ErvDecision::SuppressedByDwell { .. } => Ok(()),
            ErvDecision::SetSpeed {
                target_speed,
                reason,
                ..
            } => {
                if !self.erv.local_retry_allowed(now) {
                    return Ok(());
                }
                self.apply_policy_erv_speed(
                    erv_speed_from_ventilation(target_speed),
                    &reason,
                    self.latest_co2_ppm(),
                    fresh_status_checked,
                )
                .await?;
                self.broadcast_status();
                Ok(())
            }
        }
    }

    fn record_occupancy_transition_locked(&self, transition: Option<StateTransition>, now: f64) {
        let Some(transition) = transition else {
            return;
        };
        let status = {
            let machine = self
                .state_machine
                .read()
                .expect("state machine lock poisoned");
            machine.status_at(now)
        };
        self.policy
            .write()
            .expect("ERV policy lock poisoned")
            .on_occupancy_transition(
                &self.config.thresholds,
                transition.old_state,
                transition.new_state,
                now,
                status.sensors.door_open,
                status.sensors.window_open,
            );
    }

    pub async fn apply_manual_erv_speed(
        &self,
        speed: ErvFanSpeed,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus> {
        let _apply_guard = self.apply_lock.lock().await;
        let previous_override = self
            .erv
            .replace_manual_override(speed, unix_timestamp_now());

        match self
            .apply_erv_speed(speed, "manual_override", co2_ppm)
            .await
        {
            Ok(status) => Ok(status),
            Err(error) => {
                self.erv.restore_manual_override(previous_override);
                Err(error)
            }
        }
    }

    async fn apply_erv_speed(
        &self,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus> {
        self.erv
            .set_speed_with(
                &self.config.erv,
                self.writer.as_ref(),
                speed,
                reason,
                co2_ppm,
            )
            .await
    }

    async fn apply_policy_erv_speed(
        &self,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
        fresh_status_checked: bool,
    ) -> Result<ErvDeviceStatus> {
        if fresh_status_checked {
            return self
                .erv
                .set_speed_after_smoke_with(
                    &self.config.erv,
                    self.writer.as_ref(),
                    speed,
                    reason,
                    co2_ppm,
                )
                .await;
        }

        self.apply_erv_speed(speed, reason, co2_ppm).await
    }

    pub fn latest_co2_ppm(&self) -> Option<i64> {
        self.trusted_qingping_reading()
            .and_then(|reading| reading.co2_ppm)
    }

    fn trusted_qingping_reading(&self) -> Option<crate::qingping::QingpingReading> {
        self.config
            .room_mode
            .air_quality_sensors_enabled
            .then(|| self.qingping.latest())
            .flatten()
    }

    pub fn broadcast_status(&self) {
        let _ = self.status_broadcast.send(());
    }

    fn should_refresh_unknown_erv_status(
        &self,
        snapshot: &crate::erv::ErvRuntimeSnapshot,
        state_status: &crate::state::StateStatus,
        qingping_reading: Option<&crate::qingping::QingpingReading>,
        manual_override: Option<VentilationSpeed>,
    ) -> bool {
        if snapshot.status_known
            || !self.config.erv.active_control_enabled
            || !self.config.erv.is_configured()
            || snapshot.local_key_invalid
            || !self.erv.local_retry_allowed(unix_timestamp_now())
        {
            return false;
        }

        if state_status.sensors.door_open || state_status.sensors.window_open {
            return true;
        }

        if manual_override.is_some() {
            return false;
        }

        let co2_ppm = if self.config.room_mode.air_quality_sensors_enabled {
            qingping_reading
                .and_then(|reading| reading.co2_ppm)
                .or(Some(state_status.sensors.co2_ppm))
        } else {
            None
        };
        let tvoc = qingping_reading.and_then(|reading| reading.tvoc);

        if state_status.is_present {
            return !co2_ppm.is_some_and(|co2| co2 >= self.config.thresholds.co2_critical_ppm);
        }

        !co2_ppm.is_some_and(|co2| co2 > self.config.thresholds.co2_refresh_target_ppm)
            && !tvoc.is_some_and(|tvoc| tvoc > self.config.thresholds.tvoc_away_threshold)
    }

    fn record_qingping_reading_once(
        &self,
        policy: &mut ErvPolicyState,
        now: f64,
        reading: &crate::qingping::QingpingReading,
    ) {
        let key = QingpingReadingKey::from(reading);
        let should_record = {
            let mut last_recorded = self
                .last_recorded_qingping
                .lock()
                .expect("Qingping reading key lock poisoned");
            if last_recorded.as_ref() == Some(&key) {
                false
            } else {
                *last_recorded = Some(key);
                true
            }
        };

        if should_record {
            policy.record_reading(
                &self.config.thresholds,
                now,
                AirQualityReading {
                    co2_ppm: reading.co2_ppm,
                    tvoc: reading.tvoc,
                    temp_c: reading.temp_c,
                },
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QingpingReadingKey {
    timestamp: String,
    raw_data: String,
}

impl From<&crate::qingping::QingpingReading> for QingpingReadingKey {
    fn from(reading: &crate::qingping::QingpingReading) -> Self {
        Self {
            timestamp: reading.timestamp.clone(),
            raw_data: reading.raw_data.clone(),
        }
    }
}

fn ventilation_speed_from_erv(speed: ErvFanSpeed) -> VentilationSpeed {
    match speed {
        ErvFanSpeed::Off => VentilationSpeed::Off,
        ErvFanSpeed::Quiet => VentilationSpeed::Quiet,
        ErvFanSpeed::Medium => VentilationSpeed::Medium,
        ErvFanSpeed::Turbo => VentilationSpeed::Turbo,
    }
}

fn erv_speed_from_ventilation(speed: VentilationSpeed) -> ErvFanSpeed {
    match speed {
        VentilationSpeed::Off => ErvFanSpeed::Off,
        VentilationSpeed::Quiet => ErvFanSpeed::Quiet,
        VentilationSpeed::Medium => ErvFanSpeed::Medium,
        VentilationSpeed::Turbo => ErvFanSpeed::Turbo,
    }
}

fn unix_timestamp_now() -> f64 {
    chrono::Local::now().timestamp_millis() as f64 / 1_000.0
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use anyhow::Result;
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::*;
    use crate::{
        config::{
            ErvConfig, MitsubishiConfig, OrchestratorConfig, PresenceConfig, QingpingConfig,
            RuntimeConfig, TelemetryConfig, ThresholdsConfig, YoLinkConfig,
        },
        db,
        erv::BoxFutureResult,
        qingping::QingpingReading,
        state::StateMachine,
    };

    struct FakeErvWriter {
        smoke_results: Mutex<VecDeque<Result<ErvDeviceStatus>>>,
        smoke_calls: AtomicUsize,
        writes: Mutex<Vec<ErvFanSpeed>>,
        set_delay: Duration,
    }

    impl FakeErvWriter {
        fn new(smoke_results: Vec<Result<ErvDeviceStatus>>) -> Self {
            Self {
                smoke_results: Mutex::new(smoke_results.into()),
                smoke_calls: AtomicUsize::new(0),
                writes: Mutex::new(Vec::new()),
                set_delay: Duration::ZERO,
            }
        }

        fn with_set_delay(mut self, set_delay: Duration) -> Self {
            self.set_delay = set_delay;
            self
        }

        fn writes(&self) -> Vec<ErvFanSpeed> {
            self.writes.lock().expect("writes lock").clone()
        }

        fn smoke_calls(&self) -> usize {
            self.smoke_calls.load(Ordering::SeqCst)
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
                .expect("smoke lock")
                .pop_front()
                .unwrap_or_else(|| Ok(erv_status(ErvFanSpeed::Off)));
            Box::pin(async move { result })
        }

        fn set_speed<'a>(
            &'a self,
            _config: &'a ErvConfig,
            speed: ErvFanSpeed,
        ) -> BoxFutureResult<'a, ErvDeviceStatus> {
            self.writes.lock().expect("writes lock").push(speed);
            let set_delay = self.set_delay;
            Box::pin(async move {
                if !set_delay.is_zero() {
                    tokio::time::sleep(set_delay).await;
                }
                Ok(erv_status(speed))
            })
        }
    }

    fn test_config(database_path: PathBuf) -> AppConfig {
        let root = database_path
            .parent()
            .expect("database parent")
            .to_path_buf();
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            room_mode: crate::config::RoomModeConfig::default(),
            presence: PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            artifacts: crate::config::ArtifactConfig::default(),
            cloudflare_access: crate::config::CloudflareAccessConfig::default(),
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
            thresholds: ThresholdsConfig::default(),
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
                session_tool_usage_db_path: root.join("claude-tool-usage.db"),
                tool_usage_db_path: root.join("tool-usage.db"),
                engram_db_path: root.join("engram.db"),
                engram_registry_path: root.join("engram-registry.json"),
            },
        }
    }

    fn qingping_reading(co2_ppm: i64) -> QingpingReading {
        QingpingReading {
            device_name: "Qingping Air Monitor".to_string(),
            mac_hint: "AABBCCDDEEFF".to_string(),
            temp_c: Some(22.0),
            humidity: Some(45.0),
            co2_ppm: Some(co2_ppm),
            pm25: Some(2),
            pm10: Some(3),
            tvoc: Some(25),
            noise_db: Some(35),
            timestamp: "2026-06-05T12:30:00".to_string(),
            raw_data: "{}".to_string(),
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

    #[tokio::test]
    async fn sensor_reading_can_drive_erv_policy_without_route_update() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now - 2.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, now - 1.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path.clone());
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Off))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("policy applies");

        assert_eq!(writer.writes(), vec![ErvFanSpeed::Quiet]);
        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert_eq!(history.climate_actions[0]["system"], "erv");
        assert_eq!(history.climate_actions[0]["action"], "quiet");
        assert_eq!(
            history.climate_actions[0]["reason"],
            "present_co2_critical_2100ppm"
        );
    }

    #[tokio::test]
    async fn disabled_air_quality_sensors_do_not_drive_erv_policy_or_history() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let mut config = test_config(database_path.clone());
        config.room_mode.air_quality_sensors_enabled = false;
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds_and_room_mode(
            &config.thresholds,
            &config.room_mode,
            now - 2.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, now - 1.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path.clone());
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Off))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy.clone(),
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("policy applies without trusted air-quality");

        assert!(writer.writes().is_empty());
        assert_eq!(
            policy
                .read()
                .expect("policy lock poisoned")
                .air_quality_sample_counts(),
            (0, 0)
        );
        assert!(
            db::read_history(&database_path, 1, 10)
                .expect("history")
                .climate_actions
                .is_empty()
        );
        assert_eq!(coordinator.latest_co2_ppm(), None);
    }

    #[tokio::test]
    async fn disabled_climate_automation_skips_erv_policy_writes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let mut config = test_config(database_path.clone());
        config.room_mode.climate_automation_enabled = false;
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds_and_room_mode(
            &config.thresholds,
            &config.room_mode,
            now - 2.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, now - 1.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path.clone());
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Off))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("policy gate returns ok");

        assert!(writer.writes().is_empty());
        assert!(
            db::read_history(&database_path, 1, 10)
                .expect("history")
                .climate_actions
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unknown_cached_status_is_smoked_before_safety_off_decision() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now - 2.0,
        )));
        {
            let mut machine = state_machine.write().expect("state machine lock poisoned");
            machine.set_manual_presence(true, now - 1.0);
            machine.update_window(true, now - 0.5);
        }
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(700));
        let erv = ErvState::new(database_path.clone());
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(
            ErvFanSpeed::Medium,
        ))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("policy applies");

        assert_eq!(writer.writes(), vec![ErvFanSpeed::Off]);
        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert_eq!(history.climate_actions[0]["system"], "erv");
        assert_eq!(history.climate_actions[0]["action"], "off");
        assert_eq!(history.climate_actions[0]["reason"], "safety_interlock");
    }

    #[tokio::test]
    async fn locked_transition_update_applies_settle_before_policy_decision() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let now = unix_timestamp_now();
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            now - 2.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, now - 1.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path);
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Off))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine.clone(),
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        let transition = coordinator
            .update_state_and_maybe_evaluate(|| {
                let transition = state_machine
                    .write()
                    .expect("state machine lock poisoned")
                    .set_manual_presence(false, now);
                Ok((transition, transition, now, transition.is_some(), true))
            })
            .await
            .expect("state update evaluates");

        assert!(transition.is_some());
        assert!(writer.writes().is_empty());
    }

    #[tokio::test]
    async fn concurrent_sensor_policy_evaluations_issue_one_write() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            1_000.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, 1_001.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path);
        let writer = Arc::new(
            FakeErvWriter::new(vec![
                Ok(erv_status(ErvFanSpeed::Off)),
                Ok(erv_status(ErvFanSpeed::Off)),
            ])
            .with_set_delay(Duration::from_millis(25)),
        );
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy.clone(),
            writer.clone(),
            status_broadcast,
        );

        let (first, second) = tokio::join!(
            coordinator.evaluate_erv_policy(false),
            coordinator.evaluate_erv_policy(false)
        );

        first.expect("first policy applies");
        second.expect("second policy applies");
        assert_eq!(writer.writes(), vec![ErvFanSpeed::Quiet]);
        assert_eq!(
            policy
                .read()
                .expect("policy lock poisoned")
                .air_quality_sample_counts(),
            (1, 1)
        );
    }

    #[tokio::test]
    async fn automated_policy_write_respects_local_failure_backoff() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            1_000.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, 1_001.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(2100));
        let erv = ErvState::new(database_path);
        let writer = Arc::new(FakeErvWriter::new(vec![Err(anyhow::anyhow!(
            "Connection reset by peer"
        ))]));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect_err("first automated write should fail");
        assert_eq!(writer.smoke_calls(), 1);
        assert!(writer.writes().is_empty());

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("second policy evaluation respects backoff");
        assert_eq!(writer.smoke_calls(), 1);
        assert!(writer.writes().is_empty());
    }

    #[tokio::test]
    async fn safety_interlock_respects_local_retry_backoff() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            1_000.0,
        )));
        {
            let mut machine = state_machine.write().expect("state machine lock poisoned");
            machine.set_manual_presence(true, 1_001.0);
            machine.update_door(true, 1_002.0);
        }
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(700));
        let erv = ErvState::new(database_path);
        let writer = Arc::new(FakeErvWriter::new(vec![Ok(erv_status(ErvFanSpeed::Turbo))]));

        for index in 0..3 {
            erv.set_speed_after_smoke_with(
                &config.erv,
                writer.as_ref(),
                ErvFanSpeed::Turbo,
                &format!("away_refresh_{index}"),
                Some(900),
            )
            .await
            .expect("write succeeds before burst guard");
        }
        erv.set_speed_after_smoke_with(
            &config.erv,
            writer.as_ref(),
            ErvFanSpeed::Turbo,
            "away_refresh_suppressed",
            Some(900),
        )
        .await
        .expect_err("fourth automated write is suppressed");
        assert!(!erv.local_retry_allowed(unix_timestamp_now()));

        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy,
            writer.clone(),
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("safety interlock waits for local retry backoff");

        assert_eq!(
            writer.writes(),
            vec![ErvFanSpeed::Turbo, ErvFanSpeed::Turbo, ErvFanSpeed::Turbo]
        );
    }

    #[tokio::test]
    async fn manual_erv_write_serializes_with_policy_evaluation() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let mut config = test_config(database_path.clone());
        config.thresholds.erv_min_dwell_seconds = 0;
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            1_000.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, 1_001.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(450));
        let erv = ErvState::new(database_path.clone());
        let writer = Arc::new(
            FakeErvWriter::new(vec![
                Ok(erv_status(ErvFanSpeed::Medium)),
                Ok(erv_status(ErvFanSpeed::Medium)),
            ])
            .with_set_delay(Duration::from_millis(25)),
        );
        erv.smoke_status_with(&config.erv, writer.as_ref())
            .await
            .expect("initial ERV status");
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv.clone(),
            policy,
            writer.clone(),
            status_broadcast,
        );

        let (manual, policy_eval) = tokio::join!(
            coordinator.apply_manual_erv_speed(ErvFanSpeed::Turbo, Some(450)),
            coordinator.evaluate_erv_policy(true)
        );

        manual.expect("manual write succeeds");
        policy_eval.expect("policy evaluation succeeds");
        assert_eq!(writer.writes(), vec![ErvFanSpeed::Turbo]);
        assert_eq!(
            erv.active_manual_override_speed(unix_timestamp_now()),
            Some(ErvFanSpeed::Turbo)
        );
        let history = db::read_history(&database_path, 1, 10).expect("history");
        assert_eq!(history.climate_actions.len(), 1);
        assert_eq!(history.climate_actions[0]["action"], "turbo");
        assert_eq!(history.climate_actions[0]["reason"], "manual_override");
    }

    #[tokio::test]
    async fn repeated_policy_evaluation_records_same_qingping_sample_once() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let config = test_config(database_path.clone());
        let state_machine = Arc::new(RwLock::new(StateMachine::from_thresholds(
            &config.thresholds,
            1_000.0,
        )));
        state_machine
            .write()
            .expect("state machine lock poisoned")
            .set_manual_presence(true, 1_001.0);
        let qingping = QingpingState::default();
        qingping.apply_reading(qingping_reading(700));
        let erv = ErvState::new(database_path);
        let writer = Arc::new(FakeErvWriter::new(Vec::new()));
        let policy = Arc::new(RwLock::new(ErvPolicyState::new(&config.thresholds)));
        let (status_broadcast, _) = broadcast::channel(4);
        let coordinator = ErvPolicyCoordinator::new(
            config,
            state_machine,
            qingping,
            erv,
            policy.clone(),
            writer,
            status_broadcast,
        );

        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("first policy applies");
        coordinator
            .evaluate_erv_policy(false)
            .await
            .expect("second policy applies");

        assert_eq!(
            policy
                .read()
                .expect("policy lock poisoned")
                .air_quality_sample_counts(),
            (1, 1)
        );
    }
}
