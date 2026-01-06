"""
YoLink Cloud API Client

Communicates with YoLink cloud via HTTP API and MQTT for real-time events.
Based on: https://doc.yosmart.com/docs/protocol/openAPIV2/
"""

import asyncio
import json
import time
import logging
from dataclasses import dataclass, field
from typing import Optional, Callable, Any
from enum import Enum

import aiohttp
import aiomqtt

from .config import YoLinkConfig


logger = logging.getLogger(__name__)


class DeviceType(Enum):
    DOOR_SENSOR = "DoorSensor"
    MOTION_SENSOR = "MotionSensor"
    CONTACT_SENSOR = "ContactSensor"
    UNKNOWN = "Unknown"


@dataclass
class YoLinkDevice:
    device_id: str
    name: str
    token: str
    device_type: DeviceType
    state: dict = field(default_factory=dict)

    @property
    def is_open(self) -> Optional[bool]:
        """For door/contact sensors: True if open, False if closed."""
        if "state" in self.state:
            return self.state["state"] == "open"
        return None

    @property
    def motion_detected(self) -> Optional[bool]:
        """For motion sensors: True if motion detected."""
        if "state" in self.state:
            return self.state["state"] == "alert"
        return None

    @property
    def is_online(self) -> bool:
        return self.state.get("online", False)


