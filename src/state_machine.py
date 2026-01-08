"""
Office Climate Automation State Machine

Implements PRESENT/AWAY state logic:
- PRESENT: Quiet mode - ERV off unless CO2 > 2000 ppm
- AWAY: Ventilation mode - ERV full blast until CO2 < 500 ppm

Operating Modes:

Normal Mode (Door recently changed or closed):
  - AWAY → PRESENT: Mac activity OR motion AFTER door event
  - PRESENT → AWAY: Door close + 10s verification with no activity
  - Door events required for transitions

Door Open Mode (Door open for 5+ minutes):
  - AWAY → PRESENT: Immediately on any Mac OR motion activity
  - PRESENT → AWAY: After 5 minutes of no activity
  - Free transitions based on activity alone
  - Door close exits door open mode → back to normal

This handles "door open for ventilation" scenarios without false departures.
"""

import asyncio
import time
import logging
from dataclasses import dataclass, field
from enum import Enum
from typing import Callable, Any, Optional

logger = logging.getLogger(__name__)


class OccupancyState(Enum):
    PRESENT = "present"
    AWAY = "away"


@dataclass
class SensorState:
    """Current state of all sensors."""
    # macOS occupancy
    mac_last_active: float = 0  # timestamp of last keyboard/mouse activity
    external_monitor: bool = False

    # YoLink sensors
    motion_detected: bool = False
    motion_last_seen: float = 0  # timestamp

    door_open: bool = False
    door_last_changed: float = 0

    window_open: bool = False

    # Qingping
    co2_ppm: int = 400

    # Timestamps
    last_updated: float = field(default_factory=time.time)


@dataclass
class StateConfig:
    """Configuration for state machine thresholds."""
    motion_timeout_seconds: int = 60
    departure_verification_seconds: int = 10  # Time to wait after door close before confirming departure
    door_open_threshold_minutes: int = 5  # Door open this long → door open mode (activity-based transitions)
    door_open_away_timeout_minutes: int = 5  # In door open mode, no activity for this long → AWAY
    co2_critical_ppm: int = 2000
    co2_refresh_target_ppm: int = 500


