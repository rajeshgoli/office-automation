"""Configuration loader for Office Climate Automation."""

import yaml
from pathlib import Path
from dataclasses import dataclass
from typing import Optional


@dataclass
class YoLinkConfig:
    uaid: str
    secret_key: str

    @property
    def http_url(self) -> str:
        return "https://api.yosmart.com"

    @property
    def mqtt_host(self) -> str:
        return "api.yosmart.com"

    @property
    def mqtt_port(self) -> int:
        return 8003


@dataclass
class QingpingConfig:
    device_mac: str
    mqtt_broker: str = "127.0.0.1"
    mqtt_port: int = 1883
    report_interval: int = 60  # Seconds between sensor reports (min: 15)


@dataclass
class TuyaCloudConfig:
    access_id: str
    access_secret: str
    region: str = "us"


@dataclass
class ERVConfig:
    type: str  # "tuya" or "shelly"
    ip: str
    device_id: Optional[str] = None
    local_key: Optional[str] = None


@dataclass
class MitsubishiConfig:
    username: Optional[str] = None
    password: Optional[str] = None
    device_serial: Optional[str] = None
    type: str = "kumo"  # "kumo" or "esphome"
    ip: Optional[str] = None
    poll_interval_seconds: int = 600  # How often to poll device status (10 min default)


@dataclass
class ThresholdsConfig:
    co2_critical_ppm: int = 2000
    co2_critical_hysteresis_ppm: int = 200  # Turn off when CO2 < (critical - hysteresis)
    co2_refresh_target_ppm: int = 500
    tvoc_threshold_ppb: int = 250  # tVOC > this triggers ERV at MEDIUM (3/2)
    tvoc_hysteresis_ppb: int = 30  # Turn off MEDIUM when tVOC < (threshold - hysteresis)
    hvac_min_temp_f: int = 68  # Don't heat above this when away + ERV running
    hvac_critical_temp_f: int = 55  # Always heat below this (pipe freeze protection)
    expected_occupancy_start: str = "07:00"  # When to allow pre-conditioning
    expected_occupancy_end: str = "22:00"  # After this, no heating unless critical
    motion_timeout_seconds: int = 60
    mac_poll_interval_seconds: int = 5

    # Adaptive tVOC spike detection
    tvoc_spike_detection_enabled: bool = True
    tvoc_spike_baseline_delta: int = 45      # Points above baseline to detect spike
    tvoc_spike_min_trigger: int = 60         # Ignore very low readings
    tvoc_spike_min_peak: int = 90            # Only ventilate if peak > 90
    tvoc_spike_target: int = 40              # Clear to this baseline
    tvoc_spike_cooldown_hours: int = 2       # Hours between detections
    tvoc_spike_history_size: int = 15        # Readings in sliding window

    # CO2 plateau detection (AWAY mode optimization)
    co2_plateau_enabled: bool = True
    co2_plateau_rate_threshold: float = 0.5  # ppm/min - slower than this = plateau
    co2_plateau_window_minutes: int = 10     # sustained slow rate for this long
    co2_plateau_min_co2: int = 600           # don't declare plateau above this (safety, allows winter ~490ppm + margin)
    co2_history_size: int = 40               # CO2 readings in sliding window (20 min at 30s intervals)

    # Adaptive ERV speed control (AWAY mode)
    co2_adaptive_speed_enabled: bool = True
    co2_rate_turbo_threshold: float = 8.0    # > 8 ppm/min → TURBO (8/8)
    co2_rate_medium_threshold: float = 2.0   # 2-8 ppm/min → MEDIUM (3/2)
    co2_rate_quiet_threshold: float = 0.5    # 0.5-2 ppm/min → QUIET (1/1)
                                              # < 0.5 ppm/min for 10 min → OFF (plateau)
    co2_turbo_floor_ppm: int = 800           # Force TURBO above this, regardless of rate

    # tVOC AWAY mode ventilation (similar to CO2 adaptive control)
    # NOTE: tVOC is IGNORED when PRESENT - only triggers ventilation when AWAY
    tvoc_away_enabled: bool = True
    tvoc_away_threshold: int = 200           # tVOC > this in AWAY triggers purge
    tvoc_away_target: int = 40               # Stop when tVOC reaches baseline
    tvoc_away_history_size: int = 40         # tVOC readings in sliding window
    tvoc_plateau_rate_threshold: float = 0.3 # points/min - slower = plateau
    tvoc_rate_turbo_threshold: float = 5.0   # > 5 points/min → TURBO
    tvoc_rate_medium_threshold: float = 1.5  # 1.5-5 points/min → MEDIUM
    tvoc_rate_quiet_threshold: float = 0.3   # 0.3-1.5 points/min → QUIET


@dataclass
class GoogleOAuthConfig:
    client_id: str
    client_secret: str
    allowed_emails: list[str]
    token_expiry_days: int = 7
    device_flow_enabled: bool = True
    jwt_secret: Optional[str] = None
    trusted_networks: list[str] = None  # CIDR networks that skip auth (e.g., ["192.168.5.0/24"])

    def __post_init__(self):
        if self.trusted_networks is None:
            self.trusted_networks = []


@dataclass
class OrchestratorConfig:
    host: str = "0.0.0.0"
    port: int = 8080
    auth_username: Optional[str] = None  # Legacy: HTTP Basic Auth (deprecated)
    auth_password: Optional[str] = None  # Legacy: HTTP Basic Auth (deprecated)
    google_oauth: Optional['GoogleOAuthConfig'] = None  # Google OAuth (recommended)


@dataclass
class Config:
    yolink: YoLinkConfig
    qingping: QingpingConfig
    erv: ERVConfig
    mitsubishi: MitsubishiConfig
    thresholds: ThresholdsConfig
    orchestrator: OrchestratorConfig
    tuya_cloud: Optional[TuyaCloudConfig] = None


def load_config(path: str = "config.yaml") -> Config:
    """Load configuration from YAML file."""
    config_path = Path(path)

    if not config_path.exists():
        raise FileNotFoundError(
            f"Config file not found: {path}\n"
            "Copy config.example.yaml to config.yaml and fill in your values."
        )

    with open(config_path) as f:
        data = yaml.safe_load(f)

    # Parse orchestrator config with optional Google OAuth
    orchestrator_data = data.get("orchestrator", {})
    google_oauth = None
    if "google_oauth" in orchestrator_data:
        google_oauth = GoogleOAuthConfig(**orchestrator_data["google_oauth"])
        orchestrator_data = {k: v for k, v in orchestrator_data.items() if k != "google_oauth"}

    orchestrator_config = OrchestratorConfig(**orchestrator_data, google_oauth=google_oauth)

    # Parse optional Tuya Cloud config
    tuya_cloud = None
    if "tuya_cloud" in data:
        tuya_cloud = TuyaCloudConfig(**data["tuya_cloud"])

    return Config(
        yolink=YoLinkConfig(**data["yolink"]),
        qingping=QingpingConfig(**data["qingping"]),
        erv=ERVConfig(**data["erv"]),
        mitsubishi=MitsubishiConfig(**data.get("mitsubishi", {})),
        thresholds=ThresholdsConfig(**data.get("thresholds", {})),
        orchestrator=orchestrator_config,
        tuya_cloud=tuya_cloud,
    )
