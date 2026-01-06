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


@dataclass
class ThresholdsConfig:
    co2_critical_ppm: int = 2000
    co2_refresh_target_ppm: int = 500
    motion_timeout_seconds: int = 60
    mac_poll_interval_seconds: int = 5


@dataclass
class OrchestratorConfig:
    host: str = "0.0.0.0"
    port: int = 8080


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

    return Config(
        yolink=YoLinkConfig(**data["yolink"]),
        qingping=QingpingConfig(**data["qingping"]),
        erv=ERVConfig(**data["erv"]),
        mitsubishi=MitsubishiConfig(**data.get("mitsubishi", {})),
        thresholds=ThresholdsConfig(**data.get("thresholds", {})),
        orchestrator=OrchestratorConfig(**data.get("orchestrator", {})),
    )
