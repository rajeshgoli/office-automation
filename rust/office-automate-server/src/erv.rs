use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    path::PathBuf,
    pin::Pin,
    str::FromStr,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use serde_json::{Map, Value, json};
use tokio::{
    sync::{Mutex as AsyncMutex, broadcast},
    time,
};

use crate::{
    config::{AppConfig, ErvConfig},
    db,
    smart_life::{SmartLifeClient, auth_file_or_default, status_code_value},
    status::{AppNotification, ErvControlStatus, ErvStatusSource, Status},
};

const DP_POWER: &str = "1";
const DP_SUPPLY_SPEED: &str = "101";
const DP_EXHAUST_SPEED: &str = "102";
const LOCAL_KEY_ERROR_THRESHOLD: u64 = 5;
const LOCAL_FAILURE_BASE_RETRY_SECONDS: f64 = 5.0 * 60.0;
const LOCAL_FAILURE_MAX_RETRY_SECONDS: f64 = 60.0 * 60.0;
const LOCAL_ACTIVITY_HISTORY_LIMIT: usize = 32;
const LOCAL_FAILURE_RCA_THRESHOLD: u64 = 3;
const LOCAL_WRITE_BURST_WINDOW_SECONDS: f64 = 5.0 * 60.0;
const LOCAL_WRITE_BURST_ATTEMPT_LIMIT: usize = 3;
const BOOT_RECOVERY_REASON: &str = "boot_unknown_state";
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

    fn speed_preset(self, negative_pressure: bool) -> Option<(i64, i64)> {
        match self {
            Self::Off => None,
            Self::Quiet if negative_pressure => Some((1, 2)),
            Self::Medium if negative_pressure => Some((2, 3)),
            Self::Turbo if negative_pressure => Some((7, 8)),
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
        negative_pressure: bool,
    ) -> BoxFutureResult<'a, ErvDeviceStatus>;

    /// Which transport carried the most recent `set_speed`, and how well the
    /// returned status was confirmed. Defaults to a local command verified by a
    /// local read, which is what every writer did before scene control existed.
    fn last_write_report(&self) -> ErvWriteReport {
        ErvWriteReport::default()
    }

    /// Take the error from the most recent local readback attempt, if it
    /// failed, clearing it.
    ///
    /// Nothing polls any more, so a read-after-write is often the only local
    /// read that happens. Without reporting it, a local key that dies after a
    /// good boot read would fail every verification while `local_key_invalid`
    /// and the degraded-readback notification stayed clear forever.
    ///
    /// Consuming is the point: while the writer is in read backoff it performs
    /// no local I/O at all, so a value left in place would let one failure be
    /// counted once per write and manufacture a key-invalid verdict.
    fn take_local_failure(&self) -> Option<String> {
        None
    }
}

/// Which transport issued a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErvTransport {
    #[default]
    Local,
    Scene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErvWriteReport {
    pub transport: ErvTransport,
    pub source: ErvStatusSource,
}

impl Default for ErvWriteReport {
    fn default() -> Self {
        Self {
            transport: ErvTransport::Local,
            source: ErvStatusSource::Local,
        }
    }
}

/// The cloud half of ERV control: triggering tap-to-run scenes, and reading the
/// one status code the sharing API exposes for this device.
pub trait ErvCloudClient: Send + Sync {
    fn trigger_scene<'a>(&'a self, home_id: &'a str, scene_id: &'a str) -> BoxFutureResult<'a, ()>;

    /// The device's `switch` bit. `None` when the device reports no `switch`
    /// code at all. Speed is never available here — the speed data points are
    /// private and the sharing API hides them.
    fn read_power<'a>(&'a self, device_id: &'a str) -> BoxFutureResult<'a, Option<bool>>;
}

pub struct SmartLifeErvCloud {
    client: SmartLifeClient,
}

impl SmartLifeErvCloud {
    pub fn new(client_id: impl Into<String>, config: &ErvConfig) -> Self {
        Self {
            client: SmartLifeClient::new(
                client_id,
                auth_file_or_default(config.smart_life_auth_file.as_ref()),
            ),
        }
    }
}

impl ErvCloudClient for SmartLifeErvCloud {
    fn trigger_scene<'a>(&'a self, home_id: &'a str, scene_id: &'a str) -> BoxFutureResult<'a, ()> {
        Box::pin(async move { self.client.trigger_scene(home_id, scene_id).await })
    }

    fn read_power<'a>(&'a self, device_id: &'a str) -> BoxFutureResult<'a, Option<bool>> {
        Box::pin(async move {
            let status = self.client.device_status(device_id).await?;
            Ok(status_code_value(&status, "switch").and_then(value_as_bool))
        })
    }
}

/// The scene path is not configured for this write.
///
/// Deterministic configuration errors, not outages: they are surfaced rather
/// than silently substituted or written locally. A system that reports a mode
/// it is not delivering is worse than one that reports an error, and falling
/// back to a local command here would reintroduce the Err 914 path over a
/// problem no retry can fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneConfigError {
    /// No `smart_life_home_id`, so no scene can be triggered at all.
    MissingHomeId,
    /// A `(speed, pressure)` pair that resolved to no configured scene.
    MissingScene { preset: String },
}

impl SceneConfigError {
    /// Short label for `erv.control.missing_scene`.
    fn subject(&self) -> String {
        match self {
            Self::MissingHomeId => "smart_life_home_id".to_string(),
            Self::MissingScene { preset } => preset.clone(),
        }
    }
}

impl fmt::Display for SceneConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHomeId => write!(formatter, "ERV Smart Life home id is not configured"),
            Self::MissingScene { preset } => write!(
                formatter,
                "ERV Smart Life scene id for {preset} is not configured"
            ),
        }
    }
}

impl std::error::Error for SceneConfigError {}

fn scene_preset_label(speed: ErvFanSpeed, negative_pressure: bool) -> String {
    if speed == ErvFanSpeed::Off || !negative_pressure {
        speed.as_str().to_string()
    } else {
        format!("{} (negative pressure)", speed.as_str())
    }
}

fn scene_id_for(
    config: &ErvConfig,
    speed: ErvFanSpeed,
    negative_pressure: bool,
) -> Result<&str, SceneConfigError> {
    let configured = match (speed, negative_pressure) {
        (ErvFanSpeed::Off, _) => &config.off_scene_id,
        (ErvFanSpeed::Quiet, false) => &config.quiet_scene_id,
        (ErvFanSpeed::Medium, false) => &config.medium_scene_id,
        (ErvFanSpeed::Turbo, false) => &config.turbo_scene_id,
        (ErvFanSpeed::Quiet, true) => &config.quiet_negative_pressure_scene_id,
        (ErvFanSpeed::Medium, true) => &config.medium_negative_pressure_scene_id,
        (ErvFanSpeed::Turbo, true) => &config.turbo_negative_pressure_scene_id,
    };

    configured
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SceneConfigError::MissingScene {
            preset: scene_preset_label(speed, negative_pressure),
        })
}

/// The state a command asks for, used when nothing could observe the result.
fn assumed_status(speed: ErvFanSpeed, negative_pressure: bool) -> ErvDeviceStatus {
    let power = speed != ErvFanSpeed::Off;
    let (supply_speed, exhaust_speed) = match speed.speed_preset(negative_pressure) {
        Some((supply, exhaust)) => (Some(supply), Some(exhaust)),
        None => (None, None),
    };
    let mut dps = Map::new();
    dps.insert(DP_POWER.to_string(), Value::Bool(power));
    if let (Some(supply), Some(exhaust)) = (supply_speed, exhaust_speed) {
        dps.insert(DP_SUPPLY_SPEED.to_string(), json!(supply));
        dps.insert(DP_EXHAUST_SPEED.to_string(), json!(exhaust));
    }

    ErvDeviceStatus {
        power,
        fan_speed: Some(speed),
        supply_speed,
        exhaust_speed,
        raw_dps: Value::Object(dps),
    }
}

/// Writes through Smart Life tap-to-run scenes. Issues no local command ever —
/// local commands are the documented cause of the Err 914 lockout.
pub struct SceneErvSpeedWriter {
    cloud: Arc<dyn ErvCloudClient>,
}

impl SceneErvSpeedWriter {
    pub fn new(cloud: Arc<dyn ErvCloudClient>) -> Self {
        Self { cloud }
    }
}

impl ErvSpeedWriter for SceneErvSpeedWriter {
    fn smoke_status<'a>(&'a self, _config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            bail!("ERV scene control cannot read status; the cloud does not expose the speed DPs")
        })
    }

    fn set_speed<'a>(
        &'a self,
        config: &'a ErvConfig,
        speed: ErvFanSpeed,
        negative_pressure: bool,
    ) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            let home_id = config
                .smart_life_home_id()
                .ok_or(SceneConfigError::MissingHomeId)?;
            let preset = scene_preset_label(speed, negative_pressure);
            let scene_id = scene_id_for(config, speed, negative_pressure)?;

            self.cloud
                .trigger_scene(home_id, scene_id)
                .await
                .with_context(|| format!("failed to trigger ERV {preset} scene"))?;

            Ok(assumed_status(speed, negative_pressure))
        })
    }

    fn last_write_report(&self) -> ErvWriteReport {
        ErvWriteReport {
            transport: ErvTransport::Scene,
            source: ErvStatusSource::Assumed,
        }
    }
}

#[derive(Debug, Default)]
struct SplitWriterState {
    report: ErvWriteReport,
    last_known_power: Option<bool>,
    /// Health of the local path, fed by both failed readbacks and failed
    /// fallback writes. It gates readback *and* whether local is healthy
    /// enough to be used as a write fallback.
    consecutive_local_failures: u64,
    local_retry_at: Option<f64>,
    /// Surfaced to the state layer so local-key health stays accurate now
    /// that nothing polls. Cleared by a successful read.
    unreported_local_failure: Option<String>,
}

/// Splits the two transports by direction: writes go out over `writer` (the
/// scene path by default), reads come back over `reader` (local Tuya, the only
/// view of the speed DPs).
pub struct SplitErvWriter {
    writer: Arc<dyn ErvSpeedWriter>,
    reader: Arc<dyn ErvStatusReader>,
    fallback: Option<Arc<dyn ErvSpeedWriter>>,
    cloud: Option<Arc<dyn ErvCloudClient>>,
    state: Mutex<SplitWriterState>,
}

impl SplitErvWriter {
    pub fn new(writer: Arc<dyn ErvSpeedWriter>, reader: Arc<dyn ErvStatusReader>) -> Self {
        Self {
            writer,
            reader,
            fallback: None,
            cloud: None,
            state: Mutex::new(SplitWriterState::default()),
        }
    }

