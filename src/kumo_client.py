"""
Kumo Cloud client for Mitsubishi mini-split control.
API reverse-engineered from the Mitsubishi Comfort app.
"""

import aiohttp
import asyncio
import json
import time
import jwt
from typing import Optional, Dict, Any
from enum import Enum


class OperationMode(str, Enum):
    OFF = "off"
    HEAT = "heat"
    COOL = "cool"
    AUTO = "auto"
    DRY = "dry"
    VENT = "vent"  # Fan only


class FanSpeed(str, Enum):
    AUTO = "auto"
    QUIET = "quiet"
    SUPER_QUIET = "superQuiet"
    LOW = "low"
    POWERFUL = "powerful"
    SUPER_POWERFUL = "superPowerful"


class AirDirection(str, Enum):
    AUTO = "auto"
    HORIZONTAL = "horizontal"      # Highest
    MID_HORIZONTAL = "midhorizontal"  # High
    MIDPOINT = "midpoint"          # Middle
    MID_VERTICAL = "midvertical"   # Low
    VERTICAL = "vertical"          # Lowest
    SWING = "swing"


class KumoClient:
    """Client for Kumo Cloud API."""

    BASE_URL = "https://app-prod.kumocloud.com"

    def __init__(self, username: str, password: str, device_serial: Optional[str] = None):
        self.username = username
        self.password = password
        self.device_serial = device_serial
        self._token: Optional[str] = None
        self._token_expiry: float = 0
        self._session: Optional[aiohttp.ClientSession] = None

    async def _get_session(self) -> aiohttp.ClientSession:
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession()
        return self._session

    async def close(self):
        if self._session and not self._session.closed:
            await self._session.close()

    async def _ensure_token(self):
        """Ensure we have a valid token, refreshing if needed."""
        if self._token and time.time() < self._token_expiry - 60:
            return  # Token still valid

        await self._login()

    async def _login(self):
        """Login to Kumo Cloud and get JWT token."""
        session = await self._get_session()

        async with session.post(
            f"{self.BASE_URL}/v3/login",
            json={
                "username": self.username,
                "password": self.password,
                "appVersion": "1297"
            },
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                text = await resp.text()
                raise Exception(f"Login failed: {resp.status} - {text}")

            data = await resp.json()
            # V3 API returns token in data['token']['access']
            token_obj = data.get("token", {})
            self._token = token_obj.get("access")

            if not self._token:
                raise Exception(f"No token in response: {data}")

            # Decode token to get expiry
            try:
                payload = jwt.decode(self._token, options={"verify_signature": False})
                self._token_expiry = payload.get("exp", time.time() + 3600)
            except:
                self._token_expiry = time.time() + 3600

    def _headers(self) -> Dict[str, str]:
        return {
            "Authorization": f"Bearer {self._token}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "kumocloud/1297 CFNetwork/3826.600.41 Darwin/24.6.0",
            "X-App-Version": "1297",
            "X-App-Platform": "ios",
        }

    async def get_sites(self) -> list:
        """Get all sites for the user."""
        await self._ensure_token()
        session = await self._get_session()

        async with session.get(
            f"{self.BASE_URL}/v3/sites",
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                raise Exception(f"Failed to get sites: {resp.status}")
            return await resp.json()

    async def get_zones(self, site_id: str) -> list:
        """Get all zones (devices) for a site."""
        await self._ensure_token()
        session = await self._get_session()

        async with session.get(
            f"{self.BASE_URL}/v3/sites/{site_id}/zones",
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                raise Exception(f"Failed to get zones: {resp.status}")
            return await resp.json()

    async def get_device_status(self, device_serial: Optional[str] = None) -> Dict[str, Any]:
        """Get status of a specific device.

        NOTE: This endpoint only returns device metadata (firmware, WiFi, etc.),
        NOT the current operating state. Use get_full_status() for operating state.
        """
        serial = device_serial or self.device_serial
        if not serial:
            raise ValueError("No device serial specified")

        await self._ensure_token()
        session = await self._get_session()

        async with session.get(
            f"{self.BASE_URL}/v3/devices/{serial}/status",
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                raise Exception(f"Failed to get device status: {resp.status}")
            return await resp.json()

    async def get_full_status(self, device_serial: Optional[str] = None) -> Dict[str, Any]:
        """Get full operating status including mode, setpoints, power state.

        This queries the zones endpoint which has the complete device state,
        unlike get_device_status() which only has metadata.
        """
        serial = device_serial or self.device_serial
        if not serial:
            raise ValueError("No device serial specified")

        # Get all sites
        sites = await self.get_sites()
        if not sites:
            raise Exception("No sites found")

        # Search all zones in all sites for our device
        for site in sites:
            site_id = site.get("id")
            zones = await self.get_zones(site_id)

            for zone in zones:
                adapter = zone.get("adapter", {})
                if adapter.get("deviceSerial") == serial:
                    # Return the adapter object which has all the state
                    return adapter

        raise Exception(f"Device {serial} not found in any zone")

    async def send_command(
        self,
        device_serial: Optional[str] = None,
        operation_mode: Optional[OperationMode] = None,
        fan_speed: Optional[FanSpeed] = None,
        air_direction: Optional[AirDirection] = None,
        heat_setpoint: Optional[float] = None,
        cool_setpoint: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Send command to device."""
        serial = device_serial or self.device_serial
        if not serial:
            raise ValueError("No device serial specified")

        await self._ensure_token()
        session = await self._get_session()

        commands = {}

        if operation_mode is not None:
            commands["operationMode"] = operation_mode.value
            # Include setpoints when changing mode
            if heat_setpoint is not None:
                commands["spHeat"] = heat_setpoint
            if cool_setpoint is not None:
                commands["spCool"] = cool_setpoint

        if fan_speed is not None:
            commands["fanSpeed"] = fan_speed.value

        if air_direction is not None:
            commands["airDirection"] = air_direction.value

        # Allow setting setpoints without changing mode
        if operation_mode is None:
            if heat_setpoint is not None:
                commands["spHeat"] = heat_setpoint
            if cool_setpoint is not None:
                commands["spCool"] = cool_setpoint

        if not commands:
            raise ValueError("No commands specified")

        payload = {
            "deviceSerial": serial,
            "commands": commands
        }

        async with session.post(
            f"{self.BASE_URL}/v3/devices/send-command",
            json=payload,
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                text = await resp.text()
                raise Exception(f"Failed to send command: {resp.status} - {text}")
            return await resp.json()

    # Convenience methods
    async def turn_off(self, device_serial: Optional[str] = None):
        """Turn off the unit."""
        return await self.send_command(
            device_serial=device_serial,
            operation_mode=OperationMode.OFF
        )

    async def set_heat(
        self,
        temperature: float,
        device_serial: Optional[str] = None,
        fan_speed: Optional[FanSpeed] = None
    ):
        """Set to heat mode with specified temperature."""
        return await self.send_command(
            device_serial=device_serial,
            operation_mode=OperationMode.HEAT,
            heat_setpoint=temperature,
            fan_speed=fan_speed
        )

    async def set_cool(
        self,
        temperature: float,
        device_serial: Optional[str] = None,
        fan_speed: Optional[FanSpeed] = None
    ):
        """Set to cool mode with specified temperature."""
        return await self.send_command(
            device_serial=device_serial,
            operation_mode=OperationMode.COOL,
            cool_setpoint=temperature,
            fan_speed=fan_speed
        )

    async def set_auto(
        self,
        heat_setpoint: float,
        cool_setpoint: float,
        device_serial: Optional[str] = None
    ):
        """Set to auto mode with heat and cool setpoints."""
        return await self.send_command(
            device_serial=device_serial,
            operation_mode=OperationMode.AUTO,
            heat_setpoint=heat_setpoint,
            cool_setpoint=cool_setpoint
        )

    async def set_fan_speed(
        self,
        fan_speed: FanSpeed,
        device_serial: Optional[str] = None
    ):
        """Change fan speed without changing mode."""
        return await self.send_command(
            device_serial=device_serial,
            fan_speed=fan_speed
        )

    async def set_vane(
        self,
        direction: AirDirection,
        device_serial: Optional[str] = None
    ):
        """Change vane/air direction without changing mode."""
        return await self.send_command(
            device_serial=device_serial,
            air_direction=direction
        )


async def main():
    """Test the client."""
    import yaml

    # Load config
    with open("config.yaml") as f:
        config = yaml.safe_load(f)

    kumo_config = config.get("mitsubishi", {})
    username = kumo_config.get("username")
    password = kumo_config.get("password")
    device_serial = kumo_config.get("device_serial")

    if not username or not password or username == "placeholder":
        print("Please configure mitsubishi username/password in config.yaml")
        return

    client = KumoClient(username, password, device_serial)

    try:
        # Get sites
        print("Getting sites...")
        sites = await client.get_sites()
        print(f"Sites: {json.dumps(sites, indent=2)}")

        if sites:
            site_id = sites[0].get("id")
            print(f"\nGetting zones for site {site_id}...")
            zones = await client.get_zones(site_id)
            print(f"Zones: {json.dumps(zones, indent=2)}")

        # Get device status
        print(f"\nGetting device status for {device_serial}...")
        status = await client.get_device_status()
        print(f"Status: {json.dumps(status, indent=2)}")

    finally:
        await client.close()


if __name__ == "__main__":
    asyncio.run(main())