class StateMachine:
    """
    PRESENT/AWAY state machine for office climate automation.

    Transitions:
    - TO PRESENT: Any presence signal (immediate)
    - TO AWAY: Departure signal + no presence signals
    """

    def __init__(self, config: StateConfig):
        self.config = config
        self.state = OccupancyState.AWAY
        self.sensors = SensorState()
        self._callbacks: list[Callable[[OccupancyState, OccupancyState], Any]] = []
        self._last_door_state: Optional[bool] = None
        self._departure_verification_task: Optional[asyncio.Task] = None
        self._verifying_departure: bool = False
        self._door_open_away_task: Optional[asyncio.Task] = None  # Timer for door open mode AWAY transition
        self._last_activity_time: float = time.time()  # Track last Mac or motion activity for door open mode

    @property
    def in_door_open_mode(self) -> bool:
        """Check if in door open mode (door open for 5+ minutes).

        In door open mode:
        - Transitions based on activity alone (not door events)
        - AWAY → PRESENT: immediate on any activity
        - PRESENT → AWAY: after 5 min no activity
        """
        if not self.sensors.door_open:
            return False

        door_open_duration_minutes = (time.time() - self.sensors.door_last_changed) / 60
        return door_open_duration_minutes >= self.config.door_open_threshold_minutes

    @property
    def is_present(self) -> bool:
        """Check if any presence signal is active.

        In door open mode (door open 5+ min):
        - Any recent Mac activity OR motion = present
        - No timestamp comparison needed

        In normal mode:
        - Mac keyboard/mouse activity AFTER last door event (strongest signal)
        - Motion detected AFTER last door event while door is closed
        - Only activity AFTER the door last changed counts as presence
        """
        if self.in_door_open_mode:
            # Door open mode: simple activity check
            motion_age = time.time() - self.sensors.motion_last_seen
            motion_recent = self.sensors.motion_detected or (motion_age < self.config.motion_timeout_seconds)

            mac_recent = (self.sensors.external_monitor and
                         self.sensors.mac_last_active > 0)

            return mac_recent or motion_recent

        # Normal mode: door-event-based presence
        # Mac activity only counts if:
        # 1. External monitor connected (Mac is in the room)
        # 2. Activity happened AFTER last door event (not pre-departure)
        mac_presence = (self.sensors.external_monitor and
                       self.sensors.mac_last_active > self.sensors.door_last_changed)

        # Recent motion while door is closed = inside the room
        motion_age = time.time() - self.sensors.motion_last_seen
        motion_recent = self.sensors.motion_detected or (motion_age < self.config.motion_timeout_seconds)
        # Motion only counts if:
        # 1. Door is closed (not outside reaching in)
        # 2. Motion happened AFTER door last changed (not pre-departure walking to door)
        motion_inside = (motion_recent and
                        not self.sensors.door_open and
                        self.sensors.motion_last_seen > self.sensors.door_last_changed)

        return mac_presence or motion_inside

    @property
    def door_just_closed(self) -> bool:
        """Detect door open→close sequence (departure signal)."""
        if self._last_door_state is True and self.sensors.door_open is False:
            return True
        return False

    @property
    def motion_timed_out(self) -> bool:
        """Check if motion has timed out."""
        if not self.sensors.motion_detected:
            motion_age = time.time() - self.sensors.motion_last_seen
            return motion_age > self.config.motion_timeout_seconds
        return False

    @property
    def should_be_away(self) -> bool:
        """Check if departure conditions are met.

        REQUIRES door open→close sequence. Motion timeout alone is NOT sufficient
        since user cannot leave the office without using the door.
        """
        return self.door_just_closed and not self.is_present

    @property
    def safety_interlock_active(self) -> bool:
        """Check if window or door is open (climate systems should be off)."""
        return self.sensors.window_open or self.sensors.door_open

    @property
    def erv_should_run(self) -> bool:
        """Determine if ERV should be running."""
        if self.safety_interlock_active:
            return False

        if self.state == OccupancyState.PRESENT:
            # Only run if CO2 is critical
            return self.sensors.co2_ppm > self.config.co2_critical_ppm
        else:
            # Run until CO2 target reached
            return self.sensors.co2_ppm > self.config.co2_refresh_target_ppm

    def on_state_change(self, callback: Callable[[OccupancyState, OccupancyState], Any]):
        """Register callback for state changes. Callback receives (old_state, new_state)."""
        self._callbacks.append(callback)

    async def _notify_state_change(self, old_state: OccupancyState, new_state: OccupancyState):
        """Notify all callbacks of state change."""
        for callback in self._callbacks:
            try:
                result = callback(old_state, new_state)
                if asyncio.iscoroutine(result):
                    await result
            except Exception as e:
                logger.error(f"State change callback error: {e}")

    def _cancel_departure_verification(self, reason: str = "activity detected"):
        """Cancel any pending departure verification."""
        if self._departure_verification_task and not self._departure_verification_task.done():
            logger.info(f"Departure verification cancelled: {reason}")
            self._departure_verification_task.cancel()
            self._departure_verification_task = None
            self._verifying_departure = False

    def _cancel_door_open_away_timer(self, reason: str = "activity detected"):
        """Cancel door open mode AWAY timer."""
        if self._door_open_away_task and not self._door_open_away_task.done():
            logger.debug(f"Door open mode AWAY timer cancelled: {reason}")
            self._door_open_away_task.cancel()
            self._door_open_away_task = None

    async def _departure_verification_timer(self):
        """Wait for verification period, then transition to AWAY if no activity."""
        try:
            logger.info(f"Departure verification started ({self.config.departure_verification_seconds}s)")
            await asyncio.sleep(self.config.departure_verification_seconds)

            # Timer expired without activity - confirm departure
            if self.state == OccupancyState.PRESENT:
                logger.info("Departure verified: no activity detected → AWAY")
                old_state = self.state
                self.state = OccupancyState.AWAY
                # Reset motion signals so only NEW activity triggers return
                # Motion: reset timestamp so stale motion doesn't trigger PRESENT
                # Mac: timestamp comparison with door_last_changed handles this automatically
                self.sensors.motion_last_seen = 0
                self.sensors.motion_detected = False
                await self._notify_state_change(old_state, self.state)
        except asyncio.CancelledError:
            pass  # Timer was cancelled due to activity
        finally:
            self._verifying_departure = False
            self._departure_verification_task = None

    async def _door_open_away_timer(self):
        """In door open mode, transition to AWAY after no activity for 5 minutes."""
        try:
            timeout_seconds = self.config.door_open_away_timeout_minutes * 60
            logger.info(f"Door open mode AWAY timer started ({self.config.door_open_away_timeout_minutes} min)")
            await asyncio.sleep(timeout_seconds)

            # Timer expired - no activity detected in door open mode
            if self.state == OccupancyState.PRESENT and self.in_door_open_mode:
                logger.info("Door open mode: no activity for 5 minutes → AWAY")
                old_state = self.state
                self.state = OccupancyState.AWAY
                await self._notify_state_change(old_state, self.state)
        except asyncio.CancelledError:
            pass  # Timer was cancelled due to activity
        finally:
            self._door_open_away_task = None

    def _start_departure_verification(self):
        """Start departure verification timer after door closes."""
        if self.state != OccupancyState.PRESENT:
            return  # Only verify departure if currently present

        # Cancel any existing verification
        self._cancel_departure_verification("new door event")

        # Start new verification timer
        self._verifying_departure = True
        self._departure_verification_task = asyncio.create_task(
            self._departure_verification_timer()
        )

    def _start_door_open_away_timer(self):
        """Start door open mode AWAY timer (triggered by activity)."""
        if self.state != OccupancyState.PRESENT:
            return

        # Cancel existing timer and restart
        self._cancel_door_open_away_timer("restarting due to new activity")

        # Start new timer
        self._door_open_away_task = asyncio.create_task(
            self._door_open_away_timer()
        )

    async def evaluate(self) -> OccupancyState:
        """Evaluate current state and transition if needed.

        Normal mode: PRESENT → AWAY via departure verification timer
        Door open mode: AWAY ↔ PRESENT based on activity
        """
        old_state = self.state

        if self.in_door_open_mode:
            # Door open mode: activity-based transitions
            if self.state == OccupancyState.AWAY and self.is_present:
                # Immediate PRESENT on activity
                self.state = OccupancyState.PRESENT
                logger.info("Door open mode: AWAY → PRESENT (activity detected)")
                # Start timer for AWAY transition
                self._start_door_open_away_timer()
            elif self.state == OccupancyState.PRESENT and self.is_present:
                # Activity detected while PRESENT - restart AWAY timer
                self._start_door_open_away_timer()
            # Note: PRESENT → AWAY handled by _door_open_away_timer
        else:
            # Normal mode: door-event-based transitions
            if self.state == OccupancyState.AWAY and self.is_present:
                self.state = OccupancyState.PRESENT
                logger.info("State: AWAY → PRESENT")
            # Note: PRESENT → AWAY handled by _departure_verification_timer

        # Track door state for open→close detection
        self._last_door_state = self.sensors.door_open

        # Notify on change
        if self.state != old_state:
            await self._notify_state_change(old_state, self.state)

        return self.state

    # --- Sensor update methods ---

    async def update_mac_occupancy(self, last_active_timestamp: float, external_monitor: bool):
        """Update macOS occupancy status.

        Args:
            last_active_timestamp: Timestamp of last keyboard/mouse activity (Unix time)
            external_monitor: Whether external monitor is connected
        """
        self.sensors.external_monitor = external_monitor
        self.sensors.mac_last_active = last_active_timestamp
        self.sensors.last_updated = time.time()

        logger.debug(f"Mac occupancy: last_active={last_active_timestamp:.1f}, monitor={external_monitor}")

        # Cancel departure verification if mac shows activity after door event
        if last_active_timestamp > self.sensors.door_last_changed and self._verifying_departure:
            self._cancel_departure_verification("mac activity")

        await self.evaluate()

    async def update_motion(self, detected: bool):
        """Update motion sensor status."""
        self.sensors.motion_detected = detected
        if detected:
            self.sensors.motion_last_seen = time.time()
            # Cancel departure verification on motion
            if self._verifying_departure:
                self._cancel_departure_verification("motion detected")
        self.sensors.last_updated = time.time()
        logger.debug(f"Motion: {detected}")
        await self.evaluate()

    async def update_door(self, is_open: bool):
        """Update door sensor status."""
        was_open = self._last_door_state

        self.sensors.door_open = is_open
        self.sensors.door_last_changed = time.time()
        self.sensors.last_updated = time.time()
        logger.debug(f"Door: {'open' if is_open else 'closed'}")

        # Door closing exits door open mode - return to normal door-event logic
        if was_open is True and is_open is False:
            logger.info("Door closed - exiting door open mode, starting departure verification")
            # Cancel door open mode timer if active
            self._cancel_door_open_away_timer("door closed")
            # Start normal departure verification
            self._start_departure_verification()

        await self.evaluate()

    async def update_window(self, is_open: bool):
        """Update window sensor status."""
        self.sensors.window_open = is_open
        self.sensors.last_updated = time.time()
        logger.debug(f"Window: {'open' if is_open else 'closed'}")
        await self.evaluate()

    def update_co2(self, ppm: int):
        """Update CO2 reading (sync - no state transitions needed)."""
        self.sensors.co2_ppm = ppm
        self.sensors.last_updated = time.time()
        logger.debug(f"CO2: {ppm} ppm")
        # CO2 doesn't affect occupancy state, just ERV decisions

    def get_status(self) -> dict:
        """Get current status summary."""
        return {
            "state": self.state.value,
            "is_present": self.is_present,
            "safety_interlock": self.safety_interlock_active,
            "erv_should_run": self.erv_should_run,
            "verifying_departure": self._verifying_departure,
            "in_door_open_mode": self.in_door_open_mode,
            "sensors": {
                "mac_last_active": self.sensors.mac_last_active,
                "external_monitor": self.sensors.external_monitor,
                "motion_detected": self.sensors.motion_detected,
                "door_open": self.sensors.door_open,
                "window_open": self.sensors.window_open,
                "co2_ppm": self.sensors.co2_ppm,
            }
        }
