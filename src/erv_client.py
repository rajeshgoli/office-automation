"""
Pioneer Airlink ERV Client (Tuya-based)

Controls the ERV via local Tuya protocol and reports local key health.

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
import time
from datetime import datetime
from dataclasses import dataclass
from enum import Enum
from typing import Callable, Optional

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

    The orchestrator configures this for local-only control so fan speeds stay
    available and local key rotation is surfaced immediately.
    """

    # Data Point mappings
    DP_POWER = "1"
    DP_MODE = "2"
    DP_SUPPLY_SPEED = "101"
    DP_EXHAUST_SPEED = "102"

    # Delay before read-back verification to allow the device to process commands
    VERIFY_DELAY_SECONDS = 1
    LOCAL_KEY_ERROR_THRESHOLD = 5
    LOCAL_KEY_ERROR_CODE = "914"

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
        self.last_error: Optional[str] = None
        self.last_error_at: Optional[datetime] = None
        self.last_ok_at: Optional[datetime] = None
        self.last_local_ok_at: Optional[datetime] = None
        self.consecutive_local_key_errors: int = 0
        self.local_key_invalid: bool = False
        self.local_key_invalid_since: Optional[datetime] = None
        self._health_event_callback: Optional[Callable[[dict], None]] = None

        # Set up cloud fallback if credentials provided
        if cloud_api_key and cloud_api_secret:
            self._cloud = tinytuya.Cloud(
                apiRegion=cloud_region,
                apiKey=cloud_api_key,
                apiSecret=cloud_api_secret
            )

    def on_health_event(self, callback: Callable[[dict], None]):
        """Register a callback for ERV control health transitions."""
        self._health_event_callback = callback

    def connect(self):
        """Connect to the ERV device over local Tuya."""
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
            self._record_local_success()
            return status

        # Local failed, try cloud
        logger.warning(f"Local connection failed: {status}")
        self._record_local_failure("Local connection failed", status)
        if self._cloud:
            logger.info("Falling back to cloud API...")
            cloud_status = self._cloud.getstatus(self.device_id)
            if cloud_status.get('success'):
                logger.info(f"Connected to ERV via cloud API")
                self._use_cloud = True
                self._record_success()
                return cloud_status
            else:
                self._record_error(f"Cloud connection failed: {cloud_status}")
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
            self._record_local_failure("Local status failed", status)
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

        self._record_local_success()
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
            self._record_error(f"Cloud status failed: {result}")
            raise RuntimeError(f"Failed to get cloud status: {result}")

        # Parse cloud status format
        status_list = result.get('result', [])
        status_dict = {item['code']: item['value'] for item in status_list}

        power = status_dict.get('switch', False)

        self._record_success()
        return ERVStatus(
            power=power,
            fan_speed=FanSpeed.OFF if not power else None,  # Cloud doesn't report speeds
            supply_speed=None,
            exhaust_speed=None,
            raw_dps=status_dict
        )

    def set_speed(self, speed: FanSpeed) -> bool:
        """Set ERV fan speed with reconnect retry on local failure."""
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
                    self._record_error(f"Local set_speed failed: {e}")
            return self._set_speed_cloud(speed)
        else:
            try:
                result = self._set_speed_local(speed)
            except Exception as e:
                logger.warning(f"Local set_speed error: {e}")
                self._record_error(f"Local set_speed error: {e}")
                result = False

            if not result:
                # Retry once after reconnect (handles stale sessions)
                logger.warning("Local set_speed failed, reconnecting and retrying...")
                try:
                    self.connect()
                    if not self._use_cloud:
                        result = self._set_speed_local(speed)
                except Exception as e:
                    logger.warning(f"Local reconnect/retry failed: {e}")
                    self._record_error(f"Local reconnect/retry failed: {e}")

            if not result and self._cloud:
                logger.warning("Local set_speed failed after retry, trying cloud...")
                self._use_cloud = True
                return self._set_speed_cloud(speed)
            return result

    def _set_speed_local(self, speed: FanSpeed) -> bool:
        """Set speed via local API with result checking and read-back verification."""
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        if speed == FanSpeed.OFF:
            result = self._device.set_value(self.DP_POWER, False)
            if "Error" in str(result):
                logger.error(f"Failed to set power off: {result}")
                self._record_local_failure("Local set power off failed", result)
                return False
        else:
            supply, exhaust = self.SPEED_PRESETS[speed]
            r1 = self._device.set_value(self.DP_POWER, True)
            if "Error" in str(r1):
                logger.error(f"Failed to set power on: {r1}")
                self._record_local_failure("Local set power on failed", r1)
                return False
            r2 = self._device.set_value(self.DP_SUPPLY_SPEED, supply)
            if "Error" in str(r2):
                logger.error(f"Failed to set supply speed: {r2}")
                self._record_local_failure("Local set supply speed failed", r2)
                return False
            r3 = self._device.set_value(self.DP_EXHAUST_SPEED, exhaust)
            if "Error" in str(r3):
                logger.error(f"Failed to set exhaust speed: {r3}")
                self._record_local_failure("Local set exhaust speed failed", r3)
                return False

        # Read-back verification: confirm the device actually changed state
        time.sleep(self.VERIFY_DELAY_SECONDS)
        try:
            actual = self._get_status_local()
            if speed == FanSpeed.OFF:
                if actual.power:
                    logger.error(f"ERV verification failed: expected power OFF, got ON")
                    self._record_error("ERV verification failed: expected power OFF, got ON")
                    return False
            else:
                if not actual.power:
                    logger.error(f"ERV verification failed: expected power ON, got OFF")
                    self._record_error("ERV verification failed: expected power ON, got OFF")
                    return False
                expected_supply, expected_exhaust = self.SPEED_PRESETS[speed]
                if actual.supply_speed != expected_supply or actual.exhaust_speed != expected_exhaust:
                    message = (
                        f"ERV verification failed: expected SA={expected_supply}/EA={expected_exhaust}, "
                        f"got SA={actual.supply_speed}/EA={actual.exhaust_speed}"
                    )
                    logger.error(message)
                    self._record_error(message)
                    return False
        except Exception as e:
            logger.error(f"ERV verification read-back failed: {e}")
            self._record_error(f"ERV verification read-back failed: {e}")
            return False

        logger.info(f"ERV speed set to {speed.value} (verified)")
        self._record_local_success()
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
            self._record_success()
            return True
        else:
            logger.error(f"Cloud command failed: {result}")
            self._record_error(f"Cloud set_speed failed: {result}")
            return False

    def turn_on(self, speed: FanSpeed = FanSpeed.TURBO) -> bool:
        """Turn on ERV with specified speed."""
        return self.set_speed(speed)

    def turn_off(self) -> bool:
        """Turn off ERV."""
        return self.set_speed(FanSpeed.OFF)

    def get_health(self) -> dict:
        """Return ERV control health info."""
        return {
            "last_ok_at": self.last_ok_at.isoformat() if self.last_ok_at else None,
            "last_local_ok_at": self.last_local_ok_at.isoformat() if self.last_local_ok_at else None,
            "last_error": self.last_error,
            "last_error_at": self.last_error_at.isoformat() if self.last_error_at else None,
            "using_cloud": self._use_cloud,
            "local_key_invalid": self.local_key_invalid,
            "local_key_invalid_since": self.local_key_invalid_since.isoformat() if self.local_key_invalid_since else None,
            "consecutive_local_key_errors": self.consecutive_local_key_errors,
        }

    def _record_error(self, message: str):
        self.last_error = message
        self.last_error_at = datetime.now()

    def _record_success(self):
        self.last_ok_at = datetime.now()
        self.last_error = None

    def _record_local_failure(self, context: str, result):
        self._record_error(f"{context}: {result}")

        if not self._is_local_key_error(result):
            self.consecutive_local_key_errors = 0
            return

        self.consecutive_local_key_errors += 1
        if (
            self.consecutive_local_key_errors >= self.LOCAL_KEY_ERROR_THRESHOLD and
            not self.local_key_invalid
        ):
            self.local_key_invalid = True
            self.local_key_invalid_since = datetime.now()
            logger.error(
                "ERV local Tuya key appears invalid after %s consecutive Err 914 failures",
                self.consecutive_local_key_errors,
            )
            self._emit_health_event({
                "type": "erv_local_key_invalid",
                "started_at": self.local_key_invalid_since.isoformat(),
                "consecutive_errors": self.consecutive_local_key_errors,
                "last_local_ok_at": self.last_local_ok_at.isoformat() if self.last_local_ok_at else None,
                "last_error": self.last_error,
            })

    def _record_local_success(self):
        was_invalid = self.local_key_invalid
        invalid_since = self.local_key_invalid_since

        self._record_success()
        self.last_local_ok_at = self.last_ok_at
        self.consecutive_local_key_errors = 0
        self.local_key_invalid = False
        self.local_key_invalid_since = None

        if was_invalid:
            logger.info("ERV local Tuya control recovered after local key invalid state")
            self._emit_health_event({
                "type": "erv_local_key_recovered",
                "recovered_at": self.last_local_ok_at.isoformat() if self.last_local_ok_at else None,
                "invalid_since": invalid_since.isoformat() if invalid_since else None,
            })

    def _emit_health_event(self, event: dict):
        if not self._health_event_callback:
            return
        try:
            self._health_event_callback(event)
        except Exception as e:
            logger.error(f"ERV health event callback failed: {e}")

    @classmethod
    def _is_local_key_error(cls, value) -> bool:
        if isinstance(value, dict):
            err = value.get("Err") or value.get("err")
            if str(err) == cls.LOCAL_KEY_ERROR_CODE:
                return True
            error = str(value.get("Error") or value.get("error") or "")
            if "Check device key or version" in error:
                return True
            return any(cls._is_local_key_error(item) for item in value.values())
        if isinstance(value, (list, tuple)):
            return any(cls._is_local_key_error(item) for item in value)
        text = str(value)
        return (
            cls.LOCAL_KEY_ERROR_CODE in text and
            ("Err" in text or "Check device key or version" in text)
        )


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
