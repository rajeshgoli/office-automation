use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub orchestrator: OrchestratorConfig,
    pub presence: PresenceConfig,
    pub qingping: QingpingConfig,
    pub yolink: YoLinkConfig,
    pub artifacts: ArtifactConfig,
    pub cloudflare_access: CloudflareAccessConfig,
    pub erv: ErvConfig,
    pub mitsubishi: MitsubishiConfig,
    pub thresholds: ThresholdsConfig,
    pub telemetry: TelemetryConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PresenceConfig {
    pub enabled: bool,
    pub poll_interval_seconds: u64,
    pub command_timeout_seconds: u64,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            poll_interval_seconds: 5,
            command_timeout_seconds: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct TelemetryConfig {
    pub repos: Vec<PathBuf>,
    pub tool_usage_db: Option<PathBuf>,
    pub session_tool_usage_db: Option<PathBuf>,
    pub codex_events_db: Option<PathBuf>,
    pub session_manager_sessions: Option<PathBuf>,
    pub telemetry_db: Option<PathBuf>,
    pub engram_db: Option<PathBuf>,
    pub engram_registry: Option<PathBuf>,
    pub days: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct OrchestratorConfig {
    pub host: String,
    pub port: u16,
    pub admin_emails: Vec<String>,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
    pub google_oauth: Option<GoogleOAuthConfig>,
    pub controller_ipc_token: Option<String>,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            admin_emails: Vec::new(),
            auth_username: None,
            auth_password: None,
            google_oauth: None,
            controller_ipc_token: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub allowed_emails: Vec<String>,
    pub token_expiry_days: i64,
    pub device_flow_enabled: bool,
    pub jwt_secret: Option<String>,
    pub trusted_networks: Vec<String>,
}

impl Default for GoogleOAuthConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            allowed_emails: Vec::new(),
            token_expiry_days: 7,
            device_flow_enabled: true,
            jwt_secret: None,
            trusted_networks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct QingpingConfig {
    pub mqtt_broker: String,
    pub mqtt_port: u16,
    pub mqtt_username: Option<String>,
    pub mqtt_password: Option<String>,
    pub device_mac: Option<String>,
    pub report_interval: u64,
}

impl Default for QingpingConfig {
    fn default() -> Self {
        Self {
            mqtt_broker: "127.0.0.1".to_string(),
            mqtt_port: 1883,
            mqtt_username: None,
            mqtt_password: None,
            device_mac: None,
            report_interval: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct ArtifactConfig {
    pub office_climate_signing_cert_sha256: Option<String>,
    pub apksigner_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CloudflareAccessConfig {
    pub account_id: Option<String>,
    pub app_id: Option<String>,
    pub jwt_audience: Option<String>,
    pub device_policy_id: Option<String>,
    pub api_token: Option<String>,
    pub api_base_url: String,
}

impl Default for CloudflareAccessConfig {
    fn default() -> Self {
        Self {
            account_id: None,
            app_id: None,
            jwt_audience: None,
            device_policy_id: None,
            api_token: None,
            api_base_url: "https://api.cloudflare.com/client/v4".to_string(),
        }
    }
}

impl CloudflareAccessConfig {
    pub fn device_policy_sync_configured(&self) -> bool {
        self.account_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self
                .device_policy_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .api_token
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    pub fn access_jwt_audience(&self) -> Option<&str> {
        self.jwt_audience
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct YoLinkConfig {
    pub uaid: String,
    pub secret_key: String,
    pub http_url: String,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub reconnect_delay_seconds: u64,
}

impl YoLinkConfig {
    pub fn is_configured(&self) -> bool {
        !self.uaid.trim().is_empty() && !self.secret_key.trim().is_empty()
    }
}

impl Default for YoLinkConfig {
    fn default() -> Self {
        Self {
            uaid: String::new(),
            secret_key: String::new(),
            http_url: "https://api.yosmart.com".to_string(),
            mqtt_host: "api.yosmart.com".to_string(),
            mqtt_port: 8003,
            reconnect_delay_seconds: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MitsubishiConfig {
    #[serde(rename = "type")]
    pub device_type: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub device_serial: Option<String>,
    pub ip: Option<String>,
    pub active_control_enabled: bool,
    pub base_url: String,
    pub poll_interval_seconds: u64,
    pub status_timeout_seconds: u64,
}

impl MitsubishiConfig {
    pub fn is_configured(&self) -> bool {
        self.device_type == "kumo"
            && self
                .username
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .password
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .device_serial
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }
}

impl Default for MitsubishiConfig {
    fn default() -> Self {
        Self {
            device_type: "kumo".to_string(),
            username: None,
            password: None,
            device_serial: None,
            ip: None,
            active_control_enabled: false,
            base_url: "https://app-prod.kumocloud.com".to_string(),
            poll_interval_seconds: 600,
            status_timeout_seconds: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ErvConfig {
    #[serde(rename = "type")]
    pub device_type: String,
    pub ip: String,
    pub device_id: String,
    pub local_key: String,
    pub active_control_enabled: bool,
    pub version: String,
    pub port: u16,
    pub status_timeout_seconds: u64,
    pub verify_delay_seconds: u64,
    pub poll_interval_seconds: u64,
}

impl ErvConfig {
    pub fn is_configured(&self) -> bool {
        self.device_type == "tuya"
            && !self.ip.trim().is_empty()
            && !self.device_id.trim().is_empty()
            && !self.local_key.trim().is_empty()
    }
}

impl Default for ErvConfig {
    fn default() -> Self {
        Self {
            device_type: "tuya".to_string(),
            ip: String::new(),
            device_id: String::new(),
            local_key: String::new(),
            active_control_enabled: false,
            version: "3.4".to_string(),
            port: 6668,
            status_timeout_seconds: 5,
            verify_delay_seconds: 1,
            poll_interval_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThresholdsConfig {
    pub motion_timeout_seconds: u64,
    pub departure_verification_seconds: u64,
    pub door_open_threshold_minutes: u64,
    pub door_open_away_timeout_minutes: u64,
    pub co2_critical_ppm: i64,
    pub co2_critical_hysteresis_ppm: i64,
    pub co2_refresh_target_ppm: i64,
    pub co2_plateau_enabled: bool,
    pub co2_plateau_rate_threshold: f64,
    pub co2_plateau_window_minutes: u64,
    pub co2_plateau_min_co2: i64,
    pub co2_plateau_release_delta_ppm: i64,
    pub co2_history_size: usize,
    pub co2_adaptive_speed_enabled: bool,
    pub co2_rate_turbo_threshold: f64,
    pub co2_rate_medium_threshold: f64,
    pub co2_rate_quiet_threshold: f64,
    pub co2_turbo_duration_minutes: u64,
    pub min_away_seconds_before_erv: u64,
    pub erv_min_dwell_seconds: u64,
    pub tvoc_away_enabled: bool,
    pub tvoc_away_threshold: i64,
    pub tvoc_away_target: i64,
    pub tvoc_away_history_size: usize,
    pub tvoc_plateau_rate_threshold: f64,
    pub tvoc_rate_turbo_threshold: f64,
    pub tvoc_rate_medium_threshold: f64,
    pub tvoc_rate_quiet_threshold: f64,
    pub hvac_min_temp_f: i64,
    pub hvac_critical_temp_f: i64,
    pub hvac_heat_on_temp_f: i64,
    pub hvac_heat_off_temp_f: i64,
    pub hvac_cool_setpoint_f: i64,
    pub hvac_cool_off_temp_f: i64,
    pub hvac_cool_on_temp_f: i64,
    pub expected_occupancy_start: String,
    pub expected_occupancy_end: String,
    pub away_stale_flush_enabled: bool,
    pub away_stale_flush_interval_hours: u64,
    pub away_stale_flush_duration_minutes: u64,
    pub away_stale_flush_speed: String,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            motion_timeout_seconds: 60,
            departure_verification_seconds: 10,
            door_open_threshold_minutes: 5,
            door_open_away_timeout_minutes: 5,
            co2_critical_ppm: 2000,
            co2_critical_hysteresis_ppm: 200,
            co2_refresh_target_ppm: 500,
            co2_plateau_enabled: true,
            co2_plateau_rate_threshold: 0.5,
            co2_plateau_window_minutes: 10,
            co2_plateau_min_co2: 600,
            co2_plateau_release_delta_ppm: 100,
            co2_history_size: 40,
            co2_adaptive_speed_enabled: true,
            co2_rate_turbo_threshold: 8.0,
            co2_rate_medium_threshold: 2.0,
            co2_rate_quiet_threshold: 0.5,
            co2_turbo_duration_minutes: 30,
            min_away_seconds_before_erv: 60,
            erv_min_dwell_seconds: 180,
            tvoc_away_enabled: true,
            tvoc_away_threshold: 200,
            tvoc_away_target: 40,
            tvoc_away_history_size: 40,
            tvoc_plateau_rate_threshold: 0.3,
            tvoc_rate_turbo_threshold: 5.0,
            tvoc_rate_medium_threshold: 1.5,
            tvoc_rate_quiet_threshold: 0.3,
            hvac_min_temp_f: 68,
            hvac_critical_temp_f: 55,
            hvac_heat_on_temp_f: 71,
            hvac_heat_off_temp_f: 75,
            hvac_cool_setpoint_f: 78,
            hvac_cool_off_temp_f: 78,
            hvac_cool_on_temp_f: 81,
            expected_occupancy_start: "07:00".to_string(),
            expected_occupancy_end: "22:00".to_string(),
            away_stale_flush_enabled: true,
            away_stale_flush_interval_hours: 8,
            away_stale_flush_duration_minutes: 30,
            away_stale_flush_speed: "medium".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub frontend_dist: PathBuf,
    pub artifacts_dir: PathBuf,
    pub legacy_apk_path: PathBuf,
    pub base_url: Option<String>,
    pub public_url: Option<String>,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub telemetry_db_path: PathBuf,
    pub session_tool_usage_db_path: PathBuf,
    pub tool_usage_db_path: PathBuf,
    pub engram_db_path: PathBuf,
    pub engram_registry_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct FileConfig {
    orchestrator: OrchestratorConfig,
    presence: PresenceConfig,
    qingping: QingpingConfig,
    yolink: YoLinkConfig,
    artifacts: ArtifactConfig,
    cloudflare_access: CloudflareAccessConfig,
    erv: ErvConfig,
    mitsubishi: MitsubishiConfig,
    thresholds: ThresholdsConfig,
    telemetry: TelemetryConfig,
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_env(path, |key| env::var(key).ok())
    }

    pub fn load_with_env(
        path: impl AsRef<Path>,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let config_path = path.as_ref();
        let contents = fs::read_to_string(config_path)
            .with_context(|| format!("failed to read config {}", config_path.display()))?;
        let file_config: FileConfig = serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", config_path.display()))?;

        Self::from_file_config(config_path, file_config, env_lookup)
    }

    fn from_file_config(
        config_path: &Path,
        mut file_config: FileConfig,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let root = env_lookup("OFFICE_AUTOMATE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(env::current_dir().context("failed to determine current directory")?);

        let data_dir = env_lookup("OFFICE_AUTOMATE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("data"));
        let home_dir = env_lookup("HOME").map(PathBuf::from);

        if let Some(host) = env_lookup("OFFICE_AUTOMATE_MQTT_HOST") {
            file_config.qingping.mqtt_broker = host;
        }

        if let Some(port) = env_lookup("OFFICE_AUTOMATE_MQTT_PORT") {
            file_config.qingping.mqtt_port = port
                .parse()
                .with_context(|| format!("invalid OFFICE_AUTOMATE_MQTT_PORT value {port:?}"))?;
        }

        if let Some(username) = env_lookup("OFFICE_AUTOMATE_QINGPING_MQTT_USERNAME") {
            file_config.qingping.mqtt_username = Some(username);
        }

        if let Some(password) = env_lookup("OFFICE_AUTOMATE_QINGPING_MQTT_PASSWORD") {
            file_config.qingping.mqtt_password = Some(password);
        }

        if let Some(uaid) = env_lookup("OFFICE_AUTOMATE_YOLINK_UAID") {
            file_config.yolink.uaid = uaid;
        }

        if let Some(secret_key) = env_lookup("OFFICE_AUTOMATE_YOLINK_SECRET_KEY") {
            file_config.yolink.secret_key = secret_key;
        }

        if let Some(username) = env_lookup("OFFICE_AUTOMATE_KUMO_USERNAME") {
            file_config.mitsubishi.username = Some(username);
        }

        if let Some(password) = env_lookup("OFFICE_AUTOMATE_KUMO_PASSWORD") {
            file_config.mitsubishi.password = Some(password);
        }

        if let Some(serial) = env_lookup("OFFICE_AUTOMATE_KUMO_DEVICE_SERIAL") {
            file_config.mitsubishi.device_serial = Some(serial);
        }

        if let Some(base_url) = env_lookup("OFFICE_AUTOMATE_KUMO_BASE_URL") {
            file_config.mitsubishi.base_url = base_url;
        }

        if let Some(enabled) = env_lookup("OFFICE_AUTOMATE_KUMO_ACTIVE_CONTROL_ENABLED") {
            file_config.mitsubishi.active_control_enabled =
                parse_bool_env("OFFICE_AUTOMATE_KUMO_ACTIVE_CONTROL_ENABLED", &enabled)?;
        }

        if let Some(ip) = env_lookup("OFFICE_AUTOMATE_ERV_IP") {
            file_config.erv.ip = ip;
        }

        if let Some(device_id) = env_lookup("OFFICE_AUTOMATE_ERV_DEVICE_ID") {
            file_config.erv.device_id = device_id;
        }

        if let Some(local_key) = env_lookup("OFFICE_AUTOMATE_ERV_LOCAL_KEY") {
            file_config.erv.local_key = local_key;
        }

        if let Some(enabled) = env_lookup("OFFICE_AUTOMATE_ERV_ACTIVE_CONTROL_ENABLED") {
            file_config.erv.active_control_enabled =
                parse_bool_env("OFFICE_AUTOMATE_ERV_ACTIVE_CONTROL_ENABLED", &enabled)?;
        }

        if let Some(enabled) = env_lookup("OFFICE_AUTOMATE_PRESENCE_ENABLED") {
            file_config.presence.enabled =
                parse_bool_env("OFFICE_AUTOMATE_PRESENCE_ENABLED", &enabled)?;
        }

        if let Some(seconds) = env_lookup("OFFICE_AUTOMATE_PRESENCE_POLL_INTERVAL_SECONDS") {
            file_config.presence.poll_interval_seconds = seconds.parse().with_context(|| {
                format!("invalid OFFICE_AUTOMATE_PRESENCE_POLL_INTERVAL_SECONDS value {seconds:?}")
            })?;
        }

        if let Some(seconds) = env_lookup("OFFICE_AUTOMATE_PRESENCE_COMMAND_TIMEOUT_SECONDS") {
            file_config.presence.command_timeout_seconds = seconds.parse().with_context(|| {
                format!(
                    "invalid OFFICE_AUTOMATE_PRESENCE_COMMAND_TIMEOUT_SECONDS value {seconds:?}"
                )
            })?;
        }

        if let Some(token) = env_lookup("OFFICE_AUTOMATE_CONTROLLER_IPC_TOKEN") {
            file_config.orchestrator.controller_ipc_token = Some(token);
        }

        if let Some(cert_sha256) = env_lookup("OFFICE_AUTOMATE_ANDROID_SIGNING_CERT_SHA256") {
            file_config.artifacts.office_climate_signing_cert_sha256 = Some(cert_sha256);
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_APKSIGNER") {
            file_config.artifacts.apksigner_path = Some(PathBuf::from(path));
        }

        if let Some(account_id) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_ACCOUNT_ID") {
            file_config.cloudflare_access.account_id = Some(account_id);
        }

        if let Some(app_id) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_APP_ID") {
            file_config.cloudflare_access.app_id = Some(app_id);
        }

        if let Some(audience) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_JWT_AUDIENCE") {
            file_config.cloudflare_access.jwt_audience = Some(audience);
        }

        if let Some(policy_id) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_DEVICE_POLICY_ID") {
            file_config.cloudflare_access.device_policy_id = Some(policy_id);
        }

        if let Some(api_token) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_API_TOKEN") {
            file_config.cloudflare_access.api_token = Some(api_token);
        }

        if let Some(api_base_url) = env_lookup("OFFICE_AUTOMATE_CLOUDFLARE_API_BASE_URL") {
            file_config.cloudflare_access.api_base_url = api_base_url;
        }

        if let Some(admin_emails) = env_lookup("OFFICE_AUTOMATE_ADMIN_EMAILS") {
            file_config.orchestrator.admin_emails = admin_emails
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        if let Some(repos) = env_lookup("OFFICE_AUTOMATE_TELEMETRY_REPOS") {
            file_config.telemetry.repos = repos
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .collect();
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_TOOL_USAGE_DB") {
            file_config.telemetry.tool_usage_db = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_SESSION_TOOL_USAGE_DB") {
            file_config.telemetry.session_tool_usage_db = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_CODEX_EVENTS_DB") {
            file_config.telemetry.codex_events_db = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_SESSION_MANAGER_SESSIONS") {
            file_config.telemetry.session_manager_sessions = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_TELEMETRY_DB") {
            file_config.telemetry.telemetry_db = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_ENGRAM_DB") {
            file_config.telemetry.engram_db = Some(PathBuf::from(path));
        }

        if let Some(path) = env_lookup("OFFICE_AUTOMATE_ENGRAM_REGISTRY") {
            file_config.telemetry.engram_registry = Some(PathBuf::from(path));
        }

        if let Some(days) = env_lookup("OFFICE_AUTOMATE_TELEMETRY_DAYS") {
            file_config.telemetry.days = days.parse().with_context(|| {
                format!("invalid OFFICE_AUTOMATE_TELEMETRY_DAYS value {days:?}")
            })?;
        }
        file_config.telemetry.repos = file_config
            .telemetry
            .repos
            .into_iter()
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .collect();
        file_config.telemetry.codex_events_db = file_config
            .telemetry
            .codex_events_db
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()));
        file_config.telemetry.session_manager_sessions = file_config
            .telemetry
            .session_manager_sessions
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()));

        let telemetry_db_path = file_config
            .telemetry
            .telemetry_db
            .clone()
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .unwrap_or_else(|| data_dir.join("telemetry.db"));
        let default_session_tool_usage_db_path = home_dir
            .as_deref()
            .map(|home| {
                home.join(".local")
                    .join("share")
                    .join("claude-sessions")
                    .join("tool_usage.db")
            })
            .unwrap_or_else(|| data_dir.join("tool_usage.db"));
        let session_tool_usage_db_path = file_config
            .telemetry
            .session_tool_usage_db
            .clone()
            .or_else(|| file_config.telemetry.tool_usage_db.clone())
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .unwrap_or(default_session_tool_usage_db_path);
        let tool_usage_db_path = file_config
            .telemetry
            .tool_usage_db
            .clone()
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .unwrap_or_else(|| data_dir.join("tool_usage.db"));
        let engram_db_path = file_config
            .telemetry
            .engram_db
            .clone()
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .unwrap_or_else(|| data_dir.join("engram_state.db"));
        let engram_registry_path = file_config
            .telemetry
            .engram_registry
            .clone()
            .map(|path| expand_home_relative_path(path, home_dir.as_deref()))
            .unwrap_or_else(|| data_dir.join("engram_concept_registry.md"));

        let runtime = RuntimeConfig {
            frontend_dist: root.join("frontend").join("dist"),
            root,
            config_path: config_path.to_path_buf(),
            database_path: data_dir.join("office_climate.db"),
            artifacts_dir: data_dir.join("apps"),
            legacy_apk_path: data_dir.join("app-debug.apk"),
            data_dir,
            base_url: env_lookup("OFFICE_AUTOMATE_BASE_URL"),
            public_url: env_lookup("OFFICE_AUTOMATE_PUBLIC_URL"),
            mqtt_host: file_config.qingping.mqtt_broker.clone(),
            mqtt_port: file_config.qingping.mqtt_port,
            telemetry_db_path,
            session_tool_usage_db_path,
            tool_usage_db_path,
            engram_db_path,
            engram_registry_path,
        };

        Ok(Self {
            orchestrator: file_config.orchestrator,
            presence: file_config.presence,
            qingping: file_config.qingping,
            yolink: file_config.yolink,
            artifacts: file_config.artifacts,
            cloudflare_access: file_config.cloudflare_access,
            erv: file_config.erv,
            mitsubishi: file_config.mitsubishi,
            thresholds: file_config.thresholds,
            telemetry: file_config.telemetry,
            runtime,
        })
    }
}

fn parse_bool_env(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => anyhow::bail!("invalid {name} value {value:?}; expected true/false"),
    }
}

fn expand_home_relative_path(path: PathBuf, home_dir: Option<&Path>) -> PathBuf {
    let Some(home_dir) = home_dir else {
        return path;
    };
    let Some(raw_path) = path.to_str() else {
        return path;
    };
    if raw_path == "~" {
        return home_dir.to_path_buf();
    }
    raw_path
        .strip_prefix("~/")
        .map(|suffix| home_dir.join(suffix))
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_default_binds_http_to_loopback() {
        let config = OrchestratorConfig::default();

        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8080);
        assert!(config.admin_emails.is_empty());
    }

    #[test]
    fn loads_config_and_applies_environment_overrides() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config_path = temp_dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
orchestrator:
  host: "127.0.0.1"
  port: 9001
presence:
  poll_interval_seconds: 7
telemetry:
  repos:
    - "/yaml/repo"
qingping:
  mqtt_broker: "legacy-broker"
  mqtt_port: 1883
  device_mac: "AA:BB:CC:DD:EE:FF"
  report_interval: 30
yolink:
  uaid: "yaml-uaid"
  secret_key: "yaml-secret"
mitsubishi:
  username: "yaml-kumo-user"
  password: "yaml-kumo-pass"
  device_serial: "yaml-kumo-serial"
erv:
  type: "tuya"
  ip: "192.0.2.10"
  device_id: "yaml-erv-device"
  local_key: "yaml-erv-key"
thresholds:
  hvac_heat_on_temp_f: 70
  hvac_heat_off_temp_f: 74
  hvac_cool_setpoint_f: 77
  hvac_cool_off_temp_f: 79
  hvac_cool_on_temp_f: 82
  expected_occupancy_start: "08:30"
  expected_occupancy_end: "18:45"
"#,
        )
        .expect("write config");

        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "OFFICE_AUTOMATE_ROOT" => Some(temp_dir.path().join("root").display().to_string()),
            "OFFICE_AUTOMATE_DATA_DIR" => Some(temp_dir.path().join("db").display().to_string()),
            "OFFICE_AUTOMATE_MQTT_HOST" => Some("rust-broker".to_string()),
            "OFFICE_AUTOMATE_MQTT_PORT" => Some("2883".to_string()),
            "OFFICE_AUTOMATE_YOLINK_UAID" => Some("env-uaid".to_string()),
            "OFFICE_AUTOMATE_YOLINK_SECRET_KEY" => Some("env-secret".to_string()),
            "OFFICE_AUTOMATE_KUMO_USERNAME" => Some("env-kumo-user".to_string()),
            "OFFICE_AUTOMATE_KUMO_PASSWORD" => Some("env-kumo-pass".to_string()),
            "OFFICE_AUTOMATE_KUMO_DEVICE_SERIAL" => Some("env-kumo-serial".to_string()),
            "OFFICE_AUTOMATE_KUMO_BASE_URL" => Some("https://kumo.example.test".to_string()),
            "OFFICE_AUTOMATE_KUMO_ACTIVE_CONTROL_ENABLED" => Some("true".to_string()),
            "OFFICE_AUTOMATE_ERV_IP" => Some("192.0.2.11".to_string()),
            "OFFICE_AUTOMATE_ERV_DEVICE_ID" => Some("env-erv-device".to_string()),
            "OFFICE_AUTOMATE_ERV_LOCAL_KEY" => Some("env-erv-key".to_string()),
            "OFFICE_AUTOMATE_ERV_ACTIVE_CONTROL_ENABLED" => Some("true".to_string()),
            "OFFICE_AUTOMATE_PRESENCE_ENABLED" => Some("true".to_string()),
            "OFFICE_AUTOMATE_PRESENCE_COMMAND_TIMEOUT_SECONDS" => Some("3".to_string()),
            "OFFICE_AUTOMATE_TELEMETRY_REPOS" => Some("/env/repo-a,/env/repo-b".to_string()),
            "OFFICE_AUTOMATE_TOOL_USAGE_DB" => Some(
                temp_dir
                    .path()
                    .join("tool_usage.sqlite")
                    .display()
                    .to_string(),
            ),
            "OFFICE_AUTOMATE_SESSION_TOOL_USAGE_DB" => Some(
                temp_dir
                    .path()
                    .join("session_tool_usage.sqlite")
                    .display()
                    .to_string(),
            ),
            "OFFICE_AUTOMATE_CODEX_EVENTS_DB" => Some(
                temp_dir
                    .path()
                    .join("codex_events.sqlite")
                    .display()
                    .to_string(),
            ),
            "OFFICE_AUTOMATE_SESSION_MANAGER_SESSIONS" => {
                Some(temp_dir.path().join("sessions.json").display().to_string())
            }
            "OFFICE_AUTOMATE_TELEMETRY_DB" => Some(
                temp_dir
                    .path()
                    .join("telemetry.sqlite")
                    .display()
                    .to_string(),
            ),
            "OFFICE_AUTOMATE_ENGRAM_DB" => {
                Some(temp_dir.path().join("engram.sqlite").display().to_string())
            }
            "OFFICE_AUTOMATE_ENGRAM_REGISTRY" => {
                Some(temp_dir.path().join("registry.md").display().to_string())
            }
            "OFFICE_AUTOMATE_TELEMETRY_DAYS" => Some("14".to_string()),
            "OFFICE_AUTOMATE_PUBLIC_URL" => Some("https://office.example.com".to_string()),
            "OFFICE_AUTOMATE_CONTROLLER_IPC_TOKEN" => Some("edge-ipc-token".to_string()),
            "OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_JWT_AUDIENCE" => Some("access-aud".to_string()),
            "OFFICE_AUTOMATE_ADMIN_EMAILS" => {
                Some("rajesh@example.com,ops@example.com".to_string())
            }
            _ => None,
        })
        .expect("load config");

        assert_eq!(config.orchestrator.host, "127.0.0.1");
        assert_eq!(config.orchestrator.port, 9001);
        assert!(config.presence.enabled);
        assert_eq!(config.presence.poll_interval_seconds, 7);
        assert_eq!(config.presence.command_timeout_seconds, 3);
        assert_eq!(
            config.telemetry.repos,
            vec![PathBuf::from("/env/repo-a"), PathBuf::from("/env/repo-b")]
        );
        assert_eq!(config.telemetry.days, 14);
        assert_eq!(config.qingping.mqtt_broker, "rust-broker");
        assert_eq!(config.qingping.mqtt_port, 2883);
        assert_eq!(config.yolink.uaid, "env-uaid");
        assert_eq!(config.yolink.secret_key, "env-secret");
        assert_eq!(config.yolink.http_url, "https://api.yosmart.com");
        assert_eq!(config.yolink.mqtt_host, "api.yosmart.com");
        assert_eq!(config.yolink.mqtt_port, 8003);
        assert_eq!(config.yolink.reconnect_delay_seconds, 5);
        assert!(config.yolink.is_configured());
        assert_eq!(config.mitsubishi.device_type, "kumo");
        assert_eq!(config.mitsubishi.username.as_deref(), Some("env-kumo-user"));
        assert_eq!(config.mitsubishi.password.as_deref(), Some("env-kumo-pass"));
        assert_eq!(
            config.mitsubishi.device_serial.as_deref(),
            Some("env-kumo-serial")
        );
        assert_eq!(config.mitsubishi.base_url, "https://kumo.example.test");
        assert!(config.mitsubishi.active_control_enabled);
        assert_eq!(config.mitsubishi.poll_interval_seconds, 600);
        assert!(config.mitsubishi.is_configured());
        assert_eq!(config.erv.device_type, "tuya");
        assert_eq!(config.erv.ip, "192.0.2.11");
        assert_eq!(config.erv.device_id, "env-erv-device");
        assert_eq!(config.erv.local_key, "env-erv-key");
        assert!(config.erv.active_control_enabled);
        assert_eq!(config.erv.version, "3.4");
        assert_eq!(config.erv.port, 6668);
        assert!(config.erv.is_configured());
        assert_eq!(config.runtime.mqtt_host, "rust-broker");
        assert_eq!(config.runtime.mqtt_port, 2883);
        assert_eq!(config.runtime.data_dir, temp_dir.path().join("db"));
        assert_eq!(
            config.runtime.database_path,
            temp_dir.path().join("db").join("office_climate.db")
        );
        assert_eq!(
            config.runtime.frontend_dist,
            temp_dir.path().join("root").join("frontend").join("dist")
        );
        assert_eq!(
            config.runtime.artifacts_dir,
            temp_dir.path().join("db").join("apps")
        );
        assert_eq!(
            config.runtime.tool_usage_db_path,
            temp_dir.path().join("tool_usage.sqlite")
        );
        assert_eq!(
            config.runtime.session_tool_usage_db_path,
            temp_dir.path().join("session_tool_usage.sqlite")
        );
        assert_eq!(
            config.telemetry.codex_events_db,
            Some(temp_dir.path().join("codex_events.sqlite"))
        );
        assert_eq!(
            config.telemetry.session_manager_sessions,
            Some(temp_dir.path().join("sessions.json"))
        );
        assert_eq!(
            config.runtime.telemetry_db_path,
            temp_dir.path().join("telemetry.sqlite")
        );
        assert_eq!(
            config.runtime.engram_db_path,
            temp_dir.path().join("engram.sqlite")
        );
        assert_eq!(
            config.runtime.engram_registry_path,
            temp_dir.path().join("registry.md")
        );
        assert_eq!(
            config.runtime.legacy_apk_path,
            temp_dir.path().join("db").join("app-debug.apk")
        );
        assert_eq!(
            config.runtime.public_url.as_deref(),
            Some("https://office.example.com")
        );
        assert_eq!(
            config.orchestrator.controller_ipc_token.as_deref(),
            Some("edge-ipc-token")
        );
        assert_eq!(
            config.cloudflare_access.access_jwt_audience(),
            Some("access-aud")
        );
        assert_eq!(
            config.orchestrator.admin_emails,
            vec!["rajesh@example.com", "ops@example.com"]
        );
        assert_eq!(config.thresholds.hvac_heat_on_temp_f, 70);
        assert_eq!(config.thresholds.hvac_cool_setpoint_f, 77);
        assert_eq!(config.thresholds.hvac_cool_on_temp_f, 82);
        assert_eq!(config.thresholds.expected_occupancy_start, "08:30");
        assert_eq!(config.thresholds.expected_occupancy_end, "18:45");
    }

    #[test]
    fn cloudflare_access_jwt_audience_ignores_blank_values() {
        let config = CloudflareAccessConfig {
            jwt_audience: Some("  ".to_string()),
            ..CloudflareAccessConfig::default()
        };

        assert_eq!(config.access_jwt_audience(), None);

        let config = CloudflareAccessConfig {
            jwt_audience: Some(" aud-tag ".to_string()),
            ..CloudflareAccessConfig::default()
        };

        assert_eq!(config.access_jwt_audience(), Some("aud-tag"));
    }

    #[test]
    fn expands_home_relative_telemetry_repo_paths() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let home_dir = temp_dir.path().join("home");
        let config_path = temp_dir.path().join("config.yaml");
        fs::write(
            &config_path,
            r#"
telemetry:
  repos:
    - "~/Desktop/automation/office-automate"
  codex_events_db: "~/.local/share/claude-sessions/codex_events.db"
  session_manager_sessions: "~/.local/share/claude-sessions/sessions.json"
"#,
        )
        .expect("write config");

        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "HOME" => Some(home_dir.display().to_string()),
            _ => None,
        })
        .expect("load config");

        assert_eq!(
            config.telemetry.repos,
            vec![home_dir.join("Desktop/automation/office-automate")]
        );
        assert_eq!(
            config.runtime.session_tool_usage_db_path,
            home_dir
                .join(".local")
                .join("share")
                .join("claude-sessions")
                .join("tool_usage.db")
        );
        assert_eq!(
            config.telemetry.codex_events_db,
            Some(
                home_dir
                    .join(".local")
                    .join("share")
                    .join("claude-sessions")
                    .join("codex_events.db")
            )
        );
        assert_eq!(
            config.telemetry.session_manager_sessions,
            Some(
                home_dir
                    .join(".local")
                    .join("share")
                    .join("claude-sessions")
                    .join("sessions.json")
            )
        );
        assert_eq!(
            config.runtime.tool_usage_db_path,
            std::env::current_dir()
                .expect("current dir")
                .join("data")
                .join("tool_usage.db")
        );

        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "HOME" => Some(home_dir.display().to_string()),
            "OFFICE_AUTOMATE_TELEMETRY_REPOS" => Some("~/repo-a,/absolute/repo-b".to_string()),
            _ => None,
        })
        .expect("load config");

        assert_eq!(
            config.telemetry.repos,
            vec![home_dir.join("repo-a"), PathBuf::from("/absolute/repo-b")]
        );
    }

    #[test]
    fn explicit_tool_usage_db_applies_to_session_telemetry_unless_split() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let home_dir = temp_dir.path().join("home");
        let config_path = temp_dir.path().join("config.yaml");
        fs::write(&config_path, "telemetry: {}\n").expect("write config");

        let shared_tool_db = home_dir.join("shared-tool-usage.db");
        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "HOME" => Some(home_dir.display().to_string()),
            "OFFICE_AUTOMATE_TOOL_USAGE_DB" => Some("~/shared-tool-usage.db".to_string()),
            _ => None,
        })
        .expect("load config");

        assert_eq!(config.runtime.tool_usage_db_path, shared_tool_db);
        assert_eq!(config.runtime.session_tool_usage_db_path, shared_tool_db);

        let split_session_db = home_dir.join("claude-tool-usage.db");
        let config = AppConfig::load_with_env(&config_path, |key| match key {
            "HOME" => Some(home_dir.display().to_string()),
            "OFFICE_AUTOMATE_TOOL_USAGE_DB" => Some("~/shared-tool-usage.db".to_string()),
            "OFFICE_AUTOMATE_SESSION_TOOL_USAGE_DB" => Some("~/claude-tool-usage.db".to_string()),
            _ => None,
        })
        .expect("load config");

        assert_eq!(config.runtime.tool_usage_db_path, shared_tool_db);
        assert_eq!(config.runtime.session_tool_usage_db_path, split_session_db);
    }
}
