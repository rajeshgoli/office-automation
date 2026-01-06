"""
Pioneer Airlink ERV Client (Tuya-based)

Controls the ERV via local Tuya protocol using tinytuya.

DPS Mapping (discovered via Smart Life):
- DP 1: Power on/off (bool)
- DP 2: Mode ('manua' = manual)
- DP 101: Supply Air (SA) fan speed (1-8)
- DP 102: Exhaust Air (EA) fan speed (1-8)
- DP 6: CO2 ppm (device sensor - using Qingping instead)
- DP 8: Humidity %
- DP 9: Temperature °F

Fan Speed Presets (SA/EA):
- QUIET (1/1): Quietest operation for present + CO2 > 2000 ppm
- MEDIUM (3/2): Positive pressure (more supply than exhaust) to flush eTVOC
- TURBO (8/8): Full purge when away
- OFF: Power off (can't set fans to 0 when on)
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
    Client for controlling Pioneer Airlink ERV via Tuya local API.

    Usage:
        client = ERVClient(device_id="...", ip="...", local_key="...")
        await client.connect()
        await client.set_speed(FanSpeed.TURBO)
        status = await client.get_status()
    """

    # Data Point mappings - discovered via Smart Life testing
    DP_POWER = "1"           # Power on/off (bool)
    DP_MODE = "2"            # Mode ('manua' = manual)
    DP_SUPPLY_SPEED = "101"  # Supply Air (SA) fan speed (1-8)
    DP_EXHAUST_SPEED = "102" # Exhaust Air (EA) fan speed (1-8)
    DP_CO2 = "6"             # CO2 ppm (using Qingping instead)
    DP_HUMIDITY = "8"        # Humidity %
    DP_TEMP = "9"            # Temperature °F

    # Fan speed presets: (supply, exhaust) - range is 1-8
    SPEED_PRESETS = {
        FanSpeed.OFF: (1, 1),     # Will turn off power instead
        FanSpeed.QUIET: (1, 1),   # Quietest
        FanSpeed.MEDIUM: (3, 2),  # Positive pressure for eTVOC flush
        FanSpeed.TURBO: (8, 8),   # Full purge when away
    }

    def __init__(
        self,
        device_id: str,
        ip: str,
        local_key: str,
        version: float = 3.4
    ):
        self.device_id = device_id
        self.ip = ip
        self.local_key = local_key
        self.version = version
        self._device: Optional[tinytuya.Device] = None

    def connect(self):
        """Connect to the ERV device."""
        logger.info(f"Connecting to ERV at {self.ip}...")
        self._device = tinytuya.Device(
            dev_id=self.device_id,
            address=self.ip,
            local_key=self.local_key,
            version=self.version
        )
        # Test connection by getting status
        status = self._device.status()
        if "Error" in status:
            raise ConnectionError(f"Failed to connect to ERV: {status}")
        logger.info(f"Connected to ERV. Status: {status}")
        return status

    def get_status(self) -> ERVStatus:
        """Get current ERV status."""
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        status = self._device.status()
        if "Error" in status:
            raise RuntimeError(f"Failed to get status: {status}")

        dps = status.get("dps", {})

        # Parse status - adjust based on actual DPs discovered
        power = dps.get(self.DP_POWER, False)
        supply = dps.get(self.DP_SUPPLY_SPEED)
        exhaust = dps.get(self.DP_EXHAUST_SPEED)

        # Determine fan speed preset
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

    def set_speed(self, speed: FanSpeed) -> bool:
        """Set ERV fan speed."""
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        logger.info(f"Setting ERV speed to {speed.value}")

        if speed == FanSpeed.OFF:
            # Turn off
            result = self._device.set_value(self.DP_POWER, False)
        else:
            supply, exhaust = self.SPEED_PRESETS[speed]
            # Turn on and set speeds
            self._device.set_value(self.DP_POWER, True)
            self._device.set_value(self.DP_SUPPLY_SPEED, supply)
            result = self._device.set_value(self.DP_EXHAUST_SPEED, exhaust)

        if "Error" in str(result):
            logger.error(f"Failed to set speed: {result}")
            return False

        logger.info(f"ERV speed set to {speed.value}")
        return True

    def turn_on(self, speed: FanSpeed = FanSpeed.TURBO) -> bool:
        """Turn on ERV with specified speed."""
        return self.set_speed(speed)

    def turn_off(self) -> bool:
        """Turn off ERV."""
        return self.set_speed(FanSpeed.OFF)

    def discover_dps(self) -> dict:
        """
        Discover available Data Points by querying the device.
        Useful for figuring out what controls are available.
        """
        if not self._device:
            raise RuntimeError("Not connected. Call connect() first.")

        status = self._device.status()
        logger.info(f"Device DPs: {status}")
        return status


# Standalone test
if __name__ == "__main__":
    import sys
    logging.basicConfig(level=logging.INFO)

    if len(sys.argv) < 4:
        print("Usage: python -m src.erv_client <device_id> <ip> <local_key>")
        print("Example: python -m src.erv_client eb9f48addc097ca5dbvcwg 192.168.5.119 abc123...")
        sys.exit(1)

    device_id, ip, local_key = sys.argv[1:4]

    client = ERVClient(device_id=device_id, ip=ip, local_key=local_key)

    print("Connecting...")
    client.connect()

    print("\nDiscovering DPs...")
    dps = client.discover_dps()
    print(f"Raw DPs: {dps}")

    print("\nCurrent status:")
    status = client.get_status()
    print(f"  Power: {status.power}")
    print(f"  Fan Speed: {status.fan_speed}")
    print(f"  Supply: {status.supply_speed}")
    print(f"  Exhaust: {status.exhaust_speed}")
    print(f"  Raw: {status.raw_dps}")