class YoLinkClient:
    """Client for YoLink Cloud API."""

    def __init__(self, config: YoLinkConfig):
        self.config = config
        self.access_token: Optional[str] = None
        self.token_expires: float = 0
        self.home_id: Optional[str] = None
        self.devices: dict[str, YoLinkDevice] = {}
        self._mqtt_task: Optional[asyncio.Task] = None
        self._event_callbacks: list[Callable[[YoLinkDevice, dict], Any]] = []

    async def authenticate(self) -> str:
        """Get access token from YoLink cloud."""
        url = f"{self.config.http_url}/open/yolink/token"

        payload = {
            "grant_type": "client_credentials",
            "client_id": self.config.uaid,
            "client_secret": self.config.secret_key,
        }

        async with aiohttp.ClientSession() as session:
            async with session.post(url, json=payload) as resp:
                if resp.status != 200:
                    raise Exception(f"Auth failed: {resp.status} {await resp.text()}")

                data = await resp.json()

                if "access_token" not in data:
                    raise Exception(f"No access_token in response: {data}")

                self.access_token = data["access_token"]
                # Token typically expires in 7200 seconds (2 hours)
                expires_in = data.get("expires_in", 7200)
                self.token_expires = time.time() + expires_in - 60  # Refresh 1 min early

                logger.info(f"Authenticated with YoLink hub, token expires in {expires_in}s")
                return self.access_token

    async def _ensure_token(self):
        """Ensure we have a valid access token."""
        if not self.access_token or time.time() > self.token_expires:
            await self.authenticate()

    async def _api_call(self, method: str, target_device: Optional[dict] = None) -> dict:
        """Make an API call to the local hub."""
        await self._ensure_token()

        url = f"{self.config.http_url}/open/yolink/v2/api"

        payload = {
            "method": method,
            "time": int(time.time() * 1000),
        }

        if target_device:
            payload["targetDevice"] = target_device

        headers = {"Authorization": f"Bearer {self.access_token}"}

        async with aiohttp.ClientSession() as session:
            async with session.post(url, json=payload, headers=headers) as resp:
                data = await resp.json()

                if data.get("code") != "000000":
                    raise Exception(f"API error: {data}")

                return data

    async def get_home_id(self) -> str:
        """Fetch the Home ID required for MQTT subscription."""
        data = await self._api_call("Home.getGeneralInfo")
        self.home_id = data.get("data", {}).get("id")
        if not self.home_id:
            raise Exception(f"Could not get Home ID: {data}")
        logger.info(f"Got Home ID: {self.home_id}")
        return self.home_id

    async def get_devices(self) -> list[YoLinkDevice]:
        """Fetch all devices from the hub."""
        data = await self._api_call("Home.getDeviceList")

        self.devices.clear()

        for device_data in data.get("data", {}).get("devices", []):
            device_id = device_data["deviceId"]
            device_type_str = device_data.get("type", "Unknown")

            try:
                device_type = DeviceType(device_type_str)
            except ValueError:
                device_type = DeviceType.UNKNOWN

            device = YoLinkDevice(
                device_id=device_id,
                name=device_data.get("name", device_id),
                token=device_data.get("token", ""),
                device_type=device_type,
            )

            self.devices[device_id] = device
            logger.info(f"Found device: {device.name} ({device_type_str})")

        return list(self.devices.values())

    async def get_device_state(self, device: YoLinkDevice) -> dict:
        """Get current state of a device."""
        method = f"{device.device_type.value}.getState"

        data = await self._api_call(
            method,
            target_device={"deviceId": device.device_id, "token": device.token},
        )

        state = data.get("data", {})
        device.state = state
        return state

    async def refresh_all_states(self):
        """Refresh state of all known devices."""
        for device in self.devices.values():
            try:
                await self.get_device_state(device)
                logger.debug(f"Refreshed {device.name}: {device.state}")
            except Exception as e:
                logger.error(f"Failed to refresh {device.name}: {e}")

    def on_event(self, callback: Callable[[YoLinkDevice, dict], Any]):
        """Register a callback for device events."""
        self._event_callbacks.append(callback)

    async def _handle_mqtt_message(self, message: aiomqtt.Message):
        """Handle incoming MQTT message."""
        try:
            payload = json.loads(message.payload.decode())
            device_id = payload.get("deviceId")

            if device_id and device_id in self.devices:
                device = self.devices[device_id]
                event_data = payload.get("data", {})

                # Update device state
                device.state.update(event_data)

                logger.info(f"Event from {device.name}: {event_data}")

                # Notify callbacks
                for callback in self._event_callbacks:
                    try:
                        result = callback(device, event_data)
                        if asyncio.iscoroutine(result):
                            await result
                    except Exception as e:
                        logger.error(f"Callback error: {e}")

        except Exception as e:
            logger.error(f"Error handling MQTT message: {e}")

    async def start_mqtt(self):
        """Start MQTT subscription for real-time events."""
        await self._ensure_token()

        if not self.home_id:
            await self.get_home_id()

        logger.info(f"Connecting to MQTT at {self.config.mqtt_host}:{self.config.mqtt_port}")

        async with aiomqtt.Client(
            hostname=self.config.mqtt_host,
            port=self.config.mqtt_port,
            username=self.access_token,
            password="",
        ) as client:
            # Subscribe to device report topic using Home ID
            topic = f"yl-home/{self.home_id}/+/report"
            await client.subscribe(topic)
            logger.info(f"Subscribed to YoLink MQTT: {topic}")

            async for message in client.messages:
                await self._handle_mqtt_message(message)

    async def start(self):
        """Initialize client: authenticate, fetch devices, start MQTT."""
        await self.authenticate()
        await self.get_home_id()
        await self.get_devices()
        # Note: State polling doesn't work with UAC, we rely on MQTT events

        # Start MQTT listener in background
        self._mqtt_task = asyncio.create_task(self._mqtt_listener_loop())

    async def _mqtt_listener_loop(self):
        """MQTT listener with auto-reconnect."""
        while True:
            try:
                await self.start_mqtt()
            except Exception as e:
                logger.error(f"MQTT connection error: {e}, reconnecting in 5s...")
                await asyncio.sleep(5)

    async def stop(self):
        """Stop the client."""
        if self._mqtt_task:
            self._mqtt_task.cancel()
            try:
                await self._mqtt_task
            except asyncio.CancelledError:
                pass


# Convenience function for testing
async def main():
    """Test the YoLink client."""
    import sys

    logging.basicConfig(level=logging.INFO)

    # Load config
    sys.path.insert(0, str(Path(__file__).parent.parent))
    from src.config import load_config

    config = load_config()

    client = YoLinkClient(config.yolink)

    def on_event(device: YoLinkDevice, event: dict):
        print(f"EVENT: {device.name} -> {event}")

    client.on_event(on_event)

    try:
        await client.start()

        # Keep running
        while True:
            await asyncio.sleep(1)

    except KeyboardInterrupt:
        print("\nStopping...")
    finally:
        await client.stop()


if __name__ == "__main__":
    from pathlib import Path
    asyncio.run(main())
