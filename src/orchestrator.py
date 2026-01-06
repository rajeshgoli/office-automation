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

from datetime import datetime
from .config import load_config, Config
from .yolink_client import YoLinkClient, YoLinkDevice, DeviceType
from .state_machine import StateMachine, StateConfig, OccupancyState
from .qingping_client import QingpingMQTTClient, QingpingReading
from .erv_client import ERVClient, FanSpeed
from .kumo_client import KumoClient, OperationMode as HVACMode
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

        # Kumo client (Mitsubishi HVAC)
        self.kumo: Optional[KumoClient] = None
        if config.mitsubishi.username and config.mitsubishi.password:
            self.kumo = KumoClient(
                username=config.mitsubishi.username,
                password=config.mitsubishi.password,
                device_serial=config.mitsubishi.device_serial,
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

        # HVAC state tracking
        self._hvac_mode: str = "off"  # heat, cool, off, auto
        self._hvac_setpoint_c: float = 22.0  # Celsius
        self._hvac_suspended: bool = False  # True when we turned off HVAC due to ERV running
        self._hvac_last_mode: str = "heat"  # Mode before we suspended it

        # Manual override tracking
        self._manual_erv_override: bool = False
        self._manual_erv_speed: Optional[str] = None  # "off", "quiet", "medium", "turbo"
        self._manual_erv_override_at: Optional[datetime] = None
        self._manual_hvac_override: bool = False
        self._manual_hvac_mode: Optional[str] = None  # "off", "heat"
        self._manual_hvac_setpoint_f: Optional[float] = None
        self._manual_hvac_override_at: Optional[datetime] = None
        self._manual_override_timeout: int = 30 * 60  # 30 minutes default

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

    def _check_manual_override_expiry(self):
        """Check if manual overrides have expired and clear them."""
        now = datetime.now()
        if self._manual_erv_override and self._manual_erv_override_at:
            elapsed = (now - self._manual_erv_override_at).total_seconds()
            if elapsed > self._manual_override_timeout:
                logger.info("Manual ERV override expired, returning to auto")
                self._manual_erv_override = False
                self._manual_erv_speed = None
                self._manual_erv_override_at = None

        if self._manual_hvac_override and self._manual_hvac_override_at:
            elapsed = (now - self._manual_hvac_override_at).total_seconds()
            if elapsed > self._manual_override_timeout:
                logger.info("Manual HVAC override expired, returning to auto")
                self._manual_hvac_override = False
                self._manual_hvac_mode = None
                self._manual_hvac_setpoint_f = None
                self._manual_hvac_override_at = None

    def _evaluate_erv_state(self):
        """Evaluate whether ERV should be on or off based on current state.

        Priority:
        1. Safety interlock (window/door open) = ERV OFF
        2. Manual override (if active and not expired)
        3. tVOC > threshold = ERV MEDIUM (both PRESENT and AWAY)
        4. CO2 logic (PRESENT: critical only, AWAY: until target)

        When multiple triggers are active:
        - PRESENT: tVOC uses MEDIUM (louder than QUIET for CO2)
        - AWAY: TURBO handles both tVOC and CO2
        """
        # Check for expired manual overrides
        self._check_manual_override_expiry()

        state = self.state_machine.state
        co2 = self.state_machine.sensors.co2_ppm

        # Get tVOC reading
        reading = self.qingping.latest_reading
        tvoc = reading.tvoc if reading else None
        tvoc_threshold = self.config.thresholds.tvoc_threshold_ppb

        # Safety: window/door open = ERV off (overrides everything including manual)
        if self.state_machine.sensors.window_open or self.state_machine.sensors.door_open:
            if self._erv_running:
                logger.info("ACTION: ERV OFF (window/door open)")
                self.erv.turn_off()
                self._erv_running = False
                if self._tvoc_ventilation_active:
                    self._tvoc_ventilation_active = False
                    self.db.log_climate_action("erv", "off", co2_ppm=co2, reason="safety_interlock")
            return

        # Manual override takes priority over automation
        if self._manual_erv_override:
            target_speed = self._manual_erv_speed
            if target_speed == "off":
                if self._erv_running:
                    logger.info("ACTION: ERV OFF (manual override)")
                    self.erv.turn_off()
                    self._erv_running = False
            else:
                speed_map = {"quiet": FanSpeed.QUIET, "medium": FanSpeed.MEDIUM, "turbo": FanSpeed.TURBO}
                fan_speed = speed_map.get(target_speed, FanSpeed.QUIET)
                if not self._erv_running:
                    logger.info(f"ACTION: ERV {target_speed.upper()} (manual override)")
                    self.erv.turn_on(fan_speed)
                    self._erv_running = True
            return  # Skip automation logic when manual override is active

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

        # After ERV state changes, evaluate HVAC coordination
        try:
            asyncio.get_event_loop().create_task(self._evaluate_hvac_state())
        except RuntimeError:
            pass  # No event loop running

    def _is_within_occupancy_hours(self) -> bool:
        """Check if current time is within expected occupancy hours."""
        now = datetime.now().time()
        try:
            start = datetime.strptime(self.config.thresholds.expected_occupancy_start, "%H:%M").time()
            end = datetime.strptime(self.config.thresholds.expected_occupancy_end, "%H:%M").time()
            return start <= now <= end
        except ValueError:
            logger.warning("Invalid occupancy hours config, defaulting to 7AM-10PM")
            return 7 <= now.hour < 22

    def _get_temp_f(self) -> Optional[float]:
        """Get current temperature in Fahrenheit from Qingping."""
        reading = self.qingping.latest_reading
        if reading and reading.temp_c is not None:
            return reading.temp_c * 9 / 5 + 32
        return None

    async def _evaluate_hvac_state(self):
        """Evaluate HVAC state based on ERV coordination rules.

        When AWAY and ERV is running aggressively:
        - If temp > hvac_min_temp_f: suspend heating (don't heat vented air)
        - Resume heating when ERV stops or we return to PRESENT

        Always heat if temp < hvac_critical_temp_f (pipe freeze protection)
        """
        if not self.kumo:
            return  # No HVAC control available

        state = self.state_machine.state
        temp_f = self._get_temp_f()
        min_temp = self.config.thresholds.hvac_min_temp_f
        critical_temp = self.config.thresholds.hvac_critical_temp_f

        # PRESENT mode: restore HVAC if we suspended it (and it was actually running)
        if state == OccupancyState.PRESENT:
            if self._hvac_suspended and self._hvac_last_mode in ("heat", "cool", "auto"):
                logger.info(f"ACTION: HVAC RESTORE (returned to present, was {self._hvac_last_mode})")
                try:
                    if self._hvac_last_mode == "heat":
                        await self.kumo.set_heat(self._hvac_setpoint_c)
                    elif self._hvac_last_mode == "cool":
                        await self.kumo.set_cool(self._hvac_setpoint_c)
                    self._hvac_mode = self._hvac_last_mode
                    self.db.log_climate_action("hvac", self._hvac_last_mode,
                                               setpoint=self._hvac_setpoint_c,
                                               reason="present_restore")
                except Exception as e:
                    logger.error(f"Failed to restore HVAC: {e}")
            self._hvac_suspended = False  # Clear flag regardless
            return

        # AWAY mode: coordinate with ERV
        if state == OccupancyState.AWAY:
            # Critical temp protection - always heat
            if temp_f is not None and temp_f < critical_temp:
                if self._hvac_suspended or self._hvac_mode == "off":
                    logger.info(f"ACTION: HVAC HEAT (critical temp: {temp_f:.1f}°F < {critical_temp}°F)")
                    try:
                        await self.kumo.set_heat(self._hvac_setpoint_c)
                        self._hvac_mode = "heat"
                        self._hvac_suspended = False
                        self.db.log_climate_action("hvac", "heat",
                                                   setpoint=self._hvac_setpoint_c,
                                                   reason=f"critical_temp_{temp_f:.0f}F")
                    except Exception as e:
                        logger.error(f"Failed to turn on HVAC: {e}")
                return

            # ERV running + temp acceptable = suspend heating
            if self._erv_running and temp_f is not None and temp_f > min_temp:
                if not self._hvac_suspended and self._hvac_mode in ("heat", "auto"):
                    logger.info(f"ACTION: HVAC SUSPEND (ERV running, temp {temp_f:.1f}°F > {min_temp}°F)")
                    try:
                        # Remember current mode before turning off
                        self._hvac_last_mode = self._hvac_mode
                        await self.kumo.turn_off()
                        self._hvac_mode = "off"
                        self._hvac_suspended = True
                        self.db.log_climate_action("hvac", "off",
                                                   reason=f"erv_running_temp_{temp_f:.0f}F")
                    except Exception as e:
                        logger.error(f"Failed to suspend HVAC: {e}")
                return

            # ERV stopped + we suspended HVAC + within occupancy hours = restore
            if not self._erv_running and self._hvac_suspended:
                if self._is_within_occupancy_hours() and self._hvac_last_mode in ("heat", "cool", "auto"):
                    logger.info(f"ACTION: HVAC RESTORE (ERV stopped, within occupancy hours)")
                    try:
                        if self._hvac_last_mode == "heat":
                            await self.kumo.set_heat(self._hvac_setpoint_c)
                        elif self._hvac_last_mode == "cool":
                            await self.kumo.set_cool(self._hvac_setpoint_c)
                        self._hvac_mode = self._hvac_last_mode
                        self._hvac_suspended = False
                        self.db.log_climate_action("hvac", self._hvac_last_mode,
                                                   setpoint=self._hvac_setpoint_c,
                                                   reason="erv_stopped_occupancy_hours")
                    except Exception as e:
                        logger.error(f"Failed to restore HVAC: {e}")
                else:
                    temp_str = f"{temp_f:.1f}°F" if temp_f else "unknown"
                    logger.info(f"HVAC stays off (outside occupancy hours or was off, temp {temp_str})")

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

        # Evaluate HVAC coordination
        try:
            asyncio.get_event_loop().create_task(self._evaluate_hvac_state())
        except RuntimeError:
            pass

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

    async def _handle_erv_post(self, request: web.Request) -> web.Response:
        """Handle POST /erv for manual ERV control.

        Body: {"speed": "off|quiet|medium|turbo"}
        """
        try:
            data = await request.json()
            speed = data.get("speed", "").lower()

            if speed not in ("off", "quiet", "medium", "turbo"):
                return web.json_response(
                    {"ok": False, "error": f"Invalid speed: {speed}. Must be off|quiet|medium|turbo"},
                    status=400
                )

            # Get current CO2 for logging
            reading = self.qingping.latest_reading
            co2 = reading.co2_ppm if reading else None

            # Set manual override
            self._manual_erv_override = True
            self._manual_erv_speed = speed
            self._manual_erv_override_at = datetime.now()

            # Apply the change immediately
            if speed == "off":
                logger.info("MANUAL: ERV OFF")
                self.erv.turn_off()
                self._erv_running = False
            else:
                speed_map = {"quiet": FanSpeed.QUIET, "medium": FanSpeed.MEDIUM, "turbo": FanSpeed.TURBO}
                fan_speed = speed_map[speed]
                logger.info(f"MANUAL: ERV {speed.upper()}")
                self.erv.turn_on(fan_speed)
                self._erv_running = True

            # Log to database
            self.db.log_climate_action("erv", speed, co2_ppm=co2, reason="manual_override")

            # Broadcast status update
            await self._broadcast_status()

            return web.json_response({
                "ok": True,
                "erv": {
                    "speed": speed,
                    "running": self._erv_running,
                    "manual_override": True,
                    "expires_in": self._manual_override_timeout
                }
            })

        except Exception as e:
            logger.error(f"Error handling ERV POST: {e}")
            return web.json_response({"ok": False, "error": str(e)}, status=400)

    async def _handle_hvac_post(self, request: web.Request) -> web.Response:
        """Handle POST /hvac for manual HVAC control.

        Body: {"mode": "off|heat", "setpoint_f": 70}
        """
        try:
            data = await request.json()
            mode = data.get("mode", "").lower()
            setpoint_f = data.get("setpoint_f", 70)

            if mode not in ("off", "heat"):
                return web.json_response(
                    {"ok": False, "error": f"Invalid mode: {mode}. Must be off|heat"},
                    status=400
                )

            if not self.kumo:
                return web.json_response(
                    {"ok": False, "error": "HVAC (Kumo) not configured or unavailable"},
                    status=503
                )

            # Convert F to C
            setpoint_c = (setpoint_f - 32) * 5 / 9

            # Set manual override
            self._manual_hvac_override = True
            self._manual_hvac_mode = mode
            self._manual_hvac_setpoint_f = setpoint_f
            self._manual_hvac_override_at = datetime.now()

            # Apply the change
            if mode == "off":
                logger.info("MANUAL: HVAC OFF")
                await self.kumo.turn_off()
                self._hvac_mode = "off"
            else:
                logger.info(f"MANUAL: HVAC HEAT {setpoint_f}°F ({setpoint_c:.1f}°C)")
                await self.kumo.set_heat(setpoint_c)
                self._hvac_mode = "heat"
                self._hvac_setpoint_c = setpoint_c

            # Log to database
            self.db.log_climate_action("hvac", mode, setpoint=setpoint_c, reason="manual_override")

            # Broadcast status update
            await self._broadcast_status()

            return web.json_response({
                "ok": True,
                "hvac": {
                    "mode": mode,
                    "setpoint_f": setpoint_f,
                    "setpoint_c": round(setpoint_c, 1),
                    "manual_override": True,
                    "expires_in": self._manual_override_timeout
                }
            })

        except Exception as e:
            logger.error(f"Error handling HVAC POST: {e}")
            return web.json_response({"ok": False, "error": str(e)}, status=400)

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
            "speed": self._manual_erv_speed if self._manual_erv_override else ("off" if not self._erv_running else "auto"),
        }

        # Add HVAC status
        sm_status["hvac"] = {
            "mode": self._hvac_mode,
            "setpoint_c": self._hvac_setpoint_c,
            "suspended": self._hvac_suspended,
        }

        # Add manual override status
        sm_status["manual_override"] = {
            "erv": self._manual_erv_override,
            "erv_speed": self._manual_erv_speed,
            "erv_expires_in": int(self._manual_override_timeout - (datetime.now() - self._manual_erv_override_at).total_seconds()) if self._manual_erv_override and self._manual_erv_override_at else None,
            "hvac": self._manual_hvac_override,
            "hvac_mode": self._manual_hvac_mode,
            "hvac_setpoint_f": self._manual_hvac_setpoint_f,
            "hvac_expires_in": int(self._manual_override_timeout - (datetime.now() - self._manual_hvac_override_at).total_seconds()) if self._manual_hvac_override and self._manual_hvac_override_at else None,
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
        self._app.router.add_post("/erv", self._handle_erv_post)
        self._app.router.add_post("/hvac", self._handle_hvac_post)

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

        # Connect to Kumo (Mitsubishi HVAC)
        if self.kumo:
            logger.info("Connecting to Kumo Cloud...")
            try:
                status = await self.kumo.get_device_status()
                # Parse current mode and setpoint from status
                if status:
                    mode = status.get("operationMode", "off")
                    self._hvac_mode = mode
                    self._hvac_last_mode = mode if mode != "off" else "heat"
                    # Get setpoint based on mode
                    if mode == "heat":
                        self._hvac_setpoint_c = status.get("spHeat", 22.0)
                    elif mode == "cool":
                        self._hvac_setpoint_c = status.get("spCool", 24.0)
                    logger.info(f"Kumo connected. Mode: {mode}, Setpoint: {self._hvac_setpoint_c}°C")
            except Exception as e:
                logger.error(f"Kumo connection failed: {e}")
                logger.warning("Continuing without HVAC control...")
                self.kumo = None  # Disable Kumo on failure

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
                # Also update state machine so erv_should_run works correctly
                if reading.co2_ppm is not None:
                    self.state_machine.sensors.co2_ppm = reading.co2_ppm
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
        if self.kumo:
            await self.kumo.close()
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
