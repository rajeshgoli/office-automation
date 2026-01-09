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


@dataclass
class GoogleOAuthConfig:
    client_id: str
    client_secret: str
    allowed_emails: list[str]
    token_expiry_days: int = 7
    device_flow_enabled: bool = True
    jwt_secret: Optional[str] = None


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

    return Config(
        yolink=YoLinkConfig(**data["yolink"]),
        qingping=QingpingConfig(**data["qingping"]),
        erv=ERVConfig(**data["erv"]),
        mitsubishi=MitsubishiConfig(**data.get("mitsubishi", {})),
        thresholds=ThresholdsConfig(**data.get("thresholds", {})),
        orchestrator=orchestrator_config,
    )
