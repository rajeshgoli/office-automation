"""
Pioneer Airlink ERV Client (Tuya-based)

Controls the ERV via local Tuya protocol with cloud fallback.

DPS Mapping (discovered via Smart Life):
- DP 1: Power on/off (bool)
- DP 2: Mode ('manua' = manual)
- DP 101: Supply Air (SA) fan speed (1-8)
- DP 102: Exhaust Air (EA) fan speed (1-8)

Fan Speed Presets (SA/EA):
- QUIET (1/1): Quietest operation for present + CO2 > 2000 ppm
- MEDIUM (3/2): Positive pressure (more supply than exhaust) to flush eTVOC
- TURBO (8/8): Full purge when away
- OFF: Power off
"""

import logging
from dataclasses import dataclass
from enum import Enum
from typing import Optional

import tinytuya

logger = logging.getLogger(__name__)


class FanSpeed(Enum):
    """ERV fan speed presets."""
    OFF = "off"
    QUIET = "quiet"      # 1/1 - minimal noise
    MEDIUM = "medium"    # 3/2 - positive pressure
    TURBO = "turbo"      # 8/8 - full purge


@dataclass
class ERVStatus:
    """Current ERV status."""
    power: bool
    fan_speed: Optional[FanSpeed]
    supply_speed: Optional[int]
    exhaust_speed: Optional[int]
    raw_dps: dict


