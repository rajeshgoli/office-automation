"""
Qingping Air Monitor Client

Supports:
- Local MQTT (via developer portal private access) - RECOMMENDED
- BLE scanning for Air Monitor Lite devices (local, no cloud)
- Cloud API (requires reverse-engineered signing - unreliable)

BLE Service UUID: 0000fdcd-0000-1000-8000-00805f9b34fb
"""

import asyncio
import json
import logging
from dataclasses import dataclass, field
from typing import Optional, Dict, List, Callable
from datetime import datetime

try:
    from bleak import BleakScanner
    HAS_BLEAK = True
except ImportError:
    HAS_BLEAK = False

try:
    import paho.mqtt.client as mqtt
    HAS_PAHO = True
except ImportError:
    HAS_PAHO = False

logger = logging.getLogger(__name__)

QINGPING_SERVICE_UUID = "0000fdcd-0000-1000-8000-00805f9b34fb"


@dataclass
class QingpingReading:
    """Sensor readings from a Qingping device."""
    device_name: str
    mac_hint: str  # From service data header (not actual BLE MAC on macOS)
    temp_c: Optional[float] = None
    humidity: Optional[float] = None
    co2_ppm: Optional[int] = None
    pm25: Optional[int] = None
    pm10: Optional[int] = None
    tvoc: Optional[int] = None
    timestamp: Optional[datetime] = None
    raw_data: Optional[str] = None


class QingpingMQTTClient:
    """
    MQTT client for Qingping Air Monitor (WiFi version).

    Connects to a local MQTT broker that receives data from Qingping devices
    configured via the developer portal's "Private Access" feature.

    Topic format: qingping/{mac}/up for device data
    """

    def __init__(
        self,
        device_mac: str,
        mqtt_host: str = "127.0.0.1",
        mqtt_port: int = 1883,
        mqtt_user: Optional[str] = None,
        mqtt_pass: Optional[str] = None,
    ):
        if not HAS_PAHO:
            raise ImportError("paho-mqtt is required. Install with: pip install paho-mqtt")

        self.device_mac = device_mac.replace(":", "").upper()
        self.mqtt_host = mqtt_host
        self.mqtt_port = mqtt_port
        self.mqtt_user = mqtt_user
        self.mqtt_pass = mqtt_pass

        self._client: Optional[mqtt.Client] = None
        self._latest_reading: Optional[QingpingReading] = None
        self._reading_callback: Optional[Callable[[QingpingReading], None]] = None
        self._connected = False

    def _on_connect(self, client, userdata, flags, rc, properties=None):
        if rc == 0:
            logger.info(f"Connected to MQTT broker at {self.mqtt_host}:{self.mqtt_port}")
            self._connected = True
            # Subscribe to device topic
            topic = f"qingping/{self.device_mac}/up"
            client.subscribe(topic)
            logger.info(f"Subscribed to {topic}")
        else:
            logger.error(f"MQTT connection failed with code {rc}")
            self._connected = False

    def _on_disconnect(self, client, userdata, disconnect_flags, rc, properties=None):
        self._connected = False
        logger.info(f"Disconnected from MQTT broker (rc={rc})")

    def _on_message(self, client, userdata, msg):
        try:
            payload = json.loads(msg.payload.decode())
            logger.debug(f"Received MQTT message: {payload}")

            # Parse sensor data (type 17 is sensor report)
            msg_type = payload.get("type")

            if msg_type == 17:  # Sensor data report
                sensor_data = payload.get("sensor_data", {})

                reading = QingpingReading(
                    device_name=f"Qingping Air Monitor",
                    mac_hint=payload.get("mac", self.device_mac),
                    temp_c=sensor_data.get("temperature", {}).get("value"),
                    humidity=sensor_data.get("humidity", {}).get("value"),
                    co2_ppm=sensor_data.get("co2", {}).get("value"),
                    pm25=sensor_data.get("pm25", {}).get("value"),
                    pm10=sensor_data.get("pm10", {}).get("value"),
                    tvoc=sensor_data.get("tvoc", {}).get("value"),
                    timestamp=datetime.fromtimestamp(payload.get("timestamp", 0)),
                    raw_data=msg.payload.decode(),
                )

                self._latest_reading = reading
                logger.info(f"Qingping: {reading.temp_c}°C, {reading.humidity}%, CO2={reading.co2_ppm}ppm")

                if self._reading_callback:
                    self._reading_callback(reading)
            else:
                logger.debug(f"Ignoring message type {msg_type}")

        except Exception as e:
            logger.error(f"Error parsing MQTT message: {e}")

    def connect(self):
        """Connect to the MQTT broker."""
        self._client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2)
        self._client.on_connect = self._on_connect
        self._client.on_disconnect = self._on_disconnect
        self._client.on_message = self._on_message

        if self.mqtt_user and self.mqtt_pass:
            self._client.username_pw_set(self.mqtt_user, self.mqtt_pass)

        self._client.connect(self.mqtt_host, self.mqtt_port, keepalive=60)
        self._client.loop_start()

    def disconnect(self):
        """Disconnect from the MQTT broker."""
        if self._client:
            self._client.loop_stop()
            self._client.disconnect()
            self._connected = False

    def set_callback(self, callback: Callable[[QingpingReading], None]):
        """Set a callback function to be called when new readings arrive."""
        self._reading_callback = callback

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def latest_reading(self) -> Optional[QingpingReading]:
        """Get the most recent reading (may be stale)."""
        return self._latest_reading

    async def get_reading(self, timeout: float = 120.0) -> Optional[QingpingReading]:
        """
        Wait for a fresh reading from the device.

        Note: Device upload interval is typically 10-60 minutes,
        so this may take a while. Consider using latest_reading instead.
        """
        if not self._connected:
            self.connect()

        start_reading = self._latest_reading
        start_time = asyncio.get_event_loop().time()

        while asyncio.get_event_loop().time() - start_time < timeout:
            if self._latest_reading is not start_reading:
                return self._latest_reading
            await asyncio.sleep(1)

        return self._latest_reading  # Return stale reading if timeout


