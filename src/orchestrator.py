"""
Office Climate Automation Orchestrator

Ties together:
- YoLink sensors (door, window, motion)
- macOS occupancy detector (via HTTP)
- Qingping Air Monitor (CO2, temp, humidity via MQTT)
- ERV control (Tuya local API)
- State machine (PRESENT/AWAY)
"""

import asyncio
import json
import logging
from typing import Optional, Set

from aiohttp import web, WSMsgType

from .config import load_config, Config
from .yolink_client import YoLinkClient, YoLinkDevice, DeviceType
from .state_machine import StateMachine, StateConfig, OccupancyState
from .qingping_client import QingpingMQTTClient, QingpingReading
from .erv_client import ERVClient, FanSpeed
from .database import Database

logger = logging.getLogger(__name__)


class Orchestrator:
    """Main orchestrator for office climate automation."""

    def __init__(self, config: Config):
        self.config = config

        # State machine
        self.state_machine = StateMachine(StateConfig(
            motion_timeout_seconds=config.thresholds.motion_timeout_seconds,
            co2_critical_ppm=config.thresholds.co2_critical_ppm,
            co2_refresh_target_ppm=config.thresholds.co2_refresh_target_ppm,
        ))

        # YoLink client
        self.yolink = YoLinkClient(config.yolink)

        # Qingping Air Monitor (CO2/temp/humidity via local MQTT)
        self.qingping = QingpingMQTTClient(
            device_mac=config.qingping.device_mac,
            mqtt_host=config.qingping.mqtt_broker,
            mqtt_port=config.qingping.mqtt_port,
        )

        # ERV client (Tuya local)
        self.erv = ERVClient(
            device_id=config.erv.device_id,
            ip=config.erv.ip,
            local_key=config.erv.local_key,
        )

        # Device name mapping (will be populated from YoLink)
        self._door_device_id: Optional[str] = None
        self._window_device_id: Optional[str] = None
        self._motion_device_id: Optional[str] = None

        # HTTP server
        self._app: Optional[web.Application] = None
        self._runner: Optional[web.AppRunner] = None

        # WebSocket connections
        self._ws_clients: Set[web.WebSocketResponse] = set()

        # Track if ERV is currently running
        self._erv_running: bool = False

        # Track tVOC-triggered ventilation (separate from CO2-based logic)
        self._tvoc_ventilation_active: bool = False

        # HVAC status (will be updated by Kumo integration)
        self._hvac_mode: str = "heat"  # heat, cool, off, auto
        self._hvac_setpoint_c: float = 22.0  # Celsius

        # Database for persistence and analysis
        self.db = Database()

    def _setup_yolink_handlers(self):
        """Map YoLink devices to state machine inputs."""
        # Find devices by type/name
        for device in self.yolink.devices.values():
            name_lower = device.name.lower()

            if device.device_type == DeviceType.MOTION_SENSOR:
                self._motion_device_id = device.device_id
                logger.info(f"Motion sensor: {device.name}")

            elif device.device_type == DeviceType.DOOR_SENSOR:
                if "door" in name_lower:
                    self._door_device_id = device.device_id
                    logger.info(f"Door sensor: {device.name}")
                elif "window" in name_lower:
                    self._window_device_id = device.device_id
                    logger.info(f"Window sensor: {device.name}")

        # Register event handler
        self.yolink.on_event(self._handle_yolink_event)

    async def _handle_yolink_event(self, device: YoLinkDevice, event_data: dict):
        """Handle YoLink sensor events."""
        state = event_data.get("state")

        if device.device_id == self._door_device_id:
            is_open = state == "open"
            logger.info(f"Door: {'OPEN' if is_open else 'CLOSED'}")
            self.db.log_device_event("door", "open" if is_open else "closed", device.name)
            await self.state_machine.update_door(is_open)

        elif device.device_id == self._window_device_id:
            is_open = state == "open"
            logger.info(f"Window: {'OPEN' if is_open else 'CLOSED'}")
            self.db.log_device_event("window", "open" if is_open else "closed", device.name)
            await self.state_machine.update_window(is_open)

        elif device.device_id == self._motion_device_id:
            detected = state == "alert"
            logger.info(f"Motion: {'DETECTED' if detected else 'clear'}")
            self.db.log_device_event("motion", "detected" if detected else "clear", device.name)
            await self.state_machine.update_motion(detected)

        # Log current status
        status = self.state_machine.get_status()
        logger.info(f"State: {status['state'].upper()} | ERV should run: {status['erv_should_run']}")

        # Broadcast to WebSocket clients
        await self._broadcast_status()

    def _on_qingping_reading(self, reading: QingpingReading):
        """Handle new air quality reading from Qingping."""
        logger.info(f"Air quality: CO2={reading.co2_ppm}ppm, {reading.temp_c}°C, {reading.humidity}%")

        # Log to database for persistence and analysis
        self.db.log_sensor_reading(
            co2_ppm=reading.co2_ppm,
            temp_c=reading.temp_c,
            humidity=reading.humidity,
            pm25=reading.pm25,
            pm10=reading.pm10,
            tvoc=reading.tvoc,
        )

        # Update state machine with CO2 reading
        if reading.co2_ppm is not None:
            self.state_machine.update_co2(reading.co2_ppm)

        # Check if we need to adjust ERV based on new CO2 reading
        self._evaluate_erv_state()

        # Broadcast to WebSocket clients (schedule async call)
        try:
            asyncio.get_event_loop().create_task(self._broadcast_status())
        except RuntimeError:
            pass  # No event loop running

    def _evaluate_erv_state(self):
        """Evaluate whether ERV should be on or off based on current state.

        Priority:
        1. Safety interlock (window/door open) = ERV OFF
        2. tVOC > threshold = ERV MEDIUM (both PRESENT and AWAY)
        3. CO2 logic (PRESENT: critical only, AWAY: until target)

        When multiple triggers are active:
        - PRESENT: tVOC uses MEDIUM (louder than QUIET for CO2)
        - AWAY: TURBO handles both tVOC and CO2
        """
        state = self.state_machine.state
        co2 = self.state_machine.sensors.co2_ppm

        # Get tVOC reading
        reading = self.qingping.latest_reading
        tvoc = reading.tvoc if reading else None
        tvoc_threshold = self.config.thresholds.tvoc_threshold_ppb

        # Safety: window/door open = ERV off
        if self.state_machine.sensors.window_open or self.state_machine.sensors.door_open:
            if self._erv_running:
                logger.info("ACTION: ERV OFF (window/door open)")
                self.erv.turn_off()
                self._erv_running = False
                if self._tvoc_ventilation_active:
                    self._tvoc_ventilation_active = False
                    self.db.log_climate_action("erv", "off", co2_ppm=co2, reason="safety_interlock")
            return

        # Determine what's triggering ventilation
        tvoc_triggered = tvoc is not None and tvoc > tvoc_threshold
        co2_critical = co2 is not None and co2 >= self.config.thresholds.co2_critical_ppm
        co2_needs_refresh = co2 is not None and co2 > self.config.thresholds.co2_refresh_target_ppm

        if state == OccupancyState.PRESENT:
            # PRESENT mode: prioritize quiet operation
            # tVOC triggers MEDIUM, CO2 critical triggers QUIET
            # tVOC takes precedence (MEDIUM is louder but handles VOCs)
            if tvoc_triggered:
                if not self._erv_running or not self._tvoc_ventilation_active:
                    logger.info(f"ACTION: ERV MEDIUM (tVOC high: {tvoc}ppb)")
                    self.erv.turn_on(FanSpeed.MEDIUM)
                    self._erv_running = True
                    if not self._tvoc_ventilation_active:
                        self._tvoc_ventilation_active = True
                        self.db.log_climate_action("erv", "medium", co2_ppm=co2, reason=f"tvoc_high_{tvoc}ppb")
            elif co2_critical:
                if not self._erv_running:
                    logger.info(f"ACTION: ERV QUIET (CO2 critical: {co2}ppm)")
                    self.erv.turn_on(FanSpeed.QUIET)
                    self._erv_running = True
                elif self._tvoc_ventilation_active:
                    # tVOC cleared, but CO2 still critical - downgrade to QUIET
                    logger.info(f"ACTION: ERV QUIET (tVOC OK, CO2 still critical: {co2}ppm)")
                    self.erv.turn_on(FanSpeed.QUIET)
                    self._tvoc_ventilation_active = False
                    self.db.log_climate_action("erv", "quiet", co2_ppm=co2, reason="tvoc_cleared_co2_critical")
            else:
                if self._erv_running:
                    logger.info("ACTION: ERV OFF (present, air quality OK)")
                    self.erv.turn_off()
                    self._erv_running = False
                    if self._tvoc_ventilation_active:
                        self._tvoc_ventilation_active = False
                        self.db.log_climate_action("erv", "off", co2_ppm=co2, reason="tvoc_cleared")

        elif state == OccupancyState.AWAY:
            # AWAY mode: aggressive ventilation
            # CO2 refresh uses TURBO, tVOC-only uses MEDIUM
            if co2_needs_refresh:
                # TURBO handles both CO2 and tVOC
                if not self._erv_running:
                    logger.info(f"ACTION: ERV TURBO (away mode, CO2={co2}ppm)")
                    self.erv.turn_on(FanSpeed.TURBO)
                    self._erv_running = True
            elif tvoc_triggered:
                # CO2 is good but tVOC needs clearing
                if not self._erv_running or not self._tvoc_ventilation_active:
                    logger.info(f"ACTION: ERV MEDIUM (away, tVOC high: {tvoc}ppb, CO2 OK)")
                    self.erv.turn_on(FanSpeed.MEDIUM)
                    self._erv_running = True
                    if not self._tvoc_ventilation_active:
                        self._tvoc_ventilation_active = True
                        self.db.log_climate_action("erv", "medium", co2_ppm=co2, reason=f"tvoc_high_{tvoc}ppb_away")
            else:
                if self._erv_running:
                    reason = "co2_target_reached" if not self._tvoc_ventilation_active else "tvoc_cleared_away"
                    logger.info(f"ACTION: ERV OFF ({reason}: CO2={co2}ppm)")
                    self.erv.turn_off()
                    self._erv_running = False
                    if self._tvoc_ventilation_active:
                        self._tvoc_ventilation_active = False
                        self.db.log_climate_action("erv", "off", co2_ppm=co2, reason=reason)

    def _on_state_change(self, old_state: OccupancyState, new_state: OccupancyState):
        """Handle occupancy state changes."""
        logger.info(f"=== STATE CHANGE: {old_state.value} → {new_state.value} ===")

        # Get latest CO2 reading
        reading = self.qingping.latest_reading
        co2 = reading.co2_ppm if reading else None
        logger.info(f"Current CO2: {co2}ppm" if co2 else "CO2: unknown")

        # Log to database
        self.db.log_occupancy_change(
            state=new_state.value,
            trigger=None,  # TODO: track what triggered the change
            co2_ppm=co2,
        )

        # Evaluate ERV state based on new occupancy
        self._evaluate_erv_state()

        # Broadcast to WebSocket clients (schedule async call)
        try:
            asyncio.get_event_loop().create_task(self._broadcast_status())
        except RuntimeError:
            pass  # No event loop running

    async def update_mac_occupancy(self, active: bool, external_monitor: bool):
        """Update from macOS occupancy detector."""
        await self.state_machine.update_mac_occupancy(active, external_monitor)

    # --- HTTP Server ---

    async def _handle_occupancy_post(self, request: web.Request) -> web.Response:
        """Handle POST /occupancy from macOS detector."""
        try:
            data = await request.json()
            active = data.get("active", False)
            external_monitor = data.get("external_monitor", False)

            logger.info(f"Mac occupancy update: active={active}, monitor={external_monitor}")
            await self.update_mac_occupancy(active, external_monitor)

            # Broadcast to WebSocket clients
            await self._broadcast_status()

            status = self.state_machine.get_status()
            return web.json_response({
                "ok": True,
                "state": status["state"],
                "erv_should_run": status["erv_should_run"]
            })
        except Exception as e:
            logger.error(f"Error handling occupancy POST: {e}")
            return web.json_response({"ok": False, "error": str(e)}, status=400)

    async def _handle_status_get(self, request: web.Request) -> web.Response:
        """Handle GET /status for debugging."""
        return web.json_response(self._get_status_dict())

    def _get_status_dict(self) -> dict:
        """Get current status as a dictionary."""
        sm_status = self.state_machine.get_status()

        # Add air quality info
        reading = self.qingping.latest_reading
        sm_status["air_quality"] = {
            "co2_ppm": reading.co2_ppm if reading else None,
            "temp_c": reading.temp_c if reading else None,
            "humidity": reading.humidity if reading else None,
            "pm25": reading.pm25 if reading else None,
            "tvoc": reading.tvoc if reading else None,
            "last_update": reading.timestamp.isoformat() if reading and reading.timestamp else None,
        }

        # Add ERV status
        sm_status["erv"] = {
            "running": self._erv_running,
            "tvoc_ventilation": self._tvoc_ventilation_active,
        }

        # Add HVAC status
        sm_status["hvac"] = {
            "mode": self._hvac_mode,
            "setpoint_c": self._hvac_setpoint_c,
        }

        return sm_status

    async def _broadcast_status(self):
        """Broadcast current status to all WebSocket clients."""
        if not self._ws_clients:
            return

        status = self._get_status_dict()
        message = json.dumps(status)

        # Send to all connected clients
        closed = set()
        for ws in self._ws_clients:
            try:
                await ws.send_str(message)
            except Exception as e:
                logger.debug(f"WebSocket send error: {e}")
                closed.add(ws)

        # Remove closed connections
        self._ws_clients -= closed

    async def _handle_websocket(self, request: web.Request) -> web.WebSocketResponse:
        """Handle WebSocket connections for real-time updates."""
        ws = web.WebSocketResponse()
        await ws.prepare(request)

        self._ws_clients.add(ws)
        logger.info(f"WebSocket client connected ({len(self._ws_clients)} total)")

        # Send current status immediately
        try:
            status = self._get_status_dict()
            await ws.send_str(json.dumps(status))
        except Exception as e:
            logger.error(f"Error sending initial status: {e}")

        try:
            async for msg in ws:
                if msg.type == WSMsgType.TEXT:
                    # Client can send 'ping' to keep connection alive
                    if msg.data == 'ping':
                        await ws.send_str('pong')
                elif msg.type == WSMsgType.ERROR:
                    logger.error(f"WebSocket error: {ws.exception()}")
        finally:
            self._ws_clients.discard(ws)
            logger.info(f"WebSocket client disconnected ({len(self._ws_clients)} total)")

        return ws

    @staticmethod
    @web.middleware
    async def _cors_middleware(request: web.Request, handler):
        """Add CORS headers to all responses."""
        if request.method == "OPTIONS":
            # Handle preflight requests
            response = web.Response()
        else:
            response = await handler(request)

        response.headers["Access-Control-Allow-Origin"] = "*"
        response.headers["Access-Control-Allow-Methods"] = "GET, POST, OPTIONS"
        response.headers["Access-Control-Allow-Headers"] = "Content-Type"
        return response

    async def _start_http_server(self):
        """Start the HTTP server for macOS occupancy detector."""
        self._app = web.Application(middlewares=[self._cors_middleware])
        self._app.router.add_post("/occupancy", self._handle_occupancy_post)
        self._app.router.add_get("/status", self._handle_status_get)
        self._app.router.add_get("/ws", self._handle_websocket)

        self._runner = web.AppRunner(self._app)
        await self._runner.setup()

        host = self.config.orchestrator.host
        port = self.config.orchestrator.port
        site = web.TCPSite(self._runner, host, port)
        await site.start()
        logger.info(f"HTTP server listening on http://{host}:{port}")

    async def _stop_http_server(self):
        """Stop the HTTP server."""
        if self._runner:
            await self._runner.cleanup()
            logger.info("HTTP server stopped")

    async def start(self):
        """Start the orchestrator."""
        logger.info("Starting Office Climate Automation...")

        # Register state change handler
        self.state_machine.on_state_change(self._on_state_change)

        # Connect to ERV
        logger.info("Connecting to ERV...")
        try:
            self.erv.connect()
            status = self.erv.get_status()
            self._erv_running = status.power
            logger.info(f"ERV connected. Power: {status.power}, Speed: {status.fan_speed}")
        except Exception as e:
            logger.error(f"ERV connection failed: {e}")
            logger.warning("Continuing without ERV control...")

        # Connect to Qingping MQTT
        logger.info("Connecting to Qingping MQTT...")
        try:
            self.qingping.set_callback(self._on_qingping_reading)
            self.qingping.connect()
            logger.info("Qingping MQTT connected. Waiting for sensor data...")

            # Restore last reading from database (survives restarts)
            cached = self.db.get_latest_sensor_reading()
            if cached and cached.get("co2_ppm"):
                from datetime import datetime
                reading = QingpingReading(
                    device_name="Qingping Air Monitor (cached)",
                    mac_hint=self.config.qingping.device_mac,
                    co2_ppm=cached.get("co2_ppm"),
                    temp_c=cached.get("temp_c"),
                    humidity=cached.get("humidity"),
                    pm25=cached.get("pm25"),
                    pm10=cached.get("pm10"),
                    tvoc=cached.get("tvoc"),
                    timestamp=datetime.fromisoformat(cached["timestamp"]) if cached.get("timestamp") else None,
                )
                self.qingping._latest_reading = reading
                logger.info(f"Restored cached reading: CO2={reading.co2_ppm}ppm from {cached.get('timestamp')}")
        except Exception as e:
            logger.error(f"Qingping MQTT connection failed: {e}")
            logger.warning("Continuing without CO2 monitoring...")

        # Start HTTP server
        await self._start_http_server()

        # Start YoLink
        logger.info("Connecting to YoLink...")
        await self.yolink.start()

        # Map devices
        self._setup_yolink_handlers()

        logger.info("Orchestrator running. Waiting for events...")

    async def stop(self):
        """Stop the orchestrator."""
        # Turn off ERV before stopping
        if self._erv_running:
            logger.info("Turning off ERV before shutdown...")
            self.erv.turn_off()

        # Disconnect clients
        self.qingping.disconnect()
        await self._stop_http_server()
        await self.yolink.stop()
        logger.info("Orchestrator stopped.")

    async def run_forever(self):
        """Run until interrupted."""
        await self.start()
        try:
            while True:
                await asyncio.sleep(1)
        except asyncio.CancelledError:
            pass
        finally:
            await self.stop()


async def main(port: int = None):
    """Run the orchestrator."""
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )

    config = load_config()
    if port:
        config.orchestrator.port = port
    orchestrator = Orchestrator(config)

    try:
        await orchestrator.run_forever()
    except KeyboardInterrupt:
        print("\nShutting down...")


if __name__ == "__main__":
    asyncio.run(main())
