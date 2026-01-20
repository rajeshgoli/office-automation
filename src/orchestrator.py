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
import base64
import ipaddress
import secrets
from collections import deque
from pathlib import Path
from typing import Optional, Set, Dict

from aiohttp import web, WSMsgType

from datetime import datetime, timedelta
from .config import load_config, Config
from .yolink_client import YoLinkClient, YoLinkDevice, DeviceType
from .state_machine import StateMachine, StateConfig, OccupancyState
from .qingping_client import QingpingMQTTClient, QingpingReading
from .erv_client import ERVClient, FanSpeed
from .kumo_client import KumoClient, OperationMode as HVACMode
from .database import Database
from .oauth_service import OAuthService, UserSession

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
            report_interval=config.qingping.report_interval,
        )

        # ERV client (Tuya local with cloud fallback)
        self.erv = ERVClient(
            device_id=config.erv.device_id,
            ip=config.erv.ip,
            local_key=config.erv.local_key,
            cloud_api_key=config.tuya_cloud.access_id if config.tuya_cloud else None,
            cloud_api_secret=config.tuya_cloud.access_secret if config.tuya_cloud else None,
            cloud_region=config.tuya_cloud.region if config.tuya_cloud else "us",
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

        # Track if ERV is currently running and at what speed
        self._erv_running: bool = False
        self._erv_speed: str = "off"  # "off", "quiet", "medium", "turbo"

        # CO2 plateau detection state (AWAY mode optimization)
        self._co2_history: deque[tuple[datetime, int]] = deque(
            maxlen=self.config.thresholds.co2_history_size
        )
        self._outdoor_co2_baseline: Optional[int] = None  # Learned outdoor CO2 level
        self._plateau_detected: bool = False
        self._away_start_time: Optional[datetime] = None  # When we entered AWAY mode

        # tVOC AWAY mode adaptive control (separate from spike detection)
        self._tvoc_away_history: deque[tuple[datetime, int]] = deque(
            maxlen=self.config.thresholds.tvoc_away_history_size
        )
        self._tvoc_away_ventilation_active: bool = False
        self._tvoc_baseline: Optional[int] = None  # Learned baseline tVOC level
        self._tvoc_plateau_detected: bool = False

        # OAuth service (if configured)
        self.oauth: Optional[OAuthService] = None
        if config.orchestrator.google_oauth:
            oauth_config = config.orchestrator.google_oauth
            # Determine redirect URI based on host/port
            if config.orchestrator.host == "0.0.0.0":
                redirect_uri = f"http://localhost:{config.orchestrator.port}/auth/callback"
            else:
                redirect_uri = f"http://{config.orchestrator.host}:{config.orchestrator.port}/auth/callback"

            self.oauth = OAuthService(
                client_id=oauth_config.client_id,
                client_secret=oauth_config.client_secret,
                allowed_emails=oauth_config.allowed_emails,
                token_expiry_days=oauth_config.token_expiry_days,
                redirect_uri=redirect_uri,
                jwt_secret=oauth_config.jwt_secret,
                trusted_networks=oauth_config.trusted_networks
            )
            logger.info("OAuth service initialized")

        # PKCE state storage (state -> code_verifier)
        self._oauth_states: Dict[str, str] = {}

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

        # Background task for HVAC polling
        self._hvac_poll_task: Optional[asyncio.Task] = None

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
            noise_db=reading.noise_db,
        )

        # Update tVOC history for AWAY mode adaptive control
        if reading.tvoc is not None:
            now = datetime.now()
            self._tvoc_away_history.append((now, reading.tvoc))

        # Update CO2 history for plateau detection
        if reading.co2_ppm is not None:
            now = datetime.now()
            self._co2_history.append((now, reading.co2_ppm))

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

    def _calculate_co2_rate_of_change(self) -> Optional[float]:
        """Calculate CO2 rate of change in ppm/min over the history window.

        Returns:
            Rate of change in ppm/min (negative = falling, positive = rising)
            None if insufficient data
        """
        if len(self._co2_history) < 2:
            return None

        # Get oldest and newest readings
        oldest_time, oldest_co2 = self._co2_history[0]
        newest_time, newest_co2 = self._co2_history[-1]

        # Calculate time difference in minutes
        time_delta = (newest_time - oldest_time).total_seconds() / 60.0

        if time_delta == 0:
            return None

        # Calculate rate (negative = falling, positive = rising)
        co2_delta = newest_co2 - oldest_co2
        rate = co2_delta / time_delta

        return rate

    def _detect_co2_plateau(self) -> bool:
        """Detect if CO2 has plateaued (equilibrium with outdoor air).

        Plateau = rate of change < threshold for sustained period.

        Returns:
            True if plateau detected, False otherwise
        """
        if not self.config.thresholds.co2_plateau_enabled:
            return False

        # Need enough data to calculate rate over window
        min_readings = max(20, int(self.config.thresholds.co2_plateau_window_minutes * 2))
        if len(self._co2_history) < min_readings:
            return False

        # Safety: Don't declare plateau if CO2 is still high
        current_co2 = self._co2_history[-1][1]
        if current_co2 > self.config.thresholds.co2_plateau_min_co2:
            return False

        # Calculate rate of change
        rate = self._calculate_co2_rate_of_change()
        if rate is None:
            return False

        # Plateau = very slow rate (absolute value)
        rate_threshold = self.config.thresholds.co2_plateau_rate_threshold
        is_plateau = abs(rate) < rate_threshold

        if is_plateau:
            # Remember this as outdoor baseline
            self._outdoor_co2_baseline = current_co2
            logger.info(f"CO2 plateau detected at {current_co2}ppm (rate: {rate:.2f} ppm/min, "
                       f"outdoor baseline learned)")

        return is_plateau

    def _get_adaptive_erv_speed_for_away(self, co2: int) -> Optional[str]:
        """Determine adaptive ERV speed for AWAY mode based on CO2 rate of change.

        Returns:
            "turbo", "medium", "quiet", "off", or None if insufficient data
        """
        if not self.config.thresholds.co2_adaptive_speed_enabled:
            return None  # Fall back to default TURBO logic

        # Check for plateau first
        if self._detect_co2_plateau():
            logger.info(f"CO2 plateau detected at {co2}ppm, stopping ERV (outdoor baseline: {self._outdoor_co2_baseline}ppm)")
            self._plateau_detected = True
            return "off"

        # Force TURBO for first N minutes after departure
        # This ensures aggressive initial purge before switching to adaptive
        turbo_duration = self.config.thresholds.co2_turbo_duration_minutes
        if self._away_start_time:
            minutes_away = (datetime.now() - self._away_start_time).total_seconds() / 60.0
            if minutes_away < turbo_duration:
                return "turbo"  # Still in initial TURBO window

        # Calculate rate of change
        rate = self._calculate_co2_rate_of_change()
        if rate is None:
            return None  # Not enough data, use default

        # Rate is negative when falling (ventilation working)
        # We want absolute value for thresholds
        abs_rate = abs(rate)

        # Adaptive speed based on how fast CO2 is falling
        if abs_rate > self.config.thresholds.co2_rate_turbo_threshold:
            return "turbo"  # Falling fast, keep TURBO
        elif abs_rate > self.config.thresholds.co2_rate_medium_threshold:
            return "medium"  # Slowing down, step to MEDIUM
        elif abs_rate > self.config.thresholds.co2_rate_quiet_threshold:
            return "quiet"  # Very slow, step to QUIET
        else:
            # Rate < 0.5 ppm/min - approaching plateau
            # But give it a few more minutes before declaring plateau
            return "quiet"

    def _calculate_tvoc_rate_of_change(self) -> Optional[float]:
        """Calculate tVOC rate of change in points/min over the history window.

        Returns:
            Rate of change in points/min (negative = falling, positive = rising)
            None if insufficient data
        """
        if len(self._tvoc_away_history) < 2:
            return None

        # Get oldest and newest readings
        oldest_time, oldest_tvoc = self._tvoc_away_history[0]
        newest_time, newest_tvoc = self._tvoc_away_history[-1]

        # Calculate time difference in minutes
        time_delta = (newest_time - oldest_time).total_seconds() / 60.0

        if time_delta == 0:
            return None

        # Calculate rate (negative = falling, positive = rising)
        tvoc_delta = newest_tvoc - oldest_tvoc
        rate = tvoc_delta / time_delta

        return rate

    def _detect_tvoc_plateau(self) -> bool:
        """Detect if tVOC has plateaued (reached baseline).

        Plateau = rate of change < threshold for sustained period.

        Returns:
            True if plateau detected, False otherwise
        """
        if not self.config.thresholds.tvoc_away_enabled:
            return False

        # Need enough data to calculate rate over window
        min_readings = 20
        if len(self._tvoc_away_history) < min_readings:
            return False

        # Safety: Don't declare plateau if tVOC is still high
        current_tvoc = self._tvoc_away_history[-1][1]
        if current_tvoc > self.config.thresholds.tvoc_away_target + 20:  # Allow some margin
            return False

        # Calculate rate of change
        rate = self._calculate_tvoc_rate_of_change()
        if rate is None:
            return False

        # Plateau = very slow rate (absolute value)
        rate_threshold = self.config.thresholds.tvoc_plateau_rate_threshold
        is_plateau = abs(rate) < rate_threshold

        if is_plateau:
            # Remember this as baseline
            self._tvoc_baseline = current_tvoc
            logger.info(f"tVOC plateau detected at {current_tvoc} (rate: {rate:.2f} points/min, "
                       f"baseline learned)")

        return is_plateau

    def _get_adaptive_erv_speed_for_tvoc_away(self, tvoc: int) -> Optional[str]:
        """Determine adaptive ERV speed for AWAY mode based on tVOC rate of change.

        Returns:
            "turbo", "medium", "quiet", "off", or None if insufficient data
        """
        if not self.config.thresholds.tvoc_away_enabled:
            return None

        # Check for plateau first
        if self._detect_tvoc_plateau():
            logger.info(f"tVOC plateau detected at {tvoc}, stopping ERV (baseline: {self._tvoc_baseline})")
            self._tvoc_plateau_detected = True
            return "off"

        # Calculate rate of change
        rate = self._calculate_tvoc_rate_of_change()
        if rate is None:
            return None  # Not enough data, use default

        # Rate is negative when falling (ventilation working)
        # We want absolute value for thresholds
        abs_rate = abs(rate)

        # Adaptive speed based on how fast tVOC is falling
        if abs_rate > self.config.thresholds.tvoc_rate_turbo_threshold:
            return "turbo"  # Falling fast, keep TURBO
        elif abs_rate > self.config.thresholds.tvoc_rate_medium_threshold:
            return "medium"  # Slowing down, step to MEDIUM
        elif abs_rate > self.config.thresholds.tvoc_rate_quiet_threshold:
            return "quiet"  # Very slow, step to QUIET
        else:
            # Approaching plateau
            return "quiet"

    def _evaluate_erv_state(self):
        """Evaluate whether ERV should be on or off based on current state.

        Priority:
        1. Safety interlock (window/door open) = ERV OFF
        2. Manual override (if active and not expired)
        3. PRESENT: Only CO2 > 2000 triggers QUIET (tVOC IGNORED when present)
        4. AWAY: CO2 > 500 OR tVOC > 200 triggers adaptive ventilation
        """
        # Check for expired manual overrides
        self._check_manual_override_expiry()

        state = self.state_machine.state

        # Get CO2 and tVOC readings from Qingping
        reading = self.qingping.latest_reading
        co2 = reading.co2_ppm if reading else None
        tvoc = reading.tvoc if reading else None

        # Safety: window/door open = ERV off (overrides everything including manual)
        if self.state_machine.sensors.window_open or self.state_machine.sensors.door_open:
            if self._erv_running:
                logger.info("ACTION: ERV OFF (window/door open)")
                self.erv.turn_off()
                self._erv_running = False
                self._erv_speed = "off"
                self._tvoc_away_ventilation_active = False
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
                    self._erv_speed = "off"
            else:
                speed_map = {"quiet": FanSpeed.QUIET, "medium": FanSpeed.MEDIUM, "turbo": FanSpeed.TURBO}
                fan_speed = speed_map.get(target_speed, FanSpeed.QUIET)
                if not self._erv_running or self._erv_speed != target_speed:
                    logger.info(f"ACTION: ERV {target_speed.upper()} (manual override)")
                    self.erv.turn_on(fan_speed)
                    self._erv_running = True
                    self._erv_speed = target_speed
            return  # Skip automation logic when manual override is active

        # CO2 hysteresis: ON at 2000, OFF at 1800 (200ppm dead band)
        co2_critical_on = co2 is not None and co2 >= self.config.thresholds.co2_critical_ppm
        co2_critical_off = co2 is not None and co2 < (
            self.config.thresholds.co2_critical_ppm - self.config.thresholds.co2_critical_hysteresis_ppm
        )
        co2_needs_refresh = co2 is not None and co2 > self.config.thresholds.co2_refresh_target_ppm

        # tVOC AWAY mode thresholds
        tvoc_away_threshold = self.config.thresholds.tvoc_away_threshold
        tvoc_needs_clearing = tvoc is not None and tvoc > tvoc_away_threshold
        tvoc_at_target = tvoc is not None and tvoc <= self.config.thresholds.tvoc_away_target

        if state == OccupancyState.PRESENT:
            # PRESENT mode: prioritize quiet operation
            # tVOC is IGNORED when present - only CO2 > 2000 triggers ERV
            if co2_critical_on:
                # CO2 >= 2000 - turn on QUIET
                if not self._erv_running or self._erv_speed != "quiet":
                    logger.info(f"ACTION: ERV QUIET (CO2 critical: {co2}ppm)")
                    self.erv.turn_on(FanSpeed.QUIET)
                    self._erv_running = True
                    self._erv_speed = "quiet"
                    self.db.log_climate_action("erv", "quiet", co2_ppm=co2, reason=f"present_co2_critical_{co2}ppm")
            elif self._erv_running and self._erv_speed == "quiet":
                # Running QUIET mode, check hysteresis before turning off
                if co2_critical_off:
                    logger.info(f"ACTION: ERV OFF (CO2 dropped to {co2}ppm, below {self.config.thresholds.co2_critical_ppm - self.config.thresholds.co2_critical_hysteresis_ppm}ppm)")
                    self.erv.turn_off()
                    self._erv_running = False
                    self._erv_speed = "off"
                # else: stay in hysteresis band (1800-2000), keep running
            elif self._erv_running:
                # Turn off if running for any other reason
                logger.info("ACTION: ERV OFF (present, air quality OK)")
                self.erv.turn_off()
                self._erv_running = False
                self._erv_speed = "off"

        elif state == OccupancyState.AWAY:
            # AWAY mode: aggressive ventilation with adaptive speed control
            # Both CO2 > 500 and tVOC > 200 trigger ventilation
            # Use the more aggressive speed between CO2 and tVOC needs

            # Get adaptive speeds for both CO2 and tVOC
            co2_adaptive_speed = None
            tvoc_adaptive_speed = None

            if co2_needs_refresh:
                co2_adaptive_speed = self._get_adaptive_erv_speed_for_away(co2)

            if tvoc_needs_clearing or self._tvoc_away_ventilation_active:
                # Check if tVOC has reached target
                if tvoc_at_target and self._tvoc_away_ventilation_active:
                    logger.info(f"tVOC cleared to {tvoc}, ending tVOC ventilation")
                    self._tvoc_away_ventilation_active = False
                    self._tvoc_plateau_detected = False
                elif tvoc_needs_clearing or self._tvoc_away_ventilation_active:
                    tvoc_adaptive_speed = self._get_adaptive_erv_speed_for_tvoc_away(tvoc)
                    if not self._tvoc_away_ventilation_active and tvoc_needs_clearing:
                        self._tvoc_away_ventilation_active = True
                        logger.info(f"tVOC high ({tvoc}), starting adaptive ventilation")

            # Determine final speed: pick the more aggressive one
            # Speed priority: turbo > medium > quiet > off
            speed_priority = {"turbo": 3, "medium": 2, "quiet": 1, "off": 0, None: -1}
            co2_priority = speed_priority.get(co2_adaptive_speed, -1)
            tvoc_priority = speed_priority.get(tvoc_adaptive_speed, -1)

            # If both are "off" or plateau, turn off
            if co2_adaptive_speed == "off" and (tvoc_adaptive_speed == "off" or not self._tvoc_away_ventilation_active):
                if self._erv_running:
                    reason = "co2_plateau" if self._plateau_detected else "targets_reached"
                    logger.info(f"ACTION: ERV OFF ({reason}: CO2={co2}ppm, tVOC={tvoc})")
                    self.erv.turn_off()
                    self._erv_running = False
                    self._erv_speed = "off"
                    self.db.log_climate_action("erv", "off", co2_ppm=co2, reason=reason)
            elif co2_priority >= 0 or tvoc_priority >= 0:
                # Pick the more aggressive speed
                if co2_priority >= tvoc_priority:
                    target_speed = co2_adaptive_speed
                    trigger = f"CO2={co2}ppm"
                else:
                    target_speed = tvoc_adaptive_speed
                    trigger = f"tVOC={tvoc}"

                if target_speed and target_speed != "off":
                    speed_map = {"quiet": FanSpeed.QUIET, "medium": FanSpeed.MEDIUM, "turbo": FanSpeed.TURBO}
                    fan_speed = speed_map[target_speed]

                    if not self._erv_running or self._erv_speed != target_speed:
                        co2_rate = self._calculate_co2_rate_of_change()
                        tvoc_rate = self._calculate_tvoc_rate_of_change()
                        rate_str = f"CO2:{co2_rate:.2f}/min" if co2_rate else ""
                        if tvoc_rate:
                            rate_str += f" tVOC:{tvoc_rate:.2f}/min" if rate_str else f"tVOC:{tvoc_rate:.2f}/min"
                        logger.info(f"ACTION: ERV {target_speed.upper()} (away, adaptive: {trigger}, {rate_str})")
                        self.erv.turn_on(fan_speed)
                        self._erv_running = True
                        self._erv_speed = target_speed
                        self.db.log_climate_action("erv", target_speed, co2_ppm=co2,
                                                   reason=f"away_adaptive_{target_speed}_{trigger}")
            elif not co2_needs_refresh and not self._tvoc_away_ventilation_active:
                # Nothing needs ventilation
                if self._erv_running:
                    logger.info(f"ACTION: ERV OFF (air quality OK: CO2={co2}ppm, tVOC={tvoc})")
                    self.erv.turn_off()
                    self._erv_running = False
                    self._erv_speed = "off"
                    self.db.log_climate_action("erv", "off", co2_ppm=co2, reason="air_quality_ok")
            else:
                # Fall back to TURBO if adaptive not ready yet
                if not self._erv_running or self._erv_speed != "turbo":
                    trigger = f"CO2={co2}ppm" if co2_needs_refresh else f"tVOC={tvoc}"
                    logger.info(f"ACTION: ERV TURBO (away mode, {trigger})")
                    self.erv.turn_on(FanSpeed.TURBO)
                    self._erv_running = True
                    self._erv_speed = "turbo"
                    self.db.log_climate_action("erv", "turbo", co2_ppm=co2, reason=f"away_refresh_{trigger}")

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
                if not self._hvac_suspended:
                    try:
                        # Check ACTUAL current state before suspending (don't rely on stored state)
                        status = await self.kumo.get_full_status()
                        if status:
                            device_power = status.get("power", 0)
                            device_mode = status.get("operationMode", "off") if device_power == 1 else "off"

                            # Only suspend and remember state if heater is actually ON
                            if device_mode in ("heat", "cool", "auto"):
                                logger.info(f"ACTION: HVAC SUSPEND (ERV running, temp {temp_f:.1f}°F > {min_temp}°F, was {device_mode})")
                                self._hvac_last_mode = device_mode  # Save actual state
                                await self.kumo.turn_off()
                                self._hvac_mode = "off"
                                self._hvac_suspended = True
                                self.db.log_climate_action("hvac", "off",
                                                           reason=f"erv_running_temp_{temp_f:.0f}F")
                            else:
                                # Heater already off, nothing to suspend
                                logger.debug(f"HVAC already off, no suspension needed (ERV running, temp {temp_f:.1f}°F)")
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

    def _clear_manual_overrides(self, reason: str = "state_change"):
        """Clear all manual overrides (called on state transitions)."""
        if self._manual_erv_override or self._manual_hvac_override:
            logger.info(f"Clearing manual overrides: {reason}")

        self._manual_erv_override = False
        self._manual_erv_speed = None
        self._manual_erv_override_at = None
        self._manual_hvac_override = False
        self._manual_hvac_mode = None
        self._manual_hvac_setpoint_f = None
        self._manual_hvac_override_at = None

    async def _poll_hvac_status(self):
        """Background task to poll HVAC status periodically.

        This keeps our local state in sync with the actual device,
        even if the user changes settings via remote/app.
        Default interval is 10 minutes to be respectful of Mitsubishi's API.
        Pauses during night hours (11 PM - 6 AM) to avoid unnecessary API calls.
        """
        if not self.kumo:
            return

        poll_interval = self.config.mitsubishi.poll_interval_seconds

        while True:
            try:
                await asyncio.sleep(poll_interval)

                # Pause polling during night hours (11 PM - 6 AM)
                from datetime import datetime
                current_hour = datetime.now().hour
                if current_hour >= 23 or current_hour < 6:
                    logger.debug("HVAC polling paused (night hours: 11 PM - 6 AM)")
                    continue

                # Get current device status (use get_full_status for operating state)
                status = await self.kumo.get_full_status()
                if not status:
                    continue

                # Parse mode and power from device
                device_power = status.get("power", 0)
                device_mode = status.get("operationMode", "off") if device_power == 1 else "off"
                device_sp_heat = status.get("spHeat")
                device_sp_cool = status.get("spCool")

                # Determine the active setpoint based on mode
                if device_mode == "heat" and device_sp_heat:
                    device_setpoint_c = device_sp_heat
                elif device_mode == "cool" and device_sp_cool:
                    device_setpoint_c = device_sp_cool
                else:
                    device_setpoint_c = None

                # Check if device state differs from what we think
                mode_changed = device_mode != self._hvac_mode
                setpoint_changed = (device_setpoint_c is not None and
                                    abs(device_setpoint_c - self._hvac_setpoint_c) > 0.5)

                if mode_changed or setpoint_changed:
                    old_mode = self._hvac_mode
                    old_setpoint_f = self._hvac_setpoint_c * 9/5 + 32
                    new_setpoint_f = device_setpoint_c * 9/5 + 32 if device_setpoint_c else old_setpoint_f

                    logger.info(f"HVAC state sync: mode {old_mode}→{device_mode}, "
                                f"setpoint {old_setpoint_f:.0f}°F→{new_setpoint_f:.0f}°F "
                                f"(detected external change)")

                    # Update our local state
                    self._hvac_mode = device_mode
                    if device_setpoint_c:
                        self._hvac_setpoint_c = device_setpoint_c

                    # Update last_mode if device is on
                    if device_mode != "off":
                        self._hvac_last_mode = device_mode

                    # If we thought HVAC was suspended but it's actually on,
                    # clear the suspended flag
                    if self._hvac_suspended and device_mode != "off":
                        logger.info("Clearing HVAC suspended flag (device is on)")
                        self._hvac_suspended = False

                    # Broadcast updated status to dashboard
                    await self._broadcast_status()

            except asyncio.CancelledError:
                logger.info("HVAC polling task cancelled")
                break
            except Exception as e:
                logger.error(f"Error polling HVAC status: {e}")
                # Continue polling despite errors

    def _on_state_change(self, old_state: OccupancyState, new_state: OccupancyState):
        """Handle occupancy state changes."""
        logger.info(f"=== STATE CHANGE: {old_state.value} → {new_state.value} ===")

        # Clear manual overrides - automation takes over on state change
        self._clear_manual_overrides(f"{old_state.value}→{new_state.value}")

        # Get latest CO2 reading
        reading = self.qingping.latest_reading
        co2 = reading.co2_ppm if reading else None
        logger.info(f"Current CO2: {co2}ppm" if co2 else "CO2: unknown")

        # Clear rate history on departure - ensures TURBO start, then adaptive takes over
        if new_state == OccupancyState.AWAY:
            logger.info("Clearing CO2/tVOC history for fresh adaptive calculation")
            self._co2_history.clear()
            self._tvoc_history.clear()
            self._away_start_time = datetime.now()
            logger.info(f"TURBO mode for {self.config.thresholds.co2_turbo_duration_minutes} min, then adaptive")

        # Clear AWAY mode state on arrival
        if new_state == OccupancyState.PRESENT:
            self._away_start_time = None
            if self._tvoc_away_ventilation_active:
                logger.info("Clearing tVOC AWAY ventilation state: user returned")
                self._tvoc_away_ventilation_active = False
                self._tvoc_plateau_detected = False

        # Clear plateau detection state on arrival (start fresh CO2 refresh cycle)
        if new_state == OccupancyState.PRESENT and self._plateau_detected:
            logger.info("Clearing plateau state: user returned")
            self._plateau_detected = False
            # Keep outdoor baseline for reference

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

    async def update_mac_occupancy(self, last_active_timestamp: float, external_monitor: bool):
        """Update from macOS occupancy detector."""
        await self.state_machine.update_mac_occupancy(last_active_timestamp, external_monitor)

    # --- HTTP Server ---

    async def _handle_occupancy_post(self, request: web.Request) -> web.Response:
        """Handle POST /occupancy from macOS detector."""
        try:
            data = await request.json()
            last_active_timestamp = data.get("last_active_timestamp", 0.0)
            external_monitor = data.get("external_monitor", False)

            logger.info(f"Mac occupancy update: last_active={last_active_timestamp}, monitor={external_monitor}")
            await self.update_mac_occupancy(last_active_timestamp, external_monitor)

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
                self._erv_speed = "off"
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

            # Clear suspension flag (user manually controlling, don't auto-restore later)
            self._hvac_suspended = False
            self._hvac_last_mode = None

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

    async def _handle_qingping_interval_post(self, request: web.Request) -> web.Response:
        """Handle POST /qingping/interval to configure reporting interval.

        Body: {"interval": 60}  (seconds, minimum 15)
        """
        try:
            data = await request.json()
            interval = data.get("interval", 60)

            if not isinstance(interval, int) or interval < 15:
                return web.json_response(
                    {"ok": False, "error": "Interval must be an integer >= 15 seconds"},
                    status=400
                )

            if self.qingping.configure_interval(interval):
                logger.info(f"Qingping interval reconfigured to {interval}s via API")
                return web.json_response({
                    "ok": True,
                    "interval": interval,
                    "message": f"Device configured to report every {interval} seconds"
                })
            else:
                return web.json_response(
                    {"ok": False, "error": "Failed to send configuration to device"},
                    status=503
                )

        except Exception as e:
            logger.error(f"Error handling Qingping interval POST: {e}")
            return web.json_response({"ok": False, "error": str(e)}, status=400)

    async def _handle_history_get(self, request: web.Request) -> web.Response:
        """Handle GET /history for historical data.

        Query params:
        - hours: Number of hours of history (default: 24, max: 168)
        - limit: Max number of records (default: 1000, max: 10000)
        """
        try:
            hours = int(request.query.get("hours", "24"))
            limit = int(request.query.get("limit", "1000"))

            # Validate params
            hours = min(max(1, hours), 168)  # 1 hour to 1 week
            limit = min(max(10, limit), 10000)

            # Get historical data from database
            sensor_readings = self.db.get_sensor_readings(hours=hours, limit=limit)
            occupancy_history = self.db.get_occupancy_history(hours=hours, limit=limit)
            device_events = self.db.get_device_events(hours=hours, limit=limit)
            climate_actions = self.db.get_climate_actions(hours=hours, limit=limit)

            return web.json_response({
                "ok": True,
                "hours": hours,
                "sensor_readings": sensor_readings,
                "occupancy_history": occupancy_history,
                "device_events": device_events,
                "climate_actions": climate_actions,
            })

        except Exception as e:
            logger.error(f"Error handling history GET: {e}")
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
            "pm10": reading.pm10 if reading else None,
            "tvoc": reading.tvoc if reading else None,
            "noise_db": reading.noise_db if reading else None,
            "last_update": reading.timestamp.isoformat() if reading and reading.timestamp else None,
            "report_interval": self.qingping.report_interval,
            "interval_configured": self.qingping._interval_configured,
        }

        # Add ERV status
        sm_status["erv"] = {
            "running": self._erv_running,
            "tvoc_ventilation": self._tvoc_away_ventilation_active,  # tVOC AWAY mode ventilation
            "speed": self._erv_speed,
            "tvoc_plateau": self._tvoc_plateau_detected,
            "tvoc_baseline": self._tvoc_baseline,
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

        # Authenticate WebSocket connection if OAuth is enabled
        # Skip auth for trusted networks (local network access)
        if self.oauth and not self._is_trusted_network(request):
            try:
                # Expect first message to be auth message
                msg = await ws.receive(timeout=10)
                if msg.type != WSMsgType.TEXT:
                    await ws.close(code=4001, message=b'Authentication required')
                    return ws

                data = json.loads(msg.data)
                if data.get('type') != 'auth':
                    await ws.close(code=4001, message=b'Authentication required')
                    return ws

                token = data.get('token')
                email = self.oauth.verify_jwt(token)

                if not email:
                    await ws.close(code=4001, message=b'Invalid token')
                    return ws

                logger.debug(f"WebSocket authenticated: {email}")

            except Exception as e:
                logger.warning(f"WebSocket auth failed: {e}")
                await ws.close(code=4001, message=b'Authentication failed')
                return ws
        elif self._is_trusted_network(request):
            logger.debug("WebSocket from trusted network, skipping auth")

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
        response.headers["Access-Control-Allow-Headers"] = "Content-Type, Authorization"
        return response

    def _basic_auth_middleware(self, username: str, password: str):
        """Create a basic auth middleware with the given credentials."""
        @web.middleware
        async def middleware(request: web.Request, handler):
            # Skip auth for WebSocket upgrade requests (browser handles auth before upgrade)
            if request.headers.get("Upgrade") == "websocket":
                return await handler(request)

            # Get Authorization header
            auth_header = request.headers.get("Authorization", "")

            if not auth_header.startswith("Basic "):
                # No auth provided, request it
                return web.Response(
                    status=401,
                    headers={"WWW-Authenticate": 'Basic realm="Office Climate"'},
                    text="Authentication required"
                )

            # Decode credentials
            try:
                encoded_credentials = auth_header[6:]  # Skip "Basic "
                decoded = base64.b64decode(encoded_credentials).decode("utf-8")
                provided_username, provided_password = decoded.split(":", 1)

                # Verify credentials
                if provided_username == username and provided_password == password:
                    return await handler(request)
                else:
                    return web.Response(
                        status=401,
                        headers={"WWW-Authenticate": 'Basic realm="Office Climate"'},
                        text="Invalid credentials"
                    )
            except Exception:
                return web.Response(
                    status=401,
                    headers={"WWW-Authenticate": 'Basic realm="Office Climate"'},
                    text="Invalid authorization header"
                )

        return middleware

    def _is_trusted_network(self, request: web.Request) -> bool:
        """Check if request is from a trusted network."""
        if not self.oauth or not self.oauth.trusted_networks:
            return False

        # Get client IP (handle X-Forwarded-For for proxies)
        forwarded_for = request.headers.get('X-Forwarded-For')
        if forwarded_for:
            client_ip = forwarded_for.split(',')[0].strip()
        else:
            client_ip = request.remote

        if not client_ip:
            return False

        try:
            client_addr = ipaddress.ip_address(client_ip)
            for network_str in self.oauth.trusted_networks:
                network = ipaddress.ip_network(network_str, strict=False)
                if client_addr in network:
                    logger.info(f"Request from trusted network: {client_ip} in {network_str}")
                    return True
        except (ValueError, AttributeError) as e:
            logger.warning(f"Invalid IP or network: {e}")
            return False

        return False

    def _oauth_middleware(self):
        """Create OAuth JWT middleware."""
        @web.middleware
        async def middleware(request: web.Request, handler):
            # Skip auth for OAuth endpoints
            skip_paths = ['/auth/login', '/auth/callback', '/auth/device/start', '/auth/device/poll']
            if any(request.path.startswith(path) for path in skip_paths):
                return await handler(request)

            # Skip auth for WebSocket (will auth in handler)
            if request.headers.get("Upgrade") == "websocket":
                return await handler(request)

            # Skip auth for static assets and frontend HTML (allow login page to load)
            if request.path.startswith('/assets/') or request.path in ['/', '/index.html'] or request.path.endswith('.png') or request.path.endswith('.json'):
                return await handler(request)

            # Skip auth for trusted networks (local network access)
            if self._is_trusted_network(request):
                request['user_email'] = 'trusted_network'  # Placeholder email
                return await handler(request)

            # Get Authorization header
            auth_header = request.headers.get("Authorization", "")

            if not auth_header.startswith("Bearer "):
                return web.json_response(
                    {"error": "Authentication required", "login_url": "/auth/login"},
                    status=401
                )

            # Verify JWT
            token = auth_header[7:]
            email = self.oauth.verify_jwt(token)

            if not email:
                return web.json_response(
                    {"error": "Invalid or expired token", "login_url": "/auth/login"},
                    status=401
                )

            # Attach email to request for handlers
            request['user_email'] = email

            return await handler(request)

        return middleware

    async def _handle_auth_login(self, request: web.Request) -> web.Response:
        """Handle GET /auth/login - Start OAuth flow."""
        if not self.oauth:
            return web.json_response({"error": "OAuth not configured"}, status=501)

        # Determine redirect URI based on request host
        # LocalTunnel: climate.loca.lt -> https://climate.loca.lt/auth/callback
        # Local: localhost/IP -> http://localhost:8080/auth/callback
        host = request.host
        if 'loca.lt' in host:
            redirect_uri = f"https://{host}/auth/callback"
        else:
            redirect_uri = f"http://localhost:{self.config.orchestrator.port}/auth/callback"

        # Temporarily update OAuth redirect_uri for this request
        original_redirect_uri = self.oauth.redirect_uri
        self.oauth.redirect_uri = redirect_uri

        # Generate PKCE pair
        code_verifier, code_challenge = self.oauth.generate_pkce_pair()
        state = secrets.token_urlsafe(32)

        # Store state and redirect_uri for callback
        self._oauth_states[state] = code_verifier
        self._oauth_redirect_uris = getattr(self, '_oauth_redirect_uris', {})
        self._oauth_redirect_uris[state] = redirect_uri

        # Generate authorization URL
        auth_url = self.oauth.create_authorization_url(state, code_challenge)

        # Restore original redirect_uri
        self.oauth.redirect_uri = original_redirect_uri

        return web.json_response({
            "authorization_url": auth_url,
            "state": state
        })

    async def _handle_auth_callback(self, request: web.Request) -> web.Response:
        """Handle GET /auth/callback - OAuth redirect."""
        if not self.oauth:
            return web.Response(text="OAuth not configured", status=501)

        # Extract code and state
        code = request.query.get('code')
        state = request.query.get('state')
        error = request.query.get('error')

        if error:
            logger.warning(f"OAuth callback error: {error}")
            return web.Response(
                text=f"<html><body><h1>Login Failed</h1><p>{error}</p></body></html>",
                content_type='text/html',
                status=400
            )

        if not code or not state:
            return web.Response(text="Missing code or state", status=400)

        # Verify state and retrieve redirect_uri
        code_verifier = self._oauth_states.pop(state, None)
        redirect_uri = getattr(self, '_oauth_redirect_uris', {}).pop(state, None)

        if not code_verifier:
            return web.Response(text="Invalid state", status=400)

        # Exchange code for token (use stored redirect_uri for this state)
        session = await self.oauth.exchange_code_for_token(code, code_verifier, redirect_uri)

        if not session:
            return web.Response(
                text="<html><body><h1>Login Failed</h1><p>Email not authorized</p></body></html>",
                content_type='text/html',
                status=403
            )

        # Generate JWT
        jwt_token = self.oauth.generate_jwt(session.email)

        # Return HTML that stores token and redirects
        html = f"""
        <html>
        <head>
            <script>
                localStorage.setItem('auth_token', '{jwt_token}');
                localStorage.setItem('user_email', '{session.email}');
                window.location.href = '/';
            </script>
        </head>
        <body>
            <p>Login successful! Redirecting...</p>
        </body>
        </html>
        """

        return web.Response(text=html, content_type='text/html')

    async def _handle_auth_logout(self, request: web.Request) -> web.Response:
        """Handle POST /auth/logout - Logout user."""
        if not self.oauth:
            return web.json_response({"error": "OAuth not configured"}, status=501)

        # Get token from Authorization header
        auth_header = request.headers.get("Authorization", "")
        if not auth_header.startswith("Bearer "):
            return web.json_response({"error": "No token provided"}, status=401)

        token = auth_header[7:]
        email = self.oauth.verify_jwt(token)

        if email:
            self.oauth.logout(email)

        return web.json_response({"ok": True, "message": "Logged out"})

    async def _handle_auth_device_start(self, request: web.Request) -> web.Response:
        """Handle POST /auth/device/start - Start device flow."""
        if not self.oauth:
            return web.json_response({"error": "OAuth not configured"}, status=501)

        try:
            result = self.oauth.initiate_device_flow()
            return web.json_response(result)
        except Exception as e:
            logger.error(f"Device flow start failed: {e}")
            return web.json_response({"error": str(e)}, status=500)

    async def _handle_auth_device_poll(self, request: web.Request) -> web.Response:
        """Handle POST /auth/device/poll - Poll device flow."""
        if not self.oauth:
            return web.json_response({"error": "OAuth not configured"}, status=501)

        data = await request.json()
        device_code = data.get('device_code')

        if not device_code:
            return web.json_response({"error": "Missing device_code"}, status=400)

        result = self.oauth.poll_device_flow(device_code)
        return web.json_response(result)

    async def _start_http_server(self):
        """Start the HTTP server for macOS occupancy detector."""
        # Build middleware list
        middlewares = [self._cors_middleware]

        # Add auth middleware (prefer OAuth, fallback to Basic Auth)
        if self.oauth:
            auth_middleware = self._oauth_middleware()
            middlewares.append(auth_middleware)
            logger.info("OAuth JWT authentication enabled")
        elif self.config.orchestrator.auth_username and self.config.orchestrator.auth_password:
            # Fallback to Basic Auth (deprecated)
            auth_middleware = self._basic_auth_middleware(
                self.config.orchestrator.auth_username,
                self.config.orchestrator.auth_password
            )
            middlewares.append(auth_middleware)
            logger.warning("Using deprecated HTTP Basic Auth - migrate to OAuth!")
        else:
            logger.warning("No authentication configured - API is open!")

        self._app = web.Application(middlewares=middlewares)

        # API routes
        self._app.router.add_post("/occupancy", self._handle_occupancy_post)
        self._app.router.add_get("/status", self._handle_status_get)
        self._app.router.add_get("/history", self._handle_history_get)
        self._app.router.add_get("/ws", self._handle_websocket)
        self._app.router.add_post("/erv", self._handle_erv_post)
        self._app.router.add_post("/hvac", self._handle_hvac_post)
        self._app.router.add_post("/qingping/interval", self._handle_qingping_interval_post)

        # OAuth routes (if enabled)
        if self.oauth:
            self._app.router.add_get("/auth/login", self._handle_auth_login)
            self._app.router.add_get("/auth/callback", self._handle_auth_callback)
            self._app.router.add_post("/auth/logout", self._handle_auth_logout)
            self._app.router.add_post("/auth/device/start", self._handle_auth_device_start)
            self._app.router.add_post("/auth/device/poll", self._handle_auth_device_poll)
            logger.info("OAuth routes registered")

        # Serve frontend static files
        frontend_dist = Path(__file__).parent.parent / "frontend" / "dist"
        if frontend_dist.exists():
            # Serve static assets
            self._app.router.add_static("/assets", frontend_dist / "assets", name="assets")

            # Serve index.html for root and any other paths (SPA fallback)
            async def serve_index(request):
                return web.FileResponse(frontend_dist / "index.html")

            self._app.router.add_get("/", serve_index)
            self._app.router.add_get("/{path:.*}", serve_index)  # SPA fallback

            logger.info(f"Serving frontend from {frontend_dist}")
        else:
            logger.warning(f"Frontend dist not found at {frontend_dist}")

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
                status = await self.kumo.get_full_status()
                # Parse current mode and setpoint from status
                if status:
                    power = status.get("power", 0)
                    mode = status.get("operationMode", "off") if power == 1 else "off"
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

            # Configure device to report at desired interval
            # Small delay to ensure connection is fully established
            await asyncio.sleep(1)
            if self.qingping.configure_interval():
                logger.info(f"Qingping interval configured to {self.config.qingping.report_interval}s")
            else:
                logger.warning("Failed to configure Qingping interval (will use cloud default)")

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
                    noise_db=cached.get("noise_db"),
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

        # Map devices first (needed to identify which device is which)
        self._setup_yolink_handlers()

        # Restore YoLink sensor states from database (survives restarts)
        logger.info("Restoring YoLink sensor states from database...")
        door_state = self.db.get_latest_device_state("door")
        if door_state:
            is_open = door_state == "open"
            logger.info(f"Restored door state: {'OPEN' if is_open else 'CLOSED'}")
            await self.state_machine.update_door(is_open)

        window_state = self.db.get_latest_device_state("window")
        if window_state:
            is_open = window_state == "open"
            logger.info(f"Restored window state: {'OPEN' if is_open else 'CLOSED'}")
            await self.state_machine.update_window(is_open)

        motion_state = self.db.get_latest_device_state("motion")
        if motion_state:
            detected = motion_state == "detected"
            logger.info(f"Restored motion state: {'DETECTED' if detected else 'clear'}")
            await self.state_machine.update_motion(detected)

        # Start HVAC polling task
        if self.kumo:
            self._hvac_poll_task = asyncio.create_task(self._poll_hvac_status())
            interval = self.config.mitsubishi.poll_interval_seconds
            logger.info(f"Started HVAC status polling ({interval}s interval)")

        logger.info("Orchestrator running. Waiting for events...")

    async def stop(self):
        """Stop the orchestrator."""
        # Stop HVAC polling task
        if self._hvac_poll_task:
            self._hvac_poll_task.cancel()
            try:
                await self._hvac_poll_task
            except asyncio.CancelledError:
                pass

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