class ERVClient:
    """
    Client for controlling Pioneer Airlink ERV.

    Tries local Tuya protocol first, falls back to cloud API.
    Note: Cloud API only supports on/off, not fan speed control.
    """

    # Data Point mappings
    DP_POWER = "1"
    DP_MODE = "2"
    DP_SUPPLY_SPEED = "101"
    DP_EXHAUST_SPEED = "102"

    # Fan speed presets: (supply, exhaust)
    SPEED_PRESETS = {
        FanSpeed.OFF: (1, 1),
        FanSpeed.QUIET: (1, 1),
        FanSpeed.MEDIUM: (3, 2),
        FanSpeed.TURBO: (8, 8),
    }

    def __init__(
        self,
        device_id: str,
        ip: str,
        local_key: str,
        version: float = 3.4,
        cloud_api_key: Optional[str] = None,
        cloud_api_secret: Optional[str] = None,
        cloud_region: str = "us"
    ):
        self.device_id = device_id
        self.ip = ip
        self.local_key = local_key
        self.version = version
        self._device: Optional[tinytuya.Device] = None
        self._cloud: Optional[tinytuya.Cloud] = None
        self._use_cloud = False

        # Set up cloud fallback if credentials provided
        if cloud_api_key and cloud_api_secret:
            self._cloud = tinytuya.Cloud(
                apiRegion=cloud_region,
                apiKey=cloud_api_key,
                apiSecret=cloud_api_secret
            )

    def connect(self):
        """Connect to the ERV device. Tries local first, falls back to cloud."""
        # Try local connection first
        logger.info(f"Connecting to ERV at {self.ip} (local)...")
        self._device = tinytuya.Device(
            dev_id=self.device_id,
            address=self.ip,
            local_key=self.local_key,
            version=self.version
        )

        status = self._device.status()
        if "Error" not in status:
            logger.info(f"Connected to ERV via local API. Status: {status}")
            self._use_cloud = False
            return status

        # Local failed, try cloud
        logger.warning(f"Local connection failed: {status}")
        if self._cloud:
            logger.info("Falling back to cloud API...")
            cloud_status = self._cloud.getstatus(self.device_id)
            if cloud_status.get('success'):
                logger.info(f"Connected to ERV via cloud API")
                self._use_cloud = True
                return cloud_status
            else:
                raise ConnectionError(f"Cloud connection also failed: {cloud_status}")
        else:
            raise ConnectionError(f"Local connection failed and no cloud credentials: {status}")

    def get_status(self) -> ERVStatus:
        """Get current ERV status."""
        if self._use_cloud:
            return self._get_status_cloud()
        else:
            return self._get_status_local()

    def _get_status_local(self) -> ERVStatus:
        """Get status via local API."""
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        status = self._device.status()
        if "Error" in status:
            raise RuntimeError(f"Failed to get status: {status}")

        dps = status.get("dps", {})
        power = dps.get(self.DP_POWER, False)
        supply = dps.get(self.DP_SUPPLY_SPEED)
        exhaust = dps.get(self.DP_EXHAUST_SPEED)

        fan_speed = None
        if not power:
            fan_speed = FanSpeed.OFF
        elif supply is not None and exhaust is not None:
            for preset, (s, e) in self.SPEED_PRESETS.items():
                if s == supply and e == exhaust:
                    fan_speed = preset
                    break

        return ERVStatus(
            power=power,
            fan_speed=fan_speed,
            supply_speed=supply,
            exhaust_speed=exhaust,
            raw_dps=dps
        )

    def _get_status_cloud(self) -> ERVStatus:
        """Get status via cloud API."""
        if not self._cloud:
            raise RuntimeError("Cloud not configured")

        result = self._cloud.getstatus(self.device_id)
        if not result.get('success'):
            raise RuntimeError(f"Failed to get cloud status: {result}")

        # Parse cloud status format
        status_list = result.get('result', [])
        status_dict = {item['code']: item['value'] for item in status_list}

        power = status_dict.get('switch', False)

        return ERVStatus(
            power=power,
            fan_speed=FanSpeed.OFF if not power else None,  # Cloud doesn't report speeds
            supply_speed=None,
            exhaust_speed=None,
            raw_dps=status_dict
        )

    def set_speed(self, speed: FanSpeed) -> bool:
        """Set ERV fan speed."""
        logger.info(f"Setting ERV speed to {speed.value}")

        if self._use_cloud:
            # Try local first in case the device recovered; cloud can't set speeds.
            if self._device:
                try:
                    result = self._set_speed_local(speed)
                    if result:
                        logger.info("Local ERV control recovered; disabling cloud fallback")
                        self._use_cloud = False
                        return True
                except Exception as e:
                    logger.warning(f"Local ERV control still unavailable: {e}")
            return self._set_speed_cloud(speed)
        else:
            result = self._set_speed_local(speed)
            if not result and self._cloud:
                # Local failed, try cloud
                logger.warning("Local set_speed failed, trying cloud...")
                self._use_cloud = True
                return self._set_speed_cloud(speed)
            return result

    def _set_speed_local(self, speed: FanSpeed) -> bool:
        """Set speed via local API."""
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        if speed == FanSpeed.OFF:
            result = self._device.set_value(self.DP_POWER, False)
        else:
            supply, exhaust = self.SPEED_PRESETS[speed]
            self._device.set_value(self.DP_POWER, True)
            self._device.set_value(self.DP_SUPPLY_SPEED, supply)
            result = self._device.set_value(self.DP_EXHAUST_SPEED, exhaust)

        if "Error" in str(result):
            logger.error(f"Failed to set speed: {result}")
            return False

        logger.info(f"ERV speed set to {speed.value}")
        return True

    def _set_speed_cloud(self, speed: FanSpeed) -> bool:
        """Set speed via cloud API (limited to on/off only)."""
        if not self._cloud:
            raise RuntimeError("Cloud not configured")

        if speed == FanSpeed.OFF:
            result = self._cloud.sendcommand(
                self.device_id,
                {'commands': [{'code': 'switch', 'value': False}]}
            )
        else:
            # Cloud can only turn on, can't set specific speeds
            result = self._cloud.sendcommand(
                self.device_id,
                {'commands': [{'code': 'switch', 'value': True}]}
            )
            if speed != FanSpeed.TURBO:
                logger.warning(f"Cloud API only supports on/off. Turned ON but can't set {speed.value} speed.")

        if result.get('success'):
            logger.info(f"ERV {'turned off' if speed == FanSpeed.OFF else 'turned on'} via cloud")
            return True
        else:
            logger.error(f"Cloud command failed: {result}")
            return False

    def turn_on(self, speed: FanSpeed = FanSpeed.TURBO) -> bool:
        """Turn on ERV with specified speed."""
        return self.set_speed(speed)

    def turn_off(self) -> bool:
        """Turn off ERV."""
        return self.set_speed(FanSpeed.OFF)


# Standalone test
if __name__ == "__main__":
    import sys
    logging.basicConfig(level=logging.INFO)

    if len(sys.argv) < 4:
        print("Usage: python -m src.erv_client <device_id> <ip> <local_key> [cloud_key] [cloud_secret]")
        sys.exit(1)

    device_id, ip, local_key = sys.argv[1:4]
    cloud_key = sys.argv[4] if len(sys.argv) > 4 else None
    cloud_secret = sys.argv[5] if len(sys.argv) > 5 else None

    client = ERVClient(
        device_id=device_id,
        ip=ip,
        local_key=local_key,
        cloud_api_key=cloud_key,
        cloud_api_secret=cloud_secret
    )

    print("Connecting...")
    client.connect()

    print("\nCurrent status:")
    status = client.get_status()
    print(f"  Power: {status.power}")
    print(f"  Fan Speed: {status.fan_speed}")
    print(f"  Supply: {status.supply_speed}")
    print(f"  Exhaust: {status.exhaust_speed}")