def parse_qingping_ble(data: bytes) -> Dict:
    """
    Parse Qingping BLE service data (UUID 0xFDCD).

    Format: 8-byte header + TLV sensor data
    TLV types:
      0x01 (len 4): Temperature (2 bytes) + Humidity (2 bytes)
      0x04 (len 4): PM2.5 (2 bytes) + PM10 (2 bytes)
      0x12 (len 4): CO2 (2 bytes) + ? (2 bytes)
      0x13 (len 2): tVOC
    """
    readings = {}

    if len(data) < 10:
        return readings

    # Extract MAC hint from header (bytes 2-8, reversed)
    mac_bytes = data[2:8]
    readings['mac_hint'] = ':'.join(f'{b:02x}' for b in reversed(mac_bytes))

    # Parse TLV data after 8-byte header
    i = 8
    while i < len(data) - 1:
        sensor_type = data[i]
        length = data[i + 1]

        if i + 2 + length > len(data):
            break

        value = data[i + 2:i + 2 + length]

        if sensor_type == 0x01 and length == 4:
            # Temperature + Humidity
            readings['temp_c'] = int.from_bytes(value[0:2], 'little', signed=True) / 10
            readings['humidity'] = int.from_bytes(value[2:4], 'little') / 10
        elif sensor_type == 0x04 and length == 4:
            # PM2.5 + PM10
            readings['pm25'] = int.from_bytes(value[0:2], 'little')
            readings['pm10'] = int.from_bytes(value[2:4], 'little')
        elif sensor_type == 0x12 and length == 4:
            # CO2 (first 2 bytes)
            readings['co2_ppm'] = int.from_bytes(value[0:2], 'little')
        elif sensor_type == 0x13 and length == 2:
            # tVOC
            readings['tvoc'] = int.from_bytes(value, 'little')

        i += 2 + length

    return readings


class QingpingBLEClient:
    """
    BLE client for Qingping Air Monitor Lite devices.

    These devices broadcast sensor data via BLE advertisements.
    No pairing required - just passive scanning.
    """

    def __init__(self, target_mac: Optional[str] = None):
        """
        Initialize BLE client.

        Args:
            target_mac: Optional MAC to filter for specific device
        """
        self.target_mac = target_mac.lower().replace(':', '') if target_mac else None

    async def scan(self, timeout: float = 10.0) -> List[QingpingReading]:
        """
        Scan for Qingping BLE devices and return their readings.

        Args:
            timeout: Scan duration in seconds

        Returns:
            List of QingpingReading from discovered devices
        """
        logger.info(f"Scanning for Qingping BLE devices ({timeout}s)...")

        devices = await BleakScanner.discover(timeout=timeout, return_adv=True)
        readings = []

        for addr, (device, adv) in devices.items():
            if QINGPING_SERVICE_UUID not in adv.service_data:
                continue

            data = adv.service_data[QINGPING_SERVICE_UUID]
            parsed = parse_qingping_ble(data)

            # Filter by MAC if specified
            if self.target_mac:
                mac_hint = parsed.get('mac_hint', '').replace(':', '')
                if self.target_mac not in mac_hint and mac_hint not in self.target_mac:
                    continue

            reading = QingpingReading(
                device_name=device.name or "Unknown Qingping",
                mac_hint=parsed.get('mac_hint', ''),
                temp_c=parsed.get('temp_c'),
                humidity=parsed.get('humidity'),
                co2_ppm=parsed.get('co2_ppm'),
                pm25=parsed.get('pm25'),
                pm10=parsed.get('pm10'),
                tvoc=parsed.get('tvoc'),
                raw_data=data.hex(),
            )
            readings.append(reading)

            logger.info(f"Found: {reading.device_name} - {reading.temp_c}°C, {reading.humidity}% RH")

        return readings

    async def get_reading(self, timeout: float = 10.0) -> Optional[QingpingReading]:
        """
        Get a single reading (first device found, or target if specified).
        """
        readings = await self.scan(timeout)
        return readings[0] if readings else None