    /// The local writer used when a scene trigger fails and local is healthy.
    pub fn with_local_write_fallback(mut self, fallback: Arc<dyn ErvSpeedWriter>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    /// The cloud client used to confirm power transitions when local reads are
    /// unavailable.
    pub fn with_cloud_verifier(mut self, cloud: Arc<dyn ErvCloudClient>) -> Self {
        self.cloud = Some(cloud);
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SplitWriterState> {
        self.state.lock().expect("ERV split writer lock poisoned")
    }

    /// Local reads back off after failures so a dead local key costs one slow
    /// read every few minutes instead of one per write.
    fn local_read_allowed(&self, config: &ErvConfig) -> bool {
        if !config.local_readback_active() {
            return false;
        }
        self.state()
            .local_retry_at
            .is_none_or(|retry_at| unix_timestamp_now() >= retry_at)
    }

    #[cfg(test)]
    fn clear_local_backoff_for_test(&self) {
        self.state().local_retry_at = None;
    }

    #[cfg(test)]
    fn local_failure_count_for_test(&self) -> u64 {
        self.state().consecutive_local_failures
    }

    fn record_local_outcome(&self, succeeded: bool) {
        let mut state = self.state();
        if succeeded {
            state.consecutive_local_failures = 0;
            state.local_retry_at = None;
            return;
        }
        state.consecutive_local_failures = state.consecutive_local_failures.saturating_add(1);
        state.local_retry_at = Some(
            unix_timestamp_now()
                + local_failure_retry_delay_seconds(state.consecutive_local_failures),
        );
    }

    fn finish(&self, report: ErvWriteReport, status: Option<&ErvDeviceStatus>) {
        let mut state = self.state();
        state.report = report;
        let Some(status) = status else {
            // Nothing was commanded successfully, so power is whatever it was.
            return;
        };

        // Only an observation may claim to know the power state. Trusting an
        // assumed one would classify the next on-to-on command as speed-only
        // and skip the cloud check, so an ERV that never ran the scene would
        // keep being reported as ventilating.
        state.last_known_power = match report.source {
            ErvStatusSource::Local | ErvStatusSource::Cloud => Some(status.power),
            ErvStatusSource::Assumed | ErvStatusSource::Unknown => None,
        };
    }

    /// A power transition is the only change the cloud's `switch` bit can
    /// confirm; on a speed-only change it reads `true` before and after.
    fn is_power_transition(&self, speed: ErvFanSpeed) -> bool {
        self.state()
            .last_known_power
            .is_none_or(|power| power != (speed != ErvFanSpeed::Off))
    }

    async fn read_local(&self, config: &ErvConfig) -> Result<ErvDeviceStatus> {
        let result = self.reader.read_status(config).await;
        self.record_local_outcome(result.is_ok());
        match &result {
            Ok(status) => {
                let mut state = self.state();
                state.last_known_power = Some(status.power);
                state.unreported_local_failure = None;
            }
            Err(error) => self.state().unreported_local_failure = Some(format!("{error:#}")),
        }
        result
    }

    async fn write_fallback(
        &self,
        config: &ErvConfig,
        speed: ErvFanSpeed,
        negative_pressure: bool,
    ) -> Option<Result<ErvDeviceStatus>> {
        let fallback = self.fallback.as_ref()?;
        if !config.local_write_fallback_enabled || !config.local_tuya_configured() {
            return None;
        }
        // "Healthy" is the same signal the readback path uses: a local key that
        // has been failing is not going to accept a command either.
        if self
            .state()
            .local_retry_at
            .is_some_and(|retry_at| unix_timestamp_now() < retry_at)
        {
            return None;
        }
        Some(fallback.set_speed(config, speed, negative_pressure).await)
    }

    async fn verify_locally(
        &self,
        config: &ErvConfig,
        speed: ErvFanSpeed,
        negative_pressure: bool,
    ) -> Option<ErvDeviceStatus> {
        if !self.local_read_allowed(config) {
            return None;
        }

        match self.read_local(config).await {
            Ok(status) => {
                // A mismatch is real information, not a reason to fail the
                // write: report what the device says and let policy re-decide.
                if let Err(error) = verify_speed(speed, &status, negative_pressure) {
                    tracing::warn!("ERV scene write did not verify: {error:#}");
                }
                Some(status)
            }
            Err(error) => {
                tracing::warn!("ERV local readback after write failed: {error:#}");
                None
            }
        }
    }

    async fn verify_via_cloud(
        &self,
        config: &ErvConfig,
        speed: ErvFanSpeed,
        negative_pressure: bool,
    ) -> Option<ErvDeviceStatus> {
        let cloud = self.cloud.as_ref()?;
        let device_id = config.device_id.trim();
        if device_id.is_empty() {
            return None;
        }

        match cloud.read_power(device_id).await {
            Ok(Some(power)) if power == (speed != ErvFanSpeed::Off) => {
                Some(assumed_status(speed, negative_pressure))
            }
            Ok(Some(power)) => {
                tracing::warn!(
                    "ERV cloud verification says power={power} after a {} command",
                    speed.as_str()
                );
                let mut dps = Map::new();
                dps.insert(DP_POWER.to_string(), Value::Bool(power));
                Some(ErvDeviceStatus {
                    power,
                    fan_speed: (!power).then_some(ErvFanSpeed::Off),
                    supply_speed: None,
                    exhaust_speed: None,
                    raw_dps: Value::Object(dps),
                })
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!("ERV cloud verification failed: {error:#}");
                None
            }
        }
    }
}

impl ErvSpeedWriter for SplitErvWriter {
    fn smoke_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            if !config.local_readback_active() {
                bail!("ERV local readback is not configured");
            }
            self.read_local(config).await
        })
    }

    fn set_speed<'a>(
        &'a self,
        config: &'a ErvConfig,
        speed: ErvFanSpeed,
        negative_pressure: bool,
    ) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            let power_transition = self.is_power_transition(speed);

            let (report, status) = match self
                .writer
                .set_speed(config, speed, negative_pressure)
                .await
            {
                Ok(status) => (self.writer.last_write_report(), status),
                Err(error) => {
                    let primary_report = self.writer.last_write_report();
                    // A hole in the scene matrix is a configuration error, not
                    // an outage. Writing it locally would deliver the speed
                    // while hiding the hole, so it has to propagate.
                    if error.downcast_ref::<SceneConfigError>().is_some() {
                        self.finish(primary_report, None);
                        return Err(error);
                    }
                    match self.write_fallback(config, speed, negative_pressure).await {
                        Some(Ok(status)) => {
                            tracing::warn!(
                                "ERV scene trigger failed ({error:#}); wrote locally instead"
                            );
                            // The local path demonstrably works. Without this
                            // the old failure count survives, so the next
                            // intermittent failure resumes the exponential
                            // backoff where it left off and can disable the
                            // fallback for an hour despite writes succeeding
                            // in between.
                            self.record_local_outcome(true);
                            let report = self
                                .fallback
                                .as_ref()
                                .map(|fallback| fallback.last_write_report())
                                .unwrap_or_default();
                            (report, status)
                        }
                        Some(Err(fallback_error)) => {
                            // Mark local unhealthy, or the next cloud failure
                            // fires another command at a known-bad local key —
                            // exactly the command pattern that causes the
                            // Err 914 lockout.
                            self.record_local_outcome(false);
                            // The outer report stays Scene, so without this the
                            // state layer records a scene failure and the
                            // local-key counters never move: /status would say
                            // the local key is fine while the fallback is known
                            // bad.
                            self.state().unreported_local_failure =
                                Some(format!("{fallback_error:#}"));
                            self.finish(primary_report, None);
                            return Err(error.context(format!(
                                "ERV local write fallback also failed: {fallback_error:#}"
                            )));
                        }
                        None => {
                            self.finish(primary_report, None);
                            return Err(error);
                        }
                    }
                }
            };

            // A local write verifies itself, so only an assumed status needs
            // the ladder below.
            if report.source != ErvStatusSource::Assumed {
                self.finish(report, Some(&status));
                return Ok(status);
            }

            if config.verify_delay_seconds > 0 {
                time::sleep(Duration::from_secs(config.verify_delay_seconds)).await;
            }

            if let Some(observed) = self.verify_locally(config, speed, negative_pressure).await {
                let report = ErvWriteReport {
                    source: ErvStatusSource::Local,
                    ..report
                };
                self.finish(report, Some(&observed));
                return Ok(observed);
            }

            if power_transition
                && let Some(observed) = self
                    .verify_via_cloud(config, speed, negative_pressure)
                    .await
            {
                let report = ErvWriteReport {
                    source: ErvStatusSource::Cloud,
                    ..report
                };
                self.finish(report, Some(&observed));
                return Ok(observed);
            }

            self.finish(report, Some(&status));
            Ok(status)
        })
    }

    fn last_write_report(&self) -> ErvWriteReport {
        self.state().report
    }

    fn take_local_failure(&self) -> Option<String> {
        self.state().unreported_local_failure.take()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RustuyaErvStatusReader;

impl ErvStatusReader for RustuyaErvStatusReader {
    fn read_status<'a>(&'a self, config: &'a ErvConfig) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            if !config.local_tuya_configured() {
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
        negative_pressure: bool,
    ) -> BoxFutureResult<'a, ErvDeviceStatus> {
        Box::pin(async move {
            if !config.local_tuya_configured() {
                bail!("ERV local Tuya config is incomplete");
            }

            let device = build_rustuya_device(config)?;
            let result = set_rustuya_speed(&device, config, speed, negative_pressure).await;
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
    pub negative_pressure: Option<bool>,
    pub last_speed_changed_at: Option<f64>,
    pub local_key_invalid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ErvManualOverride {
    speed: ErvFanSpeed,
    expires_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct ErvLocalActivity {
    at: f64,
    timestamp: String,
    event: &'static str,
    target_speed: Option<ErvFanSpeed>,
    reason: Option<String>,
    co2_ppm: Option<i64>,
    message: Option<String>,
    recent_write_attempts_5m: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
struct ErvWriteAttempt {
    timestamp: String,
    recent_write_attempts_5m: usize,
    suppressed: bool,
    recent_activity: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ErvState {
    inner: Arc<RwLock<ErvInner>>,
    local_io_lock: Arc<AsyncMutex<()>>,
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
    consecutive_local_failures: u64,
    consecutive_scene_failures: u64,
    /// Backoff on the *write* path: burst suppression and failed writes.
    next_write_retry_at: Option<f64>,
    /// Backoff on the *read* path: failed local status reads.
    next_read_retry_at: Option<f64>,
    recent_local_activity: VecDeque<ErvLocalActivity>,
}

impl ErvState {
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            inner: Arc::default(),
            local_io_lock: Arc::default(),
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
        let _local_io_guard = self.local_io_lock.lock().await;
        self.refresh_with_locked(config, reader).await
    }

    async fn refresh_with_locked<R>(
        &self,
        config: &ErvConfig,
        reader: &R,
    ) -> Result<ErvDeviceStatus>
    where
        R: ErvStatusReader + ?Sized,
    {
        match reader.read_status(config).await {
            Ok(status) => {
                if self.record_status_success(status.clone(), ErvStatusSource::Local) {
                    self.notify_status();
                }
                Ok(status)
            }
            Err(error) => {
                let message = format!("{error:#}");
                if self.record_read_only_local_failure(config, &message) {
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
        negative_pressure: bool,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.active_control_enabled {
            bail!("ERV active control is disabled");
        }
        if !config.write_transport_configured() {
            bail!("ERV control config is incomplete");
        }

        let _local_io_guard = self.local_io_lock.lock().await;
        if self.would_suppress_write_attempt(reason) {
            let attempt = self.record_write_attempt(speed, reason, co2_ppm);
            if attempt.suppressed {
                self.record_write_burst_suppression(speed, reason, co2_ppm, &attempt);
                bail!(
                    "ERV automated local write suppressed by burst guard after {} attempts in {} seconds",
                    attempt.recent_write_attempts_5m,
                    LOCAL_WRITE_BURST_WINDOW_SECONDS as u64
                );
            }
        }

        // On the scene path the pre-write read is an optimisation that skips
        // redundant writes, not a gate: requiring it to succeed is what let one
        // dead local key take out every control path for a month.
        //
        // In local control mode it is still a gate, because there the local key
        // *is* the control path. Writing through credentials a read just
        // rejected is the command pattern that produces the lockout.
        let local_control = !config.scene_control_selected();
        if self.pre_write_read_allowed(config) {
            match self.smoke_status_with_locked(config, writer).await {
                Ok(status) if device_status_matches_target(&status, speed, negative_pressure) => {
                    return Ok(status);
                }
                Ok(_) => {}
                Err(error) if local_control => {
                    return Err(error.context("ERV read before a local write failed"));
                }
                Err(error) => {
                    tracing::warn!("ERV read before write failed, writing anyway: {error:#}");
                }
            }
        } else if local_control && config.local_readback_active() {
            // The read was skipped because reads are in failure backoff, which
            // in local mode means the write would fail the same way.
            bail!("ERV local reads are in failure backoff; refusing to issue a local command");
        }

        self.write_speed_after_gate_locked(
            config,
            writer,
            speed,
            negative_pressure,
            reason,
            co2_ppm,
        )
        .await
    }

    /// Only read before writing when local readback is available and not in
    /// failure backoff. In scene mode with a dead local key this simply skips.
    fn pre_write_read_allowed(&self, config: &ErvConfig) -> bool {
        config.local_readback_active() && self.read_retry_allowed(unix_timestamp_now())
    }

    pub(crate) async fn set_speed_after_smoke_with<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
        speed: ErvFanSpeed,
        negative_pressure: bool,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.active_control_enabled {
            bail!("ERV active control is disabled");
        }
        if !config.write_transport_configured() {
            bail!("ERV control config is incomplete");
        }

        let _local_io_guard = self.local_io_lock.lock().await;
        self.write_speed_after_gate_locked(
            config,
            writer,
            speed,
            negative_pressure,
            reason,
            co2_ppm,
        )
        .await
    }

    async fn write_speed_after_gate_locked<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
        speed: ErvFanSpeed,
        negative_pressure: bool,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        let attempt = self.record_write_attempt(speed, reason, co2_ppm);
        if attempt.suppressed {
            self.record_write_burst_suppression(speed, reason, co2_ppm, &attempt);
            bail!(
                "ERV automated local write suppressed by burst guard after {} attempts in {} seconds",
                attempt.recent_write_attempts_5m,
                LOCAL_WRITE_BURST_WINDOW_SECONDS as u64
            );
        }

        match writer.set_speed(config, speed, negative_pressure).await {
            Ok(status) => {
                let report = writer.last_write_report();
                // An observation that contradicts the target means the command
                // did not land. Keep the observed status -- it is the truth --
                // but do not claim the speed changed: that would start the
                // dwell timer and suppress the corrective command for the
                // whole dwell window. The burst guard, not dwell, is what
                // bounds retries here.
                let landed = !matches!(
                    report.source,
                    ErvStatusSource::Local | ErvStatusSource::Cloud
                ) || device_status_matches_target(&status, speed, negative_pressure);

                if landed {
                    self.record_speed_success(status.clone(), report, speed, reason, co2_ppm);
                    self.record_write_success(
                        status.clone(),
                        report,
                        speed,
                        reason,
                        co2_ppm,
                        &attempt,
                    );
                } else {
                    self.record_status_success(status.clone(), report.source);
                    self.record_write_unverified(status.clone(), report, speed, reason, co2_ppm);
                    self.notify_status();
                }

                self.record_unreported_local_failure(config, writer);
                Ok(status)
            }
            Err(error) => {
                let report = writer.last_write_report();
                let message = format!("{error:#}");
                self.record_write_failure(report, speed, reason, co2_ppm, &message);
                self.record_missing_scene(&error);
                match report.transport {
                    ErvTransport::Local => {
                        self.record_local_failure(config, &message);
                    }
                    ErvTransport::Scene => self.record_scene_failure(&message),
                }
                // A scene report can still hide a local failure underneath it:
                // a fallback write that was attempted and failed.
                self.record_unreported_local_failure(config, writer);
                self.notify_status();
                Err(error)
            }
        }
    }

    /// Drain any local failure the writer saw that the state layer has not
    /// recorded yet: a failed read-after-write, or a failed fallback write
    /// hidden under a scene report. Nothing polls any more, so these are often
    /// the only evidence that the local path has gone bad.
    fn record_unreported_local_failure<W>(&self, config: &ErvConfig, writer: &W)
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if let Some(message) = writer.take_local_failure()
            && self.record_read_only_local_failure(config, &message)
        {
            self.notify_status();
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
        let negative_pressure = inner
            .latest_status
            .as_ref()
            .and_then(ErvDeviceStatus::negative_pressure);

        ErvRuntimeSnapshot {
            status_known,
            running,
            speed,
            negative_pressure,
            last_speed_changed_at: inner.last_speed_changed_at,
            local_key_invalid: inner.control.local_key_invalid,
        }
    }

    /// Whether the write path is out of backoff. Failed local *reads* never set
    /// this — a stale local key costs speed readback, never control.
    pub fn write_retry_allowed(&self, now: f64) -> bool {
        self.inner
            .read()
            .expect("ERV state lock poisoned")
            .next_write_retry_at
            .is_none_or(|retry_at| now >= retry_at)
    }

    #[cfg(test)]
    fn clear_read_backoff_for_test(&self) {
        self.inner
            .write()
            .expect("ERV state lock poisoned")
            .next_read_retry_at = None;
    }

    /// Whether a local status read is out of backoff.
    pub fn read_retry_allowed(&self, now: f64) -> bool {
        self.inner
            .read()
            .expect("ERV state lock poisoned")
            .next_read_retry_at
            .is_none_or(|retry_at| now >= retry_at)
    }

    pub async fn smoke_status_with<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        if !config.write_transport_configured() {
            bail!("ERV control config is incomplete");
        }

        let _local_io_guard = self.local_io_lock.lock().await;
        self.smoke_status_with_locked(config, writer).await
    }

    async fn smoke_status_with_locked<W>(
        &self,
        config: &ErvConfig,
        writer: &W,
    ) -> Result<ErvDeviceStatus>
    where
        W: ErvSpeedWriter + ?Sized,
    {
        match writer.smoke_status(config).await {
            Ok(status) => {
                if self.record_status_success(status.clone(), ErvStatusSource::Local) {
                    self.notify_status();
                }
                Ok(status)
            }
            Err(error) => {
                // This failure is being recorded right here, so drain the
                // writer's slot: it exists to carry failures the state layer
                // has *not* seen, and leaving this one would let the write
                // path count the same read a second time.
                let _ = writer.take_local_failure();

                // A failed *read* backs off reads only. Letting it back off the
                // write path is what coupled speed readback to control.
                let message = format!("{error:#}");
                if self.record_read_only_local_failure(config, &message) {
                    self.notify_status();
                }
                Err(error)
            }
        }
    }

    fn would_suppress_write_attempt(&self, reason: &str) -> bool {
        if write_burst_guard_exempt(reason) {
            return false;
        }
        let at = unix_timestamp_now();
        let inner = self.inner.read().expect("ERV state lock poisoned");
        recent_write_attempts_locked(&inner, at, LOCAL_WRITE_BURST_WINDOW_SECONDS) + 1
            > LOCAL_WRITE_BURST_ATTEMPT_LIMIT
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

    fn record_write_attempt(
        &self,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) -> ErvWriteAttempt {
        let at = unix_timestamp_now();
        let timestamp = local_iso_now();
        let snapshot = self.snapshot();
        let (recent_write_attempts_5m, recent_activity) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let recent_write_attempts_5m =
                recent_write_attempts_locked(&inner, at, LOCAL_WRITE_BURST_WINDOW_SECONDS) + 1;
            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: timestamp.clone(),
                    event: "local_write_attempt",
                    target_speed: Some(speed),
                    reason: Some(reason.to_string()),
                    co2_ppm,
                    message: None,
                    recent_write_attempts_5m: Some(recent_write_attempts_5m),
                },
            );
            (
                recent_write_attempts_5m,
                recent_local_activity_json_locked(&inner),
            )
        };
        let suppressed = recent_write_attempts_5m > LOCAL_WRITE_BURST_ATTEMPT_LIMIT
            && !write_burst_guard_exempt(reason);

        self.log_health_event(
            "local_write_attempt",
            json!({
                "type": "erv_local_write_attempt",
                "at": timestamp,
                "target_speed": speed.as_str(),
                "reason": reason,
                "co2_ppm": co2_ppm,
                "previous_status": runtime_snapshot_json(snapshot),
                "recent_write_attempts_5m": recent_write_attempts_5m,
                "burst_window_seconds": LOCAL_WRITE_BURST_WINDOW_SECONDS,
                "burst_attempt_limit": LOCAL_WRITE_BURST_ATTEMPT_LIMIT,
                "burst_guard_would_suppress": suppressed,
                "recent_local_activity": recent_activity,
            }),
        );

        ErvWriteAttempt {
            timestamp,
            recent_write_attempts_5m,
            suppressed,
            recent_activity,
        }
    }

    fn record_write_burst_suppression(
        &self,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
        attempt: &ErvWriteAttempt,
    ) {
        let at = unix_timestamp_now();
        let timestamp = local_iso_now();
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            inner.next_write_retry_at = Some(at + LOCAL_WRITE_BURST_WINDOW_SECONDS);
            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: timestamp.clone(),
                    event: "local_write_burst_suppressed",
                    target_speed: Some(speed),
                    reason: Some(reason.to_string()),
                    co2_ppm,
                    message: Some("automated write burst guard".to_string()),
                    recent_write_attempts_5m: Some(attempt.recent_write_attempts_5m),
                },
            );
        }

        self.log_health_event(
            "local_write_burst_suppressed",
            json!({
                "type": "erv_local_write_burst_suppressed",
                "at": timestamp,
                "target_speed": speed.as_str(),
                "reason": reason,
                "co2_ppm": co2_ppm,
                "recent_write_attempts_5m": attempt.recent_write_attempts_5m,
                "attempted_at": attempt.timestamp.clone(),
                "burst_window_seconds": LOCAL_WRITE_BURST_WINDOW_SECONDS,
                "burst_attempt_limit": LOCAL_WRITE_BURST_ATTEMPT_LIMIT,
                "retry_after_seconds": LOCAL_WRITE_BURST_WINDOW_SECONDS,
                "recent_local_activity": self.recent_local_activity_json(),
            }),
        );
    }

    fn record_write_success(
        &self,
        device_status: ErvDeviceStatus,
        report: ErvWriteReport,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
        attempt: &ErvWriteAttempt,
    ) {
        let at = unix_timestamp_now();
        let timestamp = local_iso_now();
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: timestamp.clone(),
                    event: "local_write_success",
                    target_speed: Some(speed),
                    reason: Some(reason.to_string()),
                    co2_ppm,
                    message: None,
                    recent_write_attempts_5m: Some(attempt.recent_write_attempts_5m),
                },
            );
        }

        self.log_health_event(
            "local_write_success",
            json!({
                "type": "erv_local_write_success",
                "at": timestamp,
                "target_speed": speed.as_str(),
                "reason": reason,
                "co2_ppm": co2_ppm,
                "transport": transport_label(report.transport),
                "status_source": report.source.as_str(),
                "device_status": device_status_json(&device_status),
                "recent_write_attempts_5m": attempt.recent_write_attempts_5m,
                "attempted_at": attempt.timestamp.clone(),
                "activity_at_attempt": attempt.recent_activity.clone(),
                "recent_local_activity": self.recent_local_activity_json(),
            }),
        );
    }

    /// The transport accepted the command but the device says otherwise. Not a
    /// failed write and not a landed one: the status is real, the speed change
    /// is not, and no climate action is logged for a change that did not happen.
    fn record_write_unverified(
        &self,
        device_status: ErvDeviceStatus,
        report: ErvWriteReport,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) {
        let at = unix_timestamp_now();
        let timestamp = local_iso_now();
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: timestamp.clone(),
                    event: "write_not_verified",
                    target_speed: Some(speed),
                    reason: Some(reason.to_string()),
                    co2_ppm,
                    message: Some("device state contradicts the commanded speed".to_string()),
                    recent_write_attempts_5m: None,
                },
            );
        }

        self.log_health_event(
            "write_not_verified",
            json!({
                "type": "erv_write_not_verified",
                "at": timestamp,
                "target_speed": speed.as_str(),
                "reason": reason,
                "co2_ppm": co2_ppm,
                "transport": transport_label(report.transport),
                "status_source": report.source.as_str(),
                "device_status": device_status_json(&device_status),
                "recent_local_activity": self.recent_local_activity_json(),
            }),
        );
    }

    fn record_write_failure(
        &self,
        report: ErvWriteReport,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
        message: &str,
    ) {
        let at = unix_timestamp_now();
        let timestamp = local_iso_now();
        let sanitized_message = sanitize_erv_error(message);
        let (recent_write_attempts_5m, recent_activity) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let recent_write_attempts_5m =
                recent_write_attempts_locked(&inner, at, LOCAL_WRITE_BURST_WINDOW_SECONDS);
            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: timestamp.clone(),
                    event: "local_write_failed",
                    target_speed: Some(speed),
                    reason: Some(reason.to_string()),
                    co2_ppm,
                    message: Some(sanitized_message.clone()),
                    recent_write_attempts_5m: Some(recent_write_attempts_5m),
                },
            );
            (
                recent_write_attempts_5m,
                recent_local_activity_json_locked(&inner),
            )
        };

        self.log_health_event(
            "local_write_failed",
            json!({
                "type": "erv_local_write_failed",
                "at": timestamp,
                "target_speed": speed.as_str(),
                "reason": reason,
                "co2_ppm": co2_ppm,
                "transport": transport_label(report.transport),
                "error": sanitized_message,
                "recent_write_attempts_5m": recent_write_attempts_5m,
                "recent_local_activity": recent_activity,
            }),
        );
    }

    /// A scene path that is not configured for this write is a configuration
    /// hole, not a transient failure: surface it instead of letting it look
    /// like an ordinary cloud error.
    ///
    /// A hole stays reported until a write actually lands. An unrelated failure
    /// -- a WAN outage on a configured scene, say -- is no evidence that anyone
    /// went and fixed the configuration, so it must not clear this.
    fn record_missing_scene(&self, error: &anyhow::Error) {
        let Some(missing) = error.downcast_ref::<SceneConfigError>() else {
            return;
        };

        let subject = missing.subject();
        let now = local_iso_now();
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            inner.control.missing_scene = Some(subject.clone());
            inner.notification = Some(missing_scene_notification(missing, &now));
        }

        self.log_health_event(
            "scene_missing",
            json!({
                "type": "erv_scene_missing",
                "at": now,
                "preset": subject,
            }),
        );
    }

    fn recent_local_activity_json(&self) -> Vec<Value> {
        let inner = self.inner.read().expect("ERV state lock poisoned");
        recent_local_activity_json_locked(&inner)
    }

    fn record_speed_success(
        &self,
        device_status: ErvDeviceStatus,
        report: ErvWriteReport,
        speed: ErvFanSpeed,
        reason: &str,
        co2_ppm: Option<i64>,
    ) {
        self.record_status_success(device_status, report.source);
        {
            // Only a landed write clears the write-path backoff and the
            // missing-scene state. A successful read says nothing about
            // whether the transport recovered or the scene hole was filled.
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            if !dwell_exempt(reason) {
                inner.last_speed_changed_at = Some(unix_timestamp_now());
            }
            inner.consecutive_scene_failures = 0;
            inner.next_write_retry_at = None;

            // Clear the alert alongside the flag, and independently of it, so
            // neither piece can be stranded: a stale critical missing-scene
            // notification would otherwise pin itself on clients forever.
            inner.control.missing_scene = None;
            if inner
                .notification
                .as_ref()
                .is_some_and(|notification| notification.notification_type == "erv_scene_missing")
            {
                inner.notification = None;
            }
        }

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

    /// Record a status we believe in, tagged with where it came from. Only a
    /// local observation clears the local-key state — an assumed or cloud
    /// status says nothing about whether local reads work.
    fn record_status_success(
        &self,
        device_status: ErvDeviceStatus,
        source: ErvStatusSource,
    ) -> bool {
        let now = local_iso_now();
        let observed_locally = source == ErvStatusSource::Local;
        let (status_changed, was_invalid, invalid_since) = {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let status_changed = inner.latest_status.as_ref() != Some(&device_status)
                || inner.control.status_source != source;
            let was_invalid = observed_locally && inner.control.local_key_invalid;
            let invalid_since = inner.control.local_key_invalid_since.clone();

            inner.latest_status = Some(device_status);
            inner.control.status_source = source;
            inner.control.last_ok_at = Some(now.clone());
            inner.control.last_error = None;
            inner.control.using_cloud = source == ErvStatusSource::Cloud;

            if observed_locally {
                inner.control.last_local_ok_at = Some(now.clone());
                inner.control.local_key_invalid = false;
                inner.control.local_key_invalid_since = None;
                inner.consecutive_local_failures = 0;
                inner.control.consecutive_local_key_errors = 0;
                inner.next_read_retry_at = None;
            }

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
                    "recent_local_activity": self.recent_local_activity_json(),
                }),
            );
        }
        status_changed || was_invalid
    }

    fn record_local_failure(&self, config: &ErvConfig, message: &str) -> bool {
        self.record_local_failure_with_retry(config, message, true)
    }

    fn record_read_only_local_failure(&self, config: &ErvConfig, message: &str) -> bool {
        self.record_local_failure_with_retry(config, message, false)
    }

    /// A scene trigger failed. This says nothing about the local key, so the
    /// local-key counters must not move; it only backs off the write path.
    fn record_scene_failure(&self, message: &str) {
        let at = unix_timestamp_now();
        let now = local_iso_now();
        let sanitized_message = sanitize_erv_error(message);
        let mut inner = self.inner.write().expect("ERV state lock poisoned");

        inner.control.last_error = Some(format!("Scene trigger failed: {sanitized_message}"));
        inner.control.last_error_at = Some(now.clone());
        inner.control.using_cloud = false;
        inner.consecutive_scene_failures = inner.consecutive_scene_failures.saturating_add(1);
        inner.next_write_retry_at =
            Some(at + local_failure_retry_delay_seconds(inner.consecutive_scene_failures));
        let recent_write_attempts_5m =
            recent_write_attempts_locked(&inner, at, LOCAL_WRITE_BURST_WINDOW_SECONDS);

        push_local_activity_locked(
            &mut inner,
            ErvLocalActivity {
                at,
                timestamp: now,
                event: "scene_write_failed",
                target_speed: None,
                reason: None,
                co2_ppm: None,
                message: Some(sanitized_message),
                recent_write_attempts_5m: Some(recent_write_attempts_5m),
            },
        );
    }

    fn record_local_failure_with_retry(
        &self,
        config: &ErvConfig,
        message: &str,
        active_retry: bool,
    ) -> bool {
        let now = local_iso_now();
        let at = unix_timestamp_now();
        let mut invalid_event = None;
        let mut failure_streak_event = None;
        {
            let mut inner = self.inner.write().expect("ERV state lock poisoned");
            let sanitized_message = sanitize_erv_error(message);
            inner.control.last_error = Some(format!("Local status failed: {sanitized_message}"));
            inner.control.last_error_at = Some(now.clone());
            inner.control.using_cloud = false;
            inner.consecutive_local_failures = inner.consecutive_local_failures.saturating_add(1);
            let retry_at = unix_timestamp_now()
                + local_failure_retry_delay_seconds(inner.consecutive_local_failures);
            inner.next_read_retry_at = Some(retry_at);
            if active_retry {
                inner.next_write_retry_at = Some(retry_at);
            }
            let recent_write_attempts_5m =
                recent_write_attempts_locked(&inner, at, LOCAL_WRITE_BURST_WINDOW_SECONDS);

            push_local_activity_locked(
                &mut inner,
                ErvLocalActivity {
                    at,
                    timestamp: now.clone(),
                    event: "local_failure",
                    target_speed: None,
                    reason: None,
                    co2_ppm: None,
                    message: Some(sanitized_message.clone()),
                    recent_write_attempts_5m: Some(recent_write_attempts_5m),
                },
            );

            if !is_local_key_error(message) {
                inner.control.consecutive_local_key_errors = 0;
                if inner.consecutive_local_failures == LOCAL_FAILURE_RCA_THRESHOLD {
                    failure_streak_event = Some(json!({
                        "type": "erv_local_failure_streak",
                        "at": now,
                        "consecutive_local_failures": inner.consecutive_local_failures,
                        "last_local_ok_at": inner.control.last_local_ok_at,
                        "last_error": inner.control.last_error,
                        "recent_local_activity": recent_local_activity_json_locked(&inner),
                    }));
                }
            } else {
                inner.control.consecutive_local_key_errors += 1;
                if inner.control.consecutive_local_key_errors >= LOCAL_KEY_ERROR_THRESHOLD
                    && !inner.control.local_key_invalid
                {
                    inner.control.local_key_invalid = true;
                    inner.control.local_key_invalid_since = Some(now.clone());
                    inner.notification = Some(invalid_key_notification(
                        &now,
                        config.scene_control_selected(),
                    ));
                    invalid_event = Some(json!({
                        "type": "erv_local_key_invalid",
                        "started_at": now,
                        "consecutive_errors": inner.control.consecutive_local_key_errors,
                        "consecutive_local_failures": inner.consecutive_local_failures,
                        "last_local_ok_at": inner.control.last_local_ok_at,
                        "last_error": inner.control.last_error,
                        "recent_local_activity": recent_local_activity_json_locked(&inner),
                    }));
                }
            }
        }

        if let Some(event) = failure_streak_event {
            self.log_health_event("local_failure_streak", event);
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

fn local_failure_retry_delay_seconds(consecutive_failures: u64) -> f64 {
    let exponent = consecutive_failures.saturating_sub(1).min(8) as i32;
    (LOCAL_FAILURE_BASE_RETRY_SECONDS * 2f64.powi(exponent)).min(LOCAL_FAILURE_MAX_RETRY_SECONDS)
}

fn write_burst_guard_exempt(reason: &str) -> bool {
    matches!(reason, "manual_override")
}

/// Dwell damps oscillation between *policy* speed decisions. Boot recovery is
/// not one: it forces a known state the policy never asked for, so charging its
/// timestamp to the dwell budget would suppress the first real decision for the
/// whole window. The burst guard still bounds the device-facing write rate.
fn dwell_exempt(reason: &str) -> bool {
    matches!(reason, BOOT_RECOVERY_REASON)
}

fn push_local_activity_locked(inner: &mut ErvInner, activity: ErvLocalActivity) {
    while inner.recent_local_activity.len() >= LOCAL_ACTIVITY_HISTORY_LIMIT {
        inner.recent_local_activity.pop_front();
    }
    inner.recent_local_activity.push_back(activity);
}

fn recent_write_attempts_locked(inner: &ErvInner, now: f64, window_seconds: f64) -> usize {
    inner
        .recent_local_activity
        .iter()
        .filter(|activity| activity.event == "local_write_attempt")
        .filter(|activity| now - activity.at <= window_seconds)
        .count()
}

fn recent_local_activity_json_locked(inner: &ErvInner) -> Vec<Value> {
    inner
        .recent_local_activity
        .iter()
        .map(local_activity_json)
        .collect()
}

fn local_activity_json(activity: &ErvLocalActivity) -> Value {
    json!({
        "at": activity.timestamp.clone(),
        "event": activity.event,
        "target_speed": activity.target_speed.map(ErvFanSpeed::as_str),
        "reason": activity.reason.as_deref(),
        "co2_ppm": activity.co2_ppm,
        "message": activity.message.as_deref(),
        "recent_write_attempts_5m": activity.recent_write_attempts_5m,
    })
}

fn runtime_snapshot_json(snapshot: ErvRuntimeSnapshot) -> Value {
    json!({
        "status_known": snapshot.status_known,
        "running": snapshot.running,
        "speed": snapshot.speed.as_str(),
        "negative_pressure": snapshot.negative_pressure,
        "last_speed_changed_at": snapshot.last_speed_changed_at,
        "local_key_invalid": snapshot.local_key_invalid,
    })
}

fn transport_label(transport: ErvTransport) -> &'static str {
    match transport {
        ErvTransport::Local => "local",
        ErvTransport::Scene => "scene",
    }
}

fn device_status_json(status: &ErvDeviceStatus) -> Value {
    json!({
        "power": status.power,
        "fan_speed": status.fan_speed.map(ErvFanSpeed::as_str),
        "supply_speed": status.supply_speed,
        "exhaust_speed": status.exhaust_speed,
    })
}

fn sanitize_erv_error(message: &str) -> String {
    const MAX_ERROR_CHARS: usize = 512;
    let sanitized = message
        .chars()
        .map(|character| {
            if character.is_control() && character != '\n' && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();

    if sanitized.chars().count() <= MAX_ERROR_CHARS {
        return sanitized;
    }

    let mut truncated = sanitized.chars().take(MAX_ERROR_CHARS).collect::<String>();
    truncated.push_str("...");
    truncated
}

/// Establish true state once at startup, replacing the status poll loop.
///
/// The loop it replaces issued 288-1440 local reads a day and its failures were
/// the trigger for the Err 914 lockout. Everything after boot is read-after-write.
/// If the boot read fails and scenes can control the unit, force a known state
/// by triggering the off scene rather than running blind.
/// The boot read goes through the *writer's* own read path, not a fresh
/// reader, so a failure lands in that writer's local-health state. Otherwise a
/// failed boot read followed by a failed off-scene trigger would find a
/// pristine health gate and fire a local command at the path that just failed.
///
/// Pass the same writer the policy coordinator uses. A second instance has its
/// own health state, which puts the pristine-gate hole back one level up.
pub async fn run_erv_boot_read<W>(config: &AppConfig, erv: &ErvState, writer: &W)
where
    W: ErvSpeedWriter + ?Sized,
{
    // Say so at startup rather than at the first ventilation request. This is
    // the moment after someone re-arms negative pressure with a date change,
    // which is exactly when a scene id nobody has looked at in months starts
    // carrying every command.
    if config.erv.scene_control_selected()
        && let Err(error) = check_erv_scene_config(config)
    {
        tracing::warn!("ERV scene configuration is incomplete: {error:#}");
    }

    if config.erv.local_readback_active() {
        match erv.smoke_status_with(&config.erv, writer).await {
            Ok(status) => {
                tracing::info!(
                    "ERV boot read: running={} speed={}",
                    status.power,
                    status
                        .fan_speed
                        .map(ErvFanSpeed::as_str)
                        .unwrap_or("unknown")
                );
                return;
            }
            Err(error) => tracing::warn!("ERV boot read failed: {error:#}"),
        }
    } else {
        tracing::info!("ERV local readback is not configured; skipping the boot read");
    }

    // Gate on the selected transport, not on a complete scene matrix: the off
    // scene is the one this needs, and a hole elsewhere in the matrix is no
    // reason to leave a possibly-running ERV in an unknown state. A missing
    // off scene fails loudly on its own.
    if !config.erv.scene_control_selected() || !config.erv.active_control_enabled {
        return;
    }

    // Never overwrite a decision that has already been made. Callers are
    // expected to run this before any policy input can arrive, but a forced
    // off would be the wrong answer if one slipped through.
    if erv.snapshot().last_speed_changed_at.is_some() {
        tracing::info!("ERV boot recovery skipped; a speed was already commanded");
        return;
    }

    match erv
        .set_speed_with(
            &config.erv,
            writer,
            ErvFanSpeed::Off,
            false,
            BOOT_RECOVERY_REASON,
            None,
        )
        .await
    {
        Ok(_) => tracing::info!("ERV boot state unknown; forced off via the Smart Life off scene"),
        Err(error) => tracing::warn!("ERV boot off-scene trigger failed: {error:#}"),
    }
}

/// Assemble the ERV writer for a configuration.
///
/// The default is `SplitErvWriter { writer: SceneErvSpeedWriter, reader:
/// RustuyaErvStatusReader }`: writes go out as scene triggers, reads come back
/// over local Tuya. `control_mode: local` keeps the pre-#154 all-local path.
///
/// Selection follows `control_mode` alone. An incomplete scene set must fail
/// per write with a missing-scene diagnostic, never quietly demote the whole
/// deployment to the local transport this change exists to stop using.
pub fn build_erv_writer(config: &AppConfig) -> Arc<dyn ErvSpeedWriter> {
    if !config.erv.scene_control_selected() {
        return Arc::new(RustuyaErvSpeedWriter);
    }

    let cloud: Arc<dyn ErvCloudClient> = Arc::new(SmartLifeErvCloud::new(
        config.smart_life.client_id.clone(),
        &config.erv,
    ));
    let mut writer = SplitErvWriter::new(
        Arc::new(SceneErvSpeedWriter::new(cloud.clone())),
        Arc::new(RustuyaErvStatusReader),
    )
    .with_cloud_verifier(cloud);

    if config.erv.local_write_fallback_enabled && config.erv.local_tuya_configured() {
        writer = writer.with_local_write_fallback(Arc::new(RustuyaErvSpeedWriter));
    }

    Arc::new(writer)
}

/// Check that every preset this deployment will actually command has a scene
/// id configured. Returns how many were checked.
///
/// Pure configuration, no network. A selected transport that cannot deliver is
/// otherwise invisible until ventilation is requested, and that matters most
/// for the negative-pressure scenes: re-arming that mode is a one-line date
/// change made months after anyone last looked at the scene list, and from
/// that moment they are the only scenes automation uses.
///
/// Whether a configured id still *exists* in Smart Life, and whether the
/// credentials work, needs a live call — see the follow-up issue.
pub fn check_erv_scene_config(config: &AppConfig) -> Result<usize> {
    if !config.erv.scene_control_selected() {
        bail!("ERV scene control is not the selected transport");
    }

    // Without a home id no scene can be triggered at all, however complete the
    // matrix looks.
    if config.erv.smart_life_home_id().is_none() {
        bail!("{}", SceneConfigError::MissingHomeId);
    }

    // The matrix this deployment will command, not the one that happens to be
    // filled in. An incomplete set does not demote anything to local control --
    // the affected writes just fail -- so this has to reject it rather than
    // skip itself.
    let required = required_scene_presets(config);
    let unconfigured = required
        .iter()
        .filter(|(_, scene_id)| scene_id.is_none())
        .map(|(label, _)| *label)
        .collect::<Vec<_>>();
    if !unconfigured.is_empty() {
        bail!(
            "ERV scene control is selected but these scene ids are not configured: {}",
            unconfigured.join(", ")
        );
    }

    Ok(required.len())
}

/// Live check of the Smart Life scene transport: credentials, scene id
/// existence, and (if a device id is configured) a device read.
///
/// `check_erv_scene_config` proves the matrix is filled in; it proves nothing
/// about whether any of it works. The auth cache can be missing, unreadable,
/// or holding a dead refresh token, and a configured scene id can be stale,
/// deleted, or mistyped -- none of that shows up until the first ventilation
/// request. This exercises the credentials and confirms every configured
/// scene id still exists in Smart Life, without commanding the device.
///
/// Existence only, per the Hard Constraints in the ERV design doc: the scene
/// read view renders every ERV speed scene's action identically, so nothing
/// may be concluded from it about what a scene *does*. That is verifiable
/// only physically, at the unit.
pub async fn smoke_erv_scene(config: &AppConfig) -> Result<String> {
    let scene_count = check_erv_scene_config(config)?;

    let client = SmartLifeClient::new(
        config.smart_life.client_id.clone(),
        auth_file_or_default(config.erv.smart_life_auth_file.as_ref()),
    );
    let endpoint = client
        .check_credentials()
        .await
        .context("Smart Life credentials are not usable")?;

    // check_erv_scene_config already confirmed a home id is configured.
    let home_id = config
        .erv
        .smart_life_home_id()
        .ok_or(SceneConfigError::MissingHomeId)?;
    let present = client
        .list_scene_ids(home_id)
        .await
        .context("Smart Life scene list failed")?;
    let missing = configured_scene_ids(&config.erv)
        .into_iter()
        .filter(|(_, scene_id)| !present.iter().any(|found| found == scene_id))
        .map(|(label, scene_id)| format!("{label}={scene_id}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "configured ERV scene ids are not present in Smart Life home {home_id}: {}",
            missing.join(", ")
        );
    }

    let device_id = config.erv.device_id.trim();
    if device_id.is_empty() {
        return Ok(format!(
            "Smart Life auth OK at {endpoint}; {scene_count} scene ids present"
        ));
    }

    let status = client
        .device_status(device_id)
        .await
        .context("Smart Life device read failed")?;
    let power = status_code_value(&status, "switch")
        .and_then(value_as_bool)
        .map(|power| power.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Ok(format!(
        "Smart Life auth OK at {endpoint}; {scene_count} scene ids present; switch={power}"
    ))
}

/// Every scene id the configuration actually sets, labelled by preset.
fn configured_scene_ids(config: &ErvConfig) -> Vec<(&'static str, &str)> {
    [
        ("off", &config.off_scene_id),
        ("quiet", &config.quiet_scene_id),
        ("medium", &config.medium_scene_id),
        ("turbo", &config.turbo_scene_id),
        (
            "quiet_negative_pressure",
            &config.quiet_negative_pressure_scene_id,
        ),
        (
            "medium_negative_pressure",
            &config.medium_negative_pressure_scene_id,
        ),
        (
            "turbo_negative_pressure",
            &config.turbo_negative_pressure_scene_id,
        ),
    ]
    .into_iter()
    .filter_map(|(label, configured)| {
        configured
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|scene_id| (label, scene_id))
    })
    .collect()
}

fn configured_scene_id(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// True when automation will pass `negative_pressure = true` on every non-off
/// command, which makes those scenes mandatory rather than optional.
fn negative_pressure_armed(config: &AppConfig) -> bool {
    config.thresholds.post_renovation_negative_pressure
        && config
            .thresholds
            .post_renovation_active_at(unix_timestamp_now())
}

/// The presets this deployment will actually command, and whether each has a
/// scene id. The negative-pressure variants are required only while that mode
/// is armed -- but while it *is* armed they are the only ones automation uses,
/// so treating them as optional would let validation pass a configuration in
/// which every non-off command fails.
fn required_scene_presets(config: &AppConfig) -> Vec<(&'static str, Option<&str>)> {
    let erv = &config.erv;
    let mut required = vec![
        ("off", configured_scene_id(&erv.off_scene_id)),
        ("quiet", configured_scene_id(&erv.quiet_scene_id)),
        ("medium", configured_scene_id(&erv.medium_scene_id)),
        ("turbo", configured_scene_id(&erv.turbo_scene_id)),
    ];
    if negative_pressure_armed(config) {
        required.extend([
            (
                "quiet_negative_pressure",
                configured_scene_id(&erv.quiet_negative_pressure_scene_id),
            ),
            (
                "medium_negative_pressure",
                configured_scene_id(&erv.medium_negative_pressure_scene_id),
            ),
            (
                "turbo_negative_pressure",
                configured_scene_id(&erv.turbo_negative_pressure_scene_id),
            ),
        ]);
    }
    required
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
    negative_pressure: bool,
) -> Result<ErvDeviceStatus> {
    if speed == ErvFanSpeed::Off {
        let result = device
            .set_value(DP_POWER, false)
            .await
            .context("failed to set ERV power off")?;
        ensure_tuya_command_ok("Local set power off failed", result.as_deref())?;
    } else {
        let (supply, exhaust) = speed
            .speed_preset(negative_pressure)
            .expect("non-off speed has preset");
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
    verify_speed(speed, &status, negative_pressure)?;
    Ok(status)
}

fn ensure_tuya_command_ok(context: &str, payload: Option<&str>) -> Result<()> {
    if payload.is_some_and(is_local_key_error) || payload.is_some_and(looks_like_tuya_error) {
        bail!("{context}: {}", payload.expect("payload checked"));
    }
    Ok(())
}

fn verify_speed(
    expected: ErvFanSpeed,
    actual: &ErvDeviceStatus,
    negative_pressure: bool,
) -> Result<()> {
    if expected == ErvFanSpeed::Off {
        if actual.power {
            bail!("ERV verification failed: expected power OFF, got ON");
        }
        return Ok(());
    }

    if !actual.power {
        bail!("ERV verification failed: expected power ON, got OFF");
    }
    let (expected_supply, expected_exhaust) = expected
        .speed_preset(negative_pressure)
        .expect("non-off speed has preset");
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
        (Some(1), Some(2)) => Some(ErvFanSpeed::Quiet),
        (Some(3), Some(2)) => Some(ErvFanSpeed::Medium),
        (Some(2), Some(3)) => Some(ErvFanSpeed::Medium),
        (Some(8), Some(8)) => Some(ErvFanSpeed::Turbo),
        (Some(7), Some(8)) => Some(ErvFanSpeed::Turbo),
        _ => None,
    }
}

impl ErvDeviceStatus {
    fn negative_pressure(&self) -> Option<bool> {
        match (self.supply_speed, self.exhaust_speed) {
            (Some(1), Some(2)) | (Some(2), Some(3)) | (Some(7), Some(8)) => Some(true),
            (Some(1), Some(1)) | (Some(3), Some(2)) | (Some(8), Some(8)) => Some(false),
            _ => None,
        }
    }
}

fn device_status_matches_target(
    status: &ErvDeviceStatus,
    speed: ErvFanSpeed,
    negative_pressure: bool,
) -> bool {
    match speed {
        ErvFanSpeed::Off => !status.power,
        _ => {
            status.power
                && status.fan_speed == Some(speed)
                && status.negative_pressure() == Some(negative_pressure)
        }
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

fn invalid_key_notification(created_at: &str, scene_control_selected: bool) -> AppNotification {
    // With scene control this is a degraded-observability notice, not an
    // outage: writes never touch the local key.
    let (severity, title, message) = if scene_control_selected {
        (
            "warning",
            "ERV speed readback degraded",
            "Local Tuya reads are failing with Err 914, so reported fan speed may be assumed rather than observed. Control is unaffected — writes go through Smart Life scenes. Run docs/tuya-local-key.md to restore readback.",
        )
    } else {
        (
            "critical",
            "ERV local key rotated",
            "Local Tuya control is failing with Err 914. Run docs/tuya-local-key.md to recover it.",
        )
    };

    AppNotification {
        id: format!("erv_local_key_invalid:{created_at}"),
        notification_type: "erv_local_key_invalid".to_string(),
        severity: severity.to_string(),
        title: title.to_string(),
        message: message.to_string(),
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
        title: "ERV local readback recovered".to_string(),
        message: "Local Tuya reads are working again; reported fan speed is observed.".to_string(),
        created_at: Some(created_at.to_string()),
        active: true,
        runbook_path: Some("docs/tuya-local-key.md".to_string()),
    }
}

fn missing_scene_notification(error: &SceneConfigError, created_at: &str) -> AppNotification {
    let subject = error.subject();
    let message = match error {
        SceneConfigError::MissingHomeId => {
            "No Smart Life home id is configured, so no ERV scene can be triggered. Set erv.smart_life_home_id in config.yaml.".to_string()
        }
        SceneConfigError::MissingScene { preset } => format!(
            "No Smart Life scene is configured for {preset}, so that mode cannot be delivered. Scenes are hand-built in the Smart Life app; the API cannot create them."
        ),
    };

    AppNotification {
        id: format!("erv_scene_missing:{subject}:{created_at}"),
        notification_type: "erv_scene_missing".to_string(),
        severity: "critical".to_string(),
        title: "ERV scene control misconfigured".to_string(),
        message,
        created_at: Some(created_at.to_string()),
        active: true,
        runbook_path: Some("docs/working/154_erv_scene_fallback.md".to_string()),
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
            AppConfig, ErvConfig, ErvControlMode, MitsubishiConfig, OrchestratorConfig,
            QingpingConfig, RuntimeConfig, ThresholdsConfig, YoLinkConfig,
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
        report: ErvWriteReport,
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
                report: ErvWriteReport::default(),
            }
        }

        /// Model a scene write whose result nothing could observe.
        fn with_scene_report(mut self) -> Self {
            self.report = ErvWriteReport {
                transport: ErvTransport::Scene,
                source: ErvStatusSource::Assumed,
            };
            self
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
            _negative_pressure: bool,
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

        fn last_write_report(&self) -> ErvWriteReport {
            self.report
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

    /// A fully configured scene write path on top of the local read path.
    fn scene_config() -> ErvConfig {
        ErvConfig {
            smart_life_home_id: Some("home-id".to_string()),
            off_scene_id: Some("off-scene".to_string()),
            quiet_scene_id: Some("quiet-scene".to_string()),
            medium_scene_id: Some("medium-scene".to_string()),
            turbo_scene_id: Some("turbo-scene".to_string()),
            ..active_config()
        }
    }

    fn app_config(erv: ErvConfig) -> AppConfig {
        AppConfig {
            orchestrator: OrchestratorConfig::default(),
            room_mode: crate::config::RoomModeConfig::default(),
            presence: crate::config::PresenceConfig::default(),
            qingping: QingpingConfig::default(),
            yolink: YoLinkConfig::default(),
            artifacts: crate::config::ArtifactConfig::default(),
            cloudflare_access: crate::config::CloudflareAccessConfig::default(),
            erv,
            blinds: crate::config::BlindsConfig::default(),
            smart_life: crate::config::SmartLifeConfig::default(),
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
                session_tool_usage_db_path: PathBuf::from("/tmp/office/data/claude_tool_usage.db"),
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

    #[test]
    fn maps_negative_pressure_presets_and_status_payloads() {
        assert_eq!(ErvFanSpeed::Quiet.speed_preset(true), Some((1, 2)));
        assert_eq!(ErvFanSpeed::Medium.speed_preset(true), Some((2, 3)));
        assert_eq!(ErvFanSpeed::Turbo.speed_preset(true), Some((7, 8)));

        let quiet =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":1,"102":2}}"#).expect("status");
        let medium =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":2,"102":3}}"#).expect("status");
        let turbo =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":7,"102":8}}"#).expect("status");

        assert_eq!(quiet.fan_speed, Some(ErvFanSpeed::Quiet));
        assert_eq!(medium.fan_speed, Some(ErvFanSpeed::Medium));
        assert_eq!(turbo.fan_speed, Some(ErvFanSpeed::Turbo));
    }

    #[test]
    fn target_match_includes_pressure_bias() {
        let normal_quiet =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":1,"102":1}}"#).expect("status");
        let negative_quiet =
            parse_erv_status_payload(r#"{"dps":{"1":true,"101":1,"102":2}}"#).expect("status");

        assert!(device_status_matches_target(
            &normal_quiet,
            ErvFanSpeed::Quiet,
            false
        ));
        assert!(!device_status_matches_target(
            &normal_quiet,
            ErvFanSpeed::Quiet,
            true
        ));
        assert!(device_status_matches_target(
            &negative_quiet,
            ErvFanSpeed::Quiet,
            true
        ));
        assert!(!device_status_matches_target(
            &negative_quiet,
            ErvFanSpeed::Quiet,
            false
        ));
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

        let history = db::read_history(&database_path, 1, 10).expect("history");
        let event = history
            .device_events
            .iter()
            .find(|event| event["event"] == "local_key_invalid")
            .expect("local key invalid event");
        let details = event["details"]
            .as_str()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .expect("event details");
        assert_eq!(details["consecutive_errors"], LOCAL_KEY_ERROR_THRESHOLD);
        assert_eq!(
            details["recent_local_activity"]
                .as_array()
                .expect("recent local activity")
                .last()
                .expect("last activity")["event"],
            "local_failure"
        );
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
    async fn local_read_failure_backs_off_reads_without_blocking_writes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let reader = FakeErvReader::new(vec![
            Err(anyhow!("Connection reset by peer")),
            Err(anyhow!("Connection reset by peer")),
        ]);

        assert!(state.refresh_with(&test_config(), &reader).await.is_err());

        let now = unix_timestamp_now();
        assert!(state.write_retry_allowed(now));
        assert!(!state.read_retry_allowed(now));
        assert!(state.read_retry_allowed(now + LOCAL_FAILURE_BASE_RETRY_SECONDS + 1.0));

        assert!(state.refresh_with(&test_config(), &reader).await.is_err());
        let now = unix_timestamp_now();
        assert!(state.write_retry_allowed(now));
        assert!(!state.read_retry_allowed(now + LOCAL_FAILURE_BASE_RETRY_SECONDS + 1.0));
        assert!(state.read_retry_allowed(now + LOCAL_FAILURE_BASE_RETRY_SECONDS * 2.0 + 1.0));
    }

    #[tokio::test]
    async fn successful_local_read_clears_the_read_backoff() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let reader = FakeErvReader::new(vec![
            Err(anyhow!("Connection reset by peer")),
            Ok(medium_status()),
        ]);

        assert!(state.refresh_with(&test_config(), &reader).await.is_err());
        assert!(!state.read_retry_allowed(unix_timestamp_now()));

        state
            .refresh_with(&test_config(), &reader)
            .await
            .expect("read succeeds");

        assert!(state.read_retry_allowed(unix_timestamp_now()));
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
                false,
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
                false,
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
        assert!(
            history
                .device_events
                .iter()
                .any(|event| event["event"] == "local_write_attempt")
        );
        let success_event = history
            .device_events
            .iter()
            .find(|event| event["event"] == "local_write_success")
            .expect("local write success event");
        let details = success_event["details"]
            .as_str()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .expect("event details");
        assert_eq!(details["target_speed"], "turbo");
        assert_eq!(details["reason"], "away_refresh_CO2=2100ppm");
        assert_eq!(details["device_status"]["fan_speed"], "turbo");
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
                false,
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
        assert!(
            history
                .device_events
                .iter()
                .all(|event| event["event"] != "local_write_attempt")
        );
    }

    #[tokio::test]
    async fn automated_write_burst_guard_suppresses_fourth_policy_write() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let writer = FakeErvWriter::new(
            Vec::new(),
            vec![
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
            ],
        );

        for index in 0..LOCAL_WRITE_BURST_ATTEMPT_LIMIT {
            state
                .set_speed_after_smoke_with(
                    &active_config(),
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    &format!("away_refresh_{index}"),
                    Some(900),
                )
                .await
                .expect("write succeeds before burst guard");
        }

        let error = state
            .set_speed_after_smoke_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh_suppressed",
                Some(900),
            )
            .await
            .expect_err("fourth automated write suppressed");

        assert!(error.to_string().contains("burst guard"));
        assert_eq!(
            writer.write_speeds(),
            vec![ErvFanSpeed::Turbo, ErvFanSpeed::Turbo, ErvFanSpeed::Turbo]
        );
        assert!(!state.write_retry_allowed(unix_timestamp_now()));

        let history = db::read_history(&database_path, 1, 20).expect("history");
        let suppressed = history
            .device_events
            .iter()
            .find(|event| event["event"] == "local_write_burst_suppressed")
            .expect("burst suppression event");
        let details = suppressed["details"]
            .as_str()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .expect("event details");
        assert_eq!(
            details["recent_write_attempts_5m"],
            (LOCAL_WRITE_BURST_ATTEMPT_LIMIT + 1) as i64
        );
        assert_eq!(details["target_speed"], "turbo");
    }

    #[tokio::test]
    async fn safety_interlock_respects_write_burst_guard() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            Vec::new(),
            vec![
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
            ],
        );

        for _ in 0..LOCAL_WRITE_BURST_ATTEMPT_LIMIT {
            state
                .set_speed_after_smoke_with(
                    &active_config(),
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    "away_refresh",
                    Some(900),
                )
                .await
                .expect("write succeeds before burst guard");
        }

        let error = state
            .set_speed_after_smoke_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "safety_interlock",
                Some(900),
            )
            .await
            .expect_err("safety write respects burst guard");

        assert!(error.to_string().contains("burst guard"));
        assert_eq!(writer.write_speeds().len(), LOCAL_WRITE_BURST_ATTEMPT_LIMIT);
        assert!(!state.write_retry_allowed(unix_timestamp_now()));
    }

    #[tokio::test]
    async fn burst_guard_blocks_pre_write_smoke_check() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            vec![Ok(medium_status())],
            vec![
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
                Ok(turbo_status()),
            ],
        );

        for index in 0..LOCAL_WRITE_BURST_ATTEMPT_LIMIT {
            state
                .set_speed_after_smoke_with(
                    &active_config(),
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    &format!("away_refresh_{index}"),
                    Some(900),
                )
                .await
                .expect("write succeeds before burst guard");
        }

        let error = state
            .set_speed_with(
                &active_config(),
                &writer,
                ErvFanSpeed::Quiet,
                false,
                "away_refresh_suppressed",
                Some(900),
            )
            .await
            .expect_err("burst guard should fail before smoke");

        assert!(error.to_string().contains("burst guard"));
        assert_eq!(writer.smoke_calls(), 0);
        assert_eq!(writer.write_speeds().len(), LOCAL_WRITE_BURST_ATTEMPT_LIMIT);
    }

    /// The regression this whole change exists for: repeated Err 914 reads mark
    /// the local key invalid, and control keeps working through all of it.
    #[tokio::test]
    async fn local_key_failures_degrade_readback_without_closing_the_write_gate() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            (0..LOCAL_KEY_ERROR_THRESHOLD)
                .map(|_| bail!("Check device key or version (Error 914)"))
                .collect(),
            (0..LOCAL_KEY_ERROR_THRESHOLD + 1)
                .map(|_| Ok(turbo_status()))
                .collect(),
        )
        .with_scene_report();

        // Reads keep failing, but every write lands. The read backoff is
        // stepped over here so each attempt actually tries a read.
        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            state
                .set_speed_with(
                    &scene_config(),
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    "manual_override",
                    None,
                )
                .await
                .expect("write succeeds while reads fail");
            state.clear_read_backoff_for_test();
        }

        let mut status = Status::read_only_default(&app_config(scene_config()));
        state.overlay_status(&mut status);
        assert!(status.erv.control.local_key_invalid);
        assert_eq!(
            status.notifications[0].notification_type,
            "erv_local_key_invalid"
        );
        assert_eq!(status.notifications[0].severity, "warning");
        assert_eq!(writer.smoke_calls(), LOCAL_KEY_ERROR_THRESHOLD as usize);
        assert_eq!(
            writer.write_speeds().len(),
            LOCAL_KEY_ERROR_THRESHOLD as usize
        );

        // And a write issued once the key is already marked invalid still goes.
        state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Quiet,
                false,
                "manual_override",
                None,
            )
            .await
            .expect("invalid local key must not block control");
        assert_eq!(
            writer.write_speeds().len(),
            LOCAL_KEY_ERROR_THRESHOLD as usize + 1
        );
    }

    #[derive(Default)]
    struct FakeCloud {
        triggers: Mutex<Vec<(String, String)>>,
        trigger_results: Mutex<VecDeque<Result<()>>>,
        power_results: Mutex<VecDeque<Result<Option<bool>>>>,
        power_reads: AtomicUsize,
    }

    impl FakeCloud {
        fn failing(errors: usize) -> Self {
            Self {
                trigger_results: Mutex::new(
                    (0..errors)
                        .map(|_| Err(anyhow!("Smart Life API error code=1106")))
                        .collect(),
                ),
                ..Self::default()
            }
        }

        fn with_power_reads(self, results: Vec<Result<Option<bool>>>) -> Self {
            *self.power_results.lock().expect("power lock") = results.into();
            self
        }

        fn triggered_scenes(&self) -> Vec<String> {
            self.triggers
                .lock()
                .expect("trigger lock")
                .iter()
                .map(|(_, scene_id)| scene_id.clone())
                .collect()
        }

        fn power_reads(&self) -> usize {
            self.power_reads.load(Ordering::SeqCst)
        }
    }

    impl ErvCloudClient for FakeCloud {
        fn trigger_scene<'a>(
            &'a self,
            home_id: &'a str,
            scene_id: &'a str,
        ) -> BoxFutureResult<'a, ()> {
            self.triggers
                .lock()
                .expect("trigger lock")
                .push((home_id.to_string(), scene_id.to_string()));
            let result = self
                .trigger_results
                .lock()
                .expect("trigger results lock")
                .pop_front()
                .unwrap_or(Ok(()));
            Box::pin(async move { result })
        }

        fn read_power<'a>(&'a self, _device_id: &'a str) -> BoxFutureResult<'a, Option<bool>> {
            self.power_reads.fetch_add(1, Ordering::SeqCst);
            let result = self
                .power_results
                .lock()
                .expect("power lock")
                .pop_front()
                .unwrap_or(Ok(None));
            Box::pin(async move { result })
        }
    }

    fn split_writer(
        cloud: Arc<FakeCloud>,
        reader: Arc<FakeErvReader>,
        fallback: Option<Arc<FakeErvWriter>>,
    ) -> SplitErvWriter {
        let mut writer =
            SplitErvWriter::new(Arc::new(SceneErvSpeedWriter::new(cloud.clone())), reader)
                .with_cloud_verifier(cloud);
        if let Some(fallback) = fallback {
            writer = writer.with_local_write_fallback(fallback);
        }
        writer
    }

    /// The property the whole design rests on: in the default configuration a
    /// write reaches the device as a scene trigger and never as a local
    /// command, because local commands are what trigger the Err 914 lockout.
    #[tokio::test]
    async fn default_write_path_triggers_a_scene_and_issues_no_local_command() {
        let cloud = Arc::new(FakeCloud::default());
        let local = Arc::new(FakeErvWriter::new(Vec::new(), Vec::new()));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Ok(turbo_status())])),
            Some(local.clone()),
        );

        let status = writer
            .set_speed(&scene_config(), ErvFanSpeed::Turbo, false)
            .await
            .expect("scene write succeeds");

        assert_eq!(cloud.triggered_scenes(), vec!["turbo-scene".to_string()]);
        assert!(
            local.write_speeds().is_empty(),
            "no local command may be issued on the scene path"
        );
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));
    }

    #[tokio::test]
    async fn scene_write_verified_by_a_local_read_reports_observed_speeds() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                Ok(medium_status()),
                Ok(turbo_status()),
            ])),
            None,
        );

        let status = state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh",
                None,
            )
            .await
            .expect("scene write succeeds");

        assert_eq!(status.supply_speed, Some(8));
        assert_eq!(status.exhaust_speed, Some(8));

        let mut app_status = Status::read_only_default(&app_config(scene_config()));
        state.overlay_status(&mut app_status);
        assert_eq!(app_status.erv.control.status_source, ErvStatusSource::Local);
        assert_eq!(app_status.erv.speed, "turbo");
    }

    #[tokio::test]
    async fn scene_write_survives_a_failed_readback_and_reports_assumed_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                Err(anyhow!("Check device key or version (Error 914)")),
                Err(anyhow!("Check device key or version (Error 914)")),
            ])),
            None,
        );

        let status = state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh",
                None,
            )
            .await
            .expect("a failed readback must not fail the write");

        assert_eq!(cloud.triggered_scenes(), vec!["turbo-scene".to_string()]);
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));

        let mut app_status = Status::read_only_default(&app_config(scene_config()));
        state.overlay_status(&mut app_status);
        assert_eq!(
            app_status.erv.control.status_source,
            ErvStatusSource::Assumed
        );
    }

    /// The cloud only exposes the `switch` bit, so it is worth a call when the
    /// power state changes and pure noise when only the speed does.
    #[tokio::test]
    async fn cloud_verification_runs_on_power_transitions_only() {
        let cloud =
            Arc::new(FakeCloud::default().with_power_reads(vec![Ok(Some(true)), Ok(Some(true))]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                Err(anyhow!("Check device key or version (Error 914)")),
                Err(anyhow!("Check device key or version (Error 914)")),
            ])),
            None,
        );
        let config = scene_config();

        // Power state is unknown, so off -> turbo is a transition.
        writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect("scene write succeeds");
        assert_eq!(cloud.power_reads(), 1);
        assert_eq!(
            writer.last_write_report().source,
            ErvStatusSource::Cloud,
            "a confirmed power bit is better than an assumption"
        );

        // Turbo -> medium leaves the switch on, so there is nothing to learn.
        writer
            .set_speed(&config, ErvFanSpeed::Medium, false)
            .await
            .expect("scene write succeeds");
        assert_eq!(cloud.power_reads(), 1);
        assert_eq!(writer.last_write_report().source, ErvStatusSource::Assumed);
    }

    #[tokio::test]
    async fn failed_scene_trigger_falls_back_to_a_local_write() {
        let cloud = Arc::new(FakeCloud::failing(1));
        let local = Arc::new(FakeErvWriter::new(Vec::new(), vec![Ok(turbo_status())]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            Some(local.clone()),
        );

        let status = writer
            .set_speed(&scene_config(), ErvFanSpeed::Turbo, false)
            .await
            .expect("local fallback keeps the ERV controllable");

        assert_eq!(local.write_speeds(), vec![ErvFanSpeed::Turbo]);
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Turbo));
        assert_eq!(writer.last_write_report().transport, ErvTransport::Local);
        assert_eq!(writer.last_write_report().source, ErvStatusSource::Local);
    }

    #[tokio::test]
    async fn failed_scene_trigger_surfaces_an_error_when_local_is_unhealthy() {
        let cloud = Arc::new(FakeCloud::failing(1));
        let local = Arc::new(FakeErvWriter::new(Vec::new(), vec![Ok(turbo_status())]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Check device key or version (Error 914)"
            ))])),
            Some(local.clone()),
        );
        let config = scene_config();

        // A failed read is what marks local unhealthy.
        assert!(writer.smoke_status(&config).await.is_err());

        let error = writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect_err("no usable transport should surface an error");

        assert!(format!("{error:#}").contains("Smart Life API error"));
        assert!(
            local.write_speeds().is_empty(),
            "an unhealthy local path must not be used as a fallback"
        );
    }

    #[tokio::test]
    async fn missing_scene_fails_loudly_instead_of_substituting() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        // The fallback is configured and healthy, as it is by default. A hole
        // in the scene matrix must still surface instead of being papered over
        // by a local write that happens to be able to deliver the speed.
        let local = Arc::new(FakeErvWriter::new(Vec::new(), vec![Ok(turbo_status())]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            Some(local.clone()),
        );

        // Negative-pressure turbo has no configured scene here.
        let error = state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Turbo,
                true,
                "away_refresh",
                None,
            )
            .await
            .expect_err("a missing scene must not fall back to another preset");

        assert!(error.downcast_ref::<SceneConfigError>().is_some());
        assert!(
            cloud.triggered_scenes().is_empty(),
            "the normal-pressure scene must not be substituted"
        );
        assert!(
            local.write_speeds().is_empty(),
            "a configuration hole must not be written away by the local fallback"
        );

        let mut app_status = Status::read_only_default(&app_config(scene_config()));
        state.overlay_status(&mut app_status);
        assert_eq!(
            app_status.erv.control.missing_scene.as_deref(),
            Some("turbo (negative pressure)")
        );
        assert_eq!(
            app_status.notifications[0].notification_type,
            "erv_scene_missing"
        );

        // Once the scene is configured and a write lands, the critical alert
        // has to go with it -- clearing only `missing_scene` would leave
        // clients showing the alert forever.
        let repaired = ErvConfig {
            turbo_negative_pressure_scene_id: Some("turbo-np-scene".to_string()),
            ..scene_config()
        };
        state
            .set_speed_with(
                &repaired,
                &writer,
                ErvFanSpeed::Turbo,
                true,
                "away_refresh",
                None,
            )
            .await
            .expect("write succeeds once the scene exists");

        let mut app_status = Status::read_only_default(&app_config(repaired));
        state.overlay_status(&mut app_status);
        assert!(app_status.erv.control.missing_scene.is_none());
        assert!(
            app_status
                .notifications
                .iter()
                .all(|notification| notification.notification_type != "erv_scene_missing"),
            "a stale missing-scene alert survived recovery"
        );
    }

    #[tokio::test]
    async fn boot_read_forces_a_known_state_when_the_local_read_fails() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Connection reset by peer"
            ))])),
            None,
        );

        run_erv_boot_read(&app_config(scene_config()), &state, &writer).await;

        assert_eq!(cloud.triggered_scenes(), vec!["off-scene".to_string()]);
        assert!(!state.snapshot().running);
    }

    /// The boot read must feed the writer's own local health. Otherwise a
    /// failed boot read plus a failed off-scene trigger finds a pristine
    /// health gate and fires a local command at the path that just failed --
    /// the exact WAN-outage-plus-Err-914 case the gate exists for.
    #[tokio::test]
    async fn failed_boot_read_marks_local_unhealthy_for_the_fallback() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::failing(1));
        let local = Arc::new(FakeErvWriter::new(Vec::new(), vec![Ok(turbo_status())]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Check device key or version (Error 914)"
            ))])),
            Some(local.clone()),
        );

        run_erv_boot_read(&app_config(scene_config()), &state, &writer).await;

        assert!(
            local.write_speeds().is_empty(),
            "a local path that just failed its read must not be used as a fallback"
        );
    }

    /// Boot recovery forces a state the policy never asked for, so it must not
    /// spend the dwell budget. Otherwise the first real decision after startup
    /// -- an AWAY ventilation command, or a PRESENT air-quality response -- is
    /// suppressed for the whole dwell window while the ERV sits off.
    #[tokio::test]
    async fn boot_recovery_does_not_start_the_policy_dwell_timer() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Connection reset by peer"
            ))])),
            None,
        );

        run_erv_boot_read(&app_config(scene_config()), &state, &writer).await;

        assert_eq!(cloud.triggered_scenes(), vec!["off-scene".to_string()]);
        assert!(
            state.snapshot().last_speed_changed_at.is_none(),
            "boot recovery charged its forced off to the dwell budget"
        );

        // It is still a real action and still logged as one.
        let history = db::read_history(&database_path, 1, 20).expect("history");
        assert_eq!(history.climate_actions[0]["action"], "off");
        assert_eq!(history.climate_actions[0]["reason"], BOOT_RECOVERY_REASON);
    }

    /// Boot recovery must never overwrite a decision that has already landed.
    #[tokio::test]
    async fn boot_recovery_yields_to_an_already_commanded_speed() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                // Pre-write read: not at target, so the write actually goes.
                Ok(medium_status()),
                // Read-after-write.
                Ok(turbo_status()),
                // Boot read, which fails and would otherwise force off.
                Err(anyhow!("Connection reset by peer")),
            ])),
            None,
        );
        let config = app_config(scene_config());

        // A policy decision lands first.
        state
            .set_speed_with(
                &config.erv,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh",
                None,
            )
            .await
            .expect("policy write succeeds");
        let triggered = cloud.triggered_scenes();

        run_erv_boot_read(&config, &state, &writer).await;

        assert_eq!(
            cloud.triggered_scenes(),
            triggered,
            "boot recovery overwrote a newer policy decision"
        );
        assert_eq!(state.snapshot().speed, ErvFanSpeed::Turbo);
    }

    #[tokio::test]
    async fn boot_read_establishes_state_without_touching_the_cloud() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Ok(medium_status())])),
            None,
        );

        run_erv_boot_read(&app_config(scene_config()), &state, &writer).await;

        assert!(cloud.triggered_scenes().is_empty());
        assert_eq!(state.snapshot().speed, ErvFanSpeed::Medium);
    }

    /// Nothing polls any more, so a failed read-after-write is often the only
    /// evidence that the local key has died. It has to reach the health
    /// counters, or `local_key_invalid` and its notification stay clear while
    /// every local read fails.
    #[tokio::test]
    async fn failed_readback_after_write_marks_the_local_key_invalid() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(
                (0..LOCAL_KEY_ERROR_THRESHOLD * 2)
                    .map(|_| bail!("Check device key or version (Error 914)"))
                    .collect(),
            )),
            None,
        );
        let config = scene_config();

        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            state
                .set_speed_with(
                    &config,
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    "manual_override",
                    None,
                )
                .await
                .expect("write succeeds while readback fails");
            state.clear_read_backoff_for_test();
        }

        let mut app_status = Status::read_only_default(&app_config(config));
        state.overlay_status(&mut app_status);
        assert!(
            app_status.erv.control.local_key_invalid,
            "a local key failing every readback was never reported"
        );
        assert_eq!(
            app_status.notifications[0].notification_type,
            "erv_local_key_invalid"
        );
    }

    /// One failed read must be counted once. While the writer is in read
    /// backoff it performs no local I/O at all, so a failure left in place
    /// would be re-counted on every write and manufacture a key-invalid
    /// verdict out of a single Err 914.
    #[tokio::test]
    async fn a_single_readback_failure_is_counted_once() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Check device key or version (Error 914)"
            ))])),
            None,
        );
        let config = scene_config();

        // Both backoffs are left alone after this, so no further read happens.
        for _ in 0..LOCAL_KEY_ERROR_THRESHOLD {
            state
                .set_speed_with(
                    &config,
                    &writer,
                    ErvFanSpeed::Turbo,
                    false,
                    "manual_override",
                    None,
                )
                .await
                .expect("scene writes keep succeeding");
        }

        let mut app_status = Status::read_only_default(&app_config(config));
        state.overlay_status(&mut app_status);
        assert_eq!(
            app_status.erv.control.consecutive_local_key_errors, 1,
            "one failed read was counted more than once"
        );
        assert!(!app_status.erv.control.local_key_invalid);
    }

    /// A missing home id is as deterministic as a missing scene id, and just
    /// as ineligible for a local fallback: no retry fixes configuration, and
    /// the local command is the thing this design exists to never issue.
    #[tokio::test]
    async fn missing_home_id_fails_instead_of_falling_back_to_local() {
        let cloud = Arc::new(FakeCloud::default());
        let local = Arc::new(FakeErvWriter::new(Vec::new(), vec![Ok(turbo_status())]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            Some(local.clone()),
        );
        let config = ErvConfig {
            smart_life_home_id: None,
            ..scene_config()
        };

        let error = writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect_err("no home id means no scene can be triggered");

        assert_eq!(
            error.downcast_ref::<SceneConfigError>(),
            Some(&SceneConfigError::MissingHomeId)
        );
        assert!(
            local.write_speeds().is_empty(),
            "a configuration error must not be written away by the local fallback"
        );
        assert!(cloud.triggered_scenes().is_empty());
    }

    /// A WAN outage on a configured scene is no evidence that anyone built the
    /// missing one, so it must not clear the hole -- least of all leave the
    /// flag cleared and the critical alert stranded.
    #[tokio::test]
    async fn unrelated_failure_does_not_strand_the_missing_scene_alert() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::failing(1));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            None,
        );
        let config = ErvConfig {
            local_readback_enabled: false,
            ..scene_config()
        };

        // A hole in the matrix is reported.
        state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                true,
                "manual_override",
                None,
            )
            .await
            .expect_err("no negative-pressure turbo scene");

        // Then an ordinary cloud failure on a configured scene.
        state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "manual_override",
                None,
            )
            .await
            .expect_err("cloud is down");

        let mut app_status = Status::read_only_default(&app_config(config));
        state.overlay_status(&mut app_status);
        assert_eq!(
            app_status.erv.control.missing_scene.as_deref(),
            Some("turbo (negative pressure)"),
            "an unrelated failure cleared the missing-scene flag"
        );
        assert_eq!(
            app_status.notifications[0].notification_type,
            "erv_scene_missing"
        );
    }

    /// Boot recovery needs the off scene, not the whole matrix. The write gate
    /// must not reject the configuration the boot path deliberately permits.
    #[tokio::test]
    async fn boot_off_scene_survives_the_write_gate_without_local_credentials() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let config = app_config(ErvConfig {
            // Scene-only deployment with one unrelated scene missing.
            ip: String::new(),
            device_id: String::new(),
            local_key: String::new(),
            quiet_scene_id: None,
            ..scene_config()
        });
        assert!(
            !config.erv.is_configured(),
            "the strict gate would reject this"
        );

        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            None,
        );

        run_erv_boot_read(&config, &state, &writer).await;

        assert_eq!(cloud.triggered_scenes(), vec!["off-scene".to_string()]);
    }

    /// A scene the API accepted but the device ignored must not start the
    /// dwell timer. Dwell exists to stop thrash between real speed changes; if
    /// a non-change starts it, the corrective command is suppressed for the
    /// whole dwell window. The burst guard is what bounds retries here.
    #[tokio::test]
    async fn contradicted_write_reports_truth_without_starting_dwell() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path.clone());
        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                // Pre-write read: still at medium.
                Ok(medium_status()),
                // Read-after-write: the scene was accepted but nothing moved.
                Ok(medium_status()),
            ])),
            None,
        );

        let status = state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh",
                None,
            )
            .await
            .expect("a contradicted verification must not fail the write");

        // The observed state is reported truthfully...
        assert_eq!(status.fan_speed, Some(ErvFanSpeed::Medium));
        assert_eq!(state.snapshot().speed, ErvFanSpeed::Medium);
        // ...and the speed is not claimed to have changed.
        assert!(
            state.snapshot().last_speed_changed_at.is_none(),
            "a command the device ignored started the dwell timer"
        );

        let history = db::read_history(&database_path, 1, 20).expect("history");
        assert!(
            history.climate_actions.is_empty(),
            "a change that did not happen was logged as a climate action"
        );
        assert!(
            history
                .device_events
                .iter()
                .any(|event| event["event"] == "write_not_verified")
        );
    }

    /// An unverified write leaves power unknown, so the next command still
    /// pays for a cloud check. Trusting the assumption would let an ERV that
    /// never ran the scene be reported as ventilating indefinitely.
    #[tokio::test]
    async fn assumed_status_does_not_count_as_known_power() {
        let cloud = Arc::new(FakeCloud::default().with_power_reads(vec![
            Err(anyhow!("Smart Life API error code=1106")),
            Ok(Some(true)),
        ]));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![
                Err(anyhow!("Check device key or version (Error 914)")),
                Err(anyhow!("Check device key or version (Error 914)")),
            ])),
            None,
        );
        let config = scene_config();

        // Neither rung confirms anything, so the result is assumed.
        writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect("scene write succeeds");
        assert_eq!(cloud.power_reads(), 1);
        assert_eq!(writer.last_write_report().source, ErvStatusSource::Assumed);

        // Turbo -> medium is on-to-on, but power was never observed, so this
        // must still be treated as worth a cloud check.
        writer
            .set_speed(&config, ErvFanSpeed::Medium, false)
            .await
            .expect("scene write succeeds");
        assert_eq!(
            cloud.power_reads(),
            2,
            "an assumed power state must not suppress cloud verification"
        );
        assert_eq!(writer.last_write_report().source, ErvStatusSource::Cloud);
    }

    /// The off scene is the only one boot recovery needs. A hole elsewhere in
    /// the matrix is no reason to leave a possibly-running ERV unknown.
    #[tokio::test]
    async fn boot_off_scene_runs_with_a_partial_scene_matrix() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let config = app_config(ErvConfig {
            quiet_scene_id: None,
            ..scene_config()
        });
        assert!(
            !config.erv.scene_configured(),
            "the scene set is deliberately incomplete"
        );

        let cloud = Arc::new(FakeCloud::default());
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Connection reset by peer"
            ))])),
            None,
        );

        run_erv_boot_read(&config, &state, &writer).await;

        assert_eq!(cloud.triggered_scenes(), vec!["off-scene".to_string()]);
    }

    /// §6 scopes "a failed read must not block control" to the scene path. In
    /// local control mode the local key *is* control, so writing through
    /// credentials a read just rejected is the command pattern that produces
    /// the lockout.
    #[tokio::test]
    async fn local_control_mode_keeps_the_failed_read_gate() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            vec![Err(anyhow!("Check device key or version (Error 914)"))],
            vec![Ok(turbo_status())],
        );
        let config = ErvConfig {
            control_mode: ErvControlMode::Local,
            ..active_config()
        };

        let error = state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "manual_override",
                None,
            )
            .await
            .expect_err("a rejected read must not be followed by a local command");

        assert!(format!("{error:#}").contains("read before a local write failed"));
        assert!(writer.write_speeds().is_empty());

        // And once reads are in backoff, the write is refused without even
        // attempting one.
        let error = state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "manual_override",
                None,
            )
            .await
            .expect_err("local writes stay gated while reads are backing off");

        assert!(error.to_string().contains("failure backoff"));
        assert_eq!(writer.smoke_calls(), 1);
        assert!(writer.write_speeds().is_empty());
    }

    /// The same failure on the scene path must not block control.
    #[tokio::test]
    async fn scene_control_mode_writes_through_a_failed_read() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let writer = FakeErvWriter::new(
            vec![Err(anyhow!("Check device key or version (Error 914)"))],
            vec![Ok(turbo_status())],
        )
        .with_scene_report();

        state
            .set_speed_with(
                &scene_config(),
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "manual_override",
                None,
            )
            .await
            .expect("a failed read must not block a scene write");

        assert_eq!(writer.write_speeds(), vec![ErvFanSpeed::Turbo]);
    }

    /// A failed fallback write is hidden under a Scene write report, so
    /// without draining it the state layer records a scene failure and the
    /// local-key counters never move -- `/status` would report the local key
    /// as fine while the fallback path is known bad.
    #[tokio::test]
    async fn failed_fallback_write_reaches_the_local_key_counters() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::failing(1));
        let local = Arc::new(FakeErvWriter::new(
            Vec::new(),
            vec![Err(anyhow!("Check device key or version (Error 914)"))],
        ));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            Some(local.clone()),
        );
        let config = ErvConfig {
            local_readback_enabled: false,
            ..scene_config()
        };

        state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "manual_override",
                None,
            )
            .await
            .expect_err("both transports failed");

        let mut app_status = Status::read_only_default(&app_config(config));
        state.overlay_status(&mut app_status);
        assert_eq!(
            app_status.erv.control.consecutive_local_key_errors, 1,
            "a failed local fallback never reached the local-key counters"
        );
    }

    /// A fallback write that lands proves local works, so the failure count
    /// behind the backoff curve has to reset. Otherwise the next intermittent
    /// failure resumes the exponential climb and can disable the fallback for
    /// an hour despite successful local writes in between.
    #[tokio::test]
    async fn successful_fallback_write_resets_local_health() {
        let cloud = Arc::new(FakeCloud::failing(3));
        let local = Arc::new(FakeErvWriter::new(
            Vec::new(),
            vec![Ok(turbo_status()), Ok(turbo_status())],
        ));
        let writer = split_writer(
            cloud.clone(),
            // One failed read to put a failure on the books, then successes.
            Arc::new(FakeErvReader::new(vec![Err(anyhow!(
                "Connection reset by peer"
            ))])),
            Some(local.clone()),
        );
        let config = ErvConfig {
            local_readback_enabled: false,
            ..scene_config()
        };

        // Seed a local failure, then let the backoff lapse.
        assert!(
            writer
                .set_speed(&config, ErvFanSpeed::Turbo, false)
                .await
                .is_ok()
        );
        assert_eq!(local.write_speeds().len(), 1);
        writer.clear_local_backoff_for_test();

        // A second fallback write after the reset must still be attempted.
        assert!(
            writer
                .set_speed(&config, ErvFanSpeed::Turbo, false)
                .await
                .is_ok()
        );
        assert_eq!(local.write_speeds().len(), 2);
        assert_eq!(
            writer.local_failure_count_for_test(),
            0,
            "a landed local write left the old failure count in place"
        );
    }

    /// A failed fallback write means local is not usable either. Leaving it
    /// marked healthy would fire another command at a known-bad local key on
    /// the next cloud failure, which is the command pattern that causes the
    /// Err 914 lockout in the first place.
    #[tokio::test]
    async fn failed_fallback_write_marks_the_local_path_unhealthy() {
        let cloud = Arc::new(FakeCloud::failing(2));
        let local = Arc::new(FakeErvWriter::new(
            Vec::new(),
            vec![Err(anyhow!("Check device key or version (Error 914)"))],
        ));
        let writer = split_writer(
            cloud.clone(),
            Arc::new(FakeErvReader::new(Vec::new())),
            Some(local.clone()),
        );
        let config = scene_config();

        writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect_err("both transports failed");
        assert_eq!(local.write_speeds().len(), 1);

        // Second cloud failure must not reach for local again.
        writer
            .set_speed(&config, ErvFanSpeed::Turbo, false)
            .await
            .expect_err("both transports still failing");
        assert_eq!(
            local.write_speeds().len(),
            1,
            "local was retried while still in failure backoff"
        );
    }

    /// Writer selection follows `control_mode`. An incomplete scene set fails
    /// per write with a missing-scene diagnostic; it must never quietly demote
    /// the deployment to the local transport.
    #[tokio::test]
    async fn incomplete_scene_config_does_not_silently_select_the_local_writer() {
        let config = app_config(ErvConfig {
            turbo_scene_id: None,
            status_timeout_seconds: 1,
            ..scene_config()
        });
        assert!(
            !config.erv.scene_configured(),
            "the scene set is deliberately incomplete"
        );
        assert!(
            config.erv.local_tuya_configured(),
            "local credentials are present, as they are pre-upgrade"
        );

        let error = build_erv_writer(&config)
            .set_speed(&config.erv, ErvFanSpeed::Turbo, false)
            .await
            .expect_err("an incomplete scene set must fail, not fall back to local");

        assert!(
            error.downcast_ref::<SceneConfigError>().is_some(),
            "expected a missing-scene diagnostic, got: {error:#}"
        );
    }

    /// `control_mode` picks the writer in both directions. The two writers are
    /// told apart by how they refuse a read with no local credentials, which
    /// needs no network.
    #[tokio::test]
    async fn control_mode_selects_the_writer_in_both_directions() {
        let without_local_credentials = ErvConfig {
            ip: String::new(),
            device_id: String::new(),
            local_key: String::new(),
            ..scene_config()
        };

        let local_mode = app_config(ErvConfig {
            control_mode: ErvControlMode::Local,
            ..without_local_credentials.clone()
        });
        let error = build_erv_writer(&local_mode)
            .smoke_status(&local_mode.erv)
            .await
            .expect_err("no local credentials");
        assert!(
            error
                .to_string()
                .contains("local Tuya config is incomplete"),
            "expected the local writer, got: {error:#}"
        );

        let scene_mode = app_config(ErvConfig {
            control_mode: ErvControlMode::Scene,
            ..without_local_credentials
        });
        let error = build_erv_writer(&scene_mode)
            .smoke_status(&scene_mode.erv)
            .await
            .expect_err("no local credentials");
        assert!(
            error
                .to_string()
                .contains("local readback is not configured"),
            "expected the split writer, got: {error:#}"
        );
    }

    /// A cloud failure backs off the write path and says nothing about the
    /// local key, which is a different subsystem entirely.
    #[tokio::test]
    async fn scene_failure_backs_off_writes_without_blaming_the_local_key() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let database_path = temp_dir.path().join("office_climate.db");
        db::migrate_database(&database_path).expect("migration");
        let state = ErvState::new(database_path);
        let cloud = Arc::new(FakeCloud::failing(1));
        let writer = split_writer(cloud, Arc::new(FakeErvReader::new(Vec::new())), None);
        let config = ErvConfig {
            local_readback_enabled: false,
            ..scene_config()
        };

        state
            .set_speed_with(
                &config,
                &writer,
                ErvFanSpeed::Turbo,
                false,
                "away_refresh",
                None,
            )
            .await
            .expect_err("scene trigger failed with no fallback");

        let now = unix_timestamp_now();
        assert!(!state.write_retry_allowed(now));
        assert!(state.read_retry_allowed(now));

        let mut app_status = Status::read_only_default(&app_config(config));
        state.overlay_status(&mut app_status);
        assert!(!app_status.erv.control.local_key_invalid);
        assert_eq!(app_status.erv.control.consecutive_local_key_errors, 0);
        assert!(
            app_status
                .erv
                .control
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("Scene trigger failed:"))
        );
    }

    /// Without a home id no scene can be triggered at all, so a complete
    /// matrix must not be mistaken for a working transport.
    #[test]
    fn scene_config_check_requires_the_home_id() {
        let complete = app_config(scene_config());
        assert_eq!(check_erv_scene_config(&complete).expect("configured"), 4);

        let no_home_id = app_config(ErvConfig {
            smart_life_home_id: None,
            ..scene_config()
        });
        let error = check_erv_scene_config(&no_home_id).expect_err("no home id");
        assert!(error.to_string().contains("home id"));

        let no_turbo = app_config(ErvConfig {
            turbo_scene_id: None,
            ..scene_config()
        });
        let error = check_erv_scene_config(&no_turbo).expect_err("incomplete matrix");
        assert!(error.to_string().contains("turbo"));
    }

    /// `smoke_erv_scene` layers live checks on top of `check_erv_scene_config`
    /// and must not reach the network -- construct a client, read the auth
    /// cache -- when the configuration gate itself already fails. Live
    /// credentials cannot be exercised in a unit test; this covers the
    /// routing that decides whether the live path is even attempted.
    #[tokio::test]
    async fn smoke_erv_scene_rejects_incomplete_config_before_touching_the_network() {
        let no_home_id = app_config(ErvConfig {
            smart_life_home_id: None,
            ..scene_config()
        });
        let error = smoke_erv_scene(&no_home_id)
            .await
            .expect_err("no home id");
        assert!(error.to_string().contains("home id"));

        let no_turbo = app_config(ErvConfig {
            turbo_scene_id: None,
            ..scene_config()
        });
        let error = smoke_erv_scene(&no_turbo).await.expect_err("incomplete matrix");
        assert!(error.to_string().contains("turbo"));

        let local_mode = app_config(ErvConfig {
            control_mode: ErvControlMode::Local,
            ..scene_config()
        });
        let error = smoke_erv_scene(&local_mode)
            .await
            .expect_err("scene control not selected");
        assert!(error.to_string().contains("not the selected transport"));
    }

    /// Every scene id the configuration sets is checked for existence, not
    /// just the ones the current pressure mode requires -- a dormant
    /// negative-pressure id that has gone stale should still be caught before
    /// the mode is re-armed, not discovered for the first time after.
    #[test]
    fn configured_scene_ids_includes_dormant_negative_pressure_and_skips_blank_entries() {
        let config = ErvConfig {
            quiet_negative_pressure_scene_id: Some("  ".to_string()),
            medium_negative_pressure_scene_id: Some("medium-np".to_string()),
            turbo_negative_pressure_scene_id: None,
            ..scene_config()
        };
        assert!(!negative_pressure_armed(&app_config(config.clone())));

        let ids = configured_scene_ids(&config);
        assert_eq!(
            ids,
            vec![
                ("off", "off-scene"),
                ("quiet", "quiet-scene"),
                ("medium", "medium-scene"),
                ("turbo", "turbo-scene"),
                ("medium_negative_pressure", "medium-np"),
            ],
            "blank ids are skipped and dormant negative-pressure ids are still included"
        );
    }

    /// While negative pressure is armed, those are the only scenes automation
    /// uses, so treating them as optional would let validation pass a
    /// configuration in which every non-off command fails.
    #[test]
    fn armed_negative_pressure_makes_those_scenes_required() {
        let normal = app_config(scene_config());
        assert!(
            required_scene_presets(&normal)
                .iter()
                .all(|(label, _)| !label.contains("negative_pressure")),
            "negative-pressure scenes are not required while the mode is dormant"
        );

        let mut armed = app_config(scene_config());
        armed.thresholds.post_renovation_enabled = true;
        armed.thresholds.post_renovation_negative_pressure = true;
        armed.thresholds.post_renovation_expires_at = Some("2099-01-01T00:00:00Z".to_string());
        assert!(negative_pressure_armed(&armed));

        let missing = required_scene_presets(&armed)
            .into_iter()
            .filter(|(_, scene_id)| scene_id.is_none())
            .map(|(label, _)| label)
            .collect::<Vec<_>>();
        assert_eq!(
            missing,
            vec![
                "quiet_negative_pressure",
                "medium_negative_pressure",
                "turbo_negative_pressure"
            ],
            "an armed negative-pressure deployment must require its scenes"
        );
    }

    #[test]
    fn scene_ids_cover_the_full_speed_by_pressure_matrix() {
        let config = ErvConfig {
            quiet_negative_pressure_scene_id: Some("quiet-np".to_string()),
            medium_negative_pressure_scene_id: Some("medium-np".to_string()),
            turbo_negative_pressure_scene_id: Some("turbo-np".to_string()),
            ..scene_config()
        };

        for (speed, negative_pressure, expected) in [
            (ErvFanSpeed::Off, false, "off-scene"),
            (ErvFanSpeed::Off, true, "off-scene"),
            (ErvFanSpeed::Quiet, false, "quiet-scene"),
            (ErvFanSpeed::Medium, false, "medium-scene"),
            (ErvFanSpeed::Turbo, false, "turbo-scene"),
            (ErvFanSpeed::Quiet, true, "quiet-np"),
            (ErvFanSpeed::Medium, true, "medium-np"),
            (ErvFanSpeed::Turbo, true, "turbo-np"),
        ] {
            assert_eq!(
                scene_id_for(&config, speed, negative_pressure),
                Ok(expected),
                "wrong scene for {speed:?} negative_pressure={negative_pressure}"
            );
        }
    }
}