class QingpingAPIClient:
    """
    API client for Qingping Air Monitor (WiFi version).

    Uses the Qingping+ cloud API (reverse-engineered from app).
    """

    BASE_URL = "https://qingplus.cleargrass.com"

    def __init__(self, jwt_token: str, device_id: str, device_mac: str, model: str = "Snow2"):
        self.jwt_token = jwt_token
        self.device_id = device_id
        self.device_mac = device_mac.replace(":", "").upper()
        self.model = model
        self._session: Optional[aiohttp.ClientSession] = None

    async def _get_session(self) -> 'aiohttp.ClientSession':
        import aiohttp
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession()
        return self._session

    async def close(self):
        if self._session and not self._session.closed:
            await self._session.close()

    def _headers(self) -> Dict:
        import time
        return {
            "jwt-token": self.jwt_token,
            "auth-type": "jwt",
            "app-id": "com.cleargrass.app.Air",
            "app-version": "3.0.1",
            "app-platform": "ios",
            "app-temp-unit": "F",
            "app-tvoc-unit": "mg/m3",
            "app-timezone": "America/Los_Angeles",
            "app-lang": "en",
            "app-reading-standard": "us",
            "app-timestamp": str(int(time.time())),
            "Accept": "*/*",
        }

    async def get_device_list(self) -> List[Dict]:
        """Get list of paired devices."""
        session = await self._get_session()
        async with session.get(
            f"{self.BASE_URL}/pair/list",
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                raise Exception(f"Failed to get device list: {resp.status}")
            data = await resp.json()
            return data.get("data", [])

    async def get_current_reading(self) -> QingpingReading:
        """Get current sensor reading for the device."""
        session = await self._get_session()
        async with session.get(
            f"{self.BASE_URL}/pair/deviceDataLogs",
            params={
                "hour": 1,
                "id": self.device_id,
                "mac": self.device_mac,
                "model": self.model,
            },
            headers=self._headers()
        ) as resp:
            if resp.status != 200:
                raise Exception(f"Failed to get device data: {resp.status}")
            data = await resp.json()

            # Parse the most recent reading
            readings_data = data.get("data", {})

            # Get the latest values from the response
            temp_c = None
            humidity = None
            co2_ppm = None
            pm25 = None
            tvoc = None

            # Data comes as arrays of readings, get the last one
            if "temperature" in readings_data and readings_data["temperature"]:
                temp_c = readings_data["temperature"][-1].get("value")
            if "humidity" in readings_data and readings_data["humidity"]:
                humidity = readings_data["humidity"][-1].get("value")
            if "co2" in readings_data and readings_data["co2"]:
                co2_ppm = readings_data["co2"][-1].get("value")
            if "pm25" in readings_data and readings_data["pm25"]:
                pm25 = readings_data["pm25"][-1].get("value")
            if "tvoc" in readings_data and readings_data["tvoc"]:
                tvoc = readings_data["tvoc"][-1].get("value")

            return QingpingReading(
                device_name=f"Qingping {self.model}",
                mac_hint=self.device_mac,
                temp_c=temp_c,
                humidity=humidity,
                co2_ppm=co2_ppm,
                pm25=pm25,
                tvoc=tvoc,
            )


# Convenience function for quick readings
async def scan_all_qingping(timeout: float = 10.0) -> List[QingpingReading]:
    """Scan and return all Qingping BLE device readings."""
    client = QingpingBLEClient()
    return await client.scan(timeout)


if __name__ == "__main__":
    import sys

    logging.basicConfig(level=logging.INFO)

    async def main():
        print("Scanning for Qingping devices...")
        readings = await scan_all_qingping(timeout=15)

        if not readings:
            print("No Qingping devices found")
            return

        print(f"\nFound {len(readings)} device(s):\n")
        for r in readings:
            print(f"  {r.device_name}")
            print(f"    MAC hint: {r.mac_hint}")
            if r.temp_c is not None:
                print(f"    Temperature: {r.temp_c}°C")
            if r.humidity is not None:
                print(f"    Humidity: {r.humidity}%")
            if r.co2_ppm is not None:
                print(f"    CO2: {r.co2_ppm} ppm")
            if r.pm25 is not None:
                print(f"    PM2.5: {r.pm25}")
            if r.tvoc is not None:
                print(f"    tVOC: {r.tvoc}")
            print()

    asyncio.run(main())
