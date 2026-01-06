"""
Office Climate Automation State Machine

Implements PRESENT/AWAY state logic:
- PRESENT: Quiet mode - ERV off unless CO2 > 2000 ppm
- AWAY: Ventilation mode - ERV full blast until CO2 < 500 ppm

Presence detection:
  is_present = (mac_active AND external_monitor) OR motion_detected_recently

State transitions:
  AWAY → PRESENT: Any presence signal (immediate)
  PRESENT → AWAY: REQUIRES door open→close sequence + no presence signals
                  (motion timeout alone is not sufficient - can't leave without door)
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
    mac_active: bool = False
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

    @property
    def is_present(self) -> bool:
        """Check if any presence signal is active.

        For AWAY→PRESENT transitions, we require physical presence (motion or door).
        Mac activity alone is not enough - it just indicates the computer is on.
        """
        # Recent motion (within timeout) - this is physical presence
        motion_age = time.time() - self.sensors.motion_last_seen
        motion_presence = self.sensors.motion_detected or (motion_age < self.config.motion_timeout_seconds)

        # Mac active with external monitor - this is supporting evidence only
        mac_presence = self.sensors.mac_active and self.sensors.external_monitor

        # Physical presence (motion) is required for AWAY→PRESENT
        # Mac presence alone can't trigger return from AWAY
        if self.state == OccupancyState.AWAY:
            return motion_presence  # Only motion can bring us back from AWAY

        # When PRESENT, either signal keeps us present
        return mac_presence or motion_presence

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
                # Reset motion timestamp to prevent stale motion from triggering false PRESENT
                # (motion sensor may send "clear" event after we've departed)
                self.sensors.motion_last_seen = 0
                self.sensors.motion_detected = False
                await self._notify_state_change(old_state, self.state)
        except asyncio.CancelledError:
            pass  # Timer was cancelled due to activity
        finally:
            self._verifying_departure = False
            self._departure_verification_task = None

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

    async def evaluate(self) -> OccupancyState:
        """Evaluate current state and transition if needed.

        Note: PRESENT → AWAY transitions are now handled by the departure
        verification timer, not by this method.
        """
        old_state = self.state

        if self.state == OccupancyState.AWAY:
            # Transition to PRESENT on any presence signal
            if self.is_present:
                self.state = OccupancyState.PRESENT
                logger.info("State: AWAY → PRESENT")

        # Track door state for open→close detection
        self._last_door_state = self.sensors.door_open

        # Notify on change
        if self.state != old_state:
            await self._notify_state_change(old_state, self.state)

        return self.state

    # --- Sensor update methods ---

    async def update_mac_occupancy(self, active: bool, external_monitor: bool):
        """Update macOS occupancy status."""
        self.sensors.mac_active = active
        self.sensors.external_monitor = external_monitor
        self.sensors.last_updated = time.time()
        logger.debug(f"Mac occupancy: active={active}, monitor={external_monitor}")

        # Cancel departure verification if mac shows activity
        if active and self._verifying_departure:
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

        # Door opening while AWAY = someone entering (immediate PRESENT)
        if is_open and self.state == OccupancyState.AWAY:
            logger.info("Door opened while AWAY - transitioning to PRESENT")
            old_state = self.state
            self.state = OccupancyState.PRESENT
            await self._notify_state_change(old_state, self.state)

        # Door just closed (was open, now closed) - start departure verification
        if was_open is True and is_open is False:
            logger.info("Door closed - starting departure verification")
            self._start_departure_verification()

        await self.evaluate()

    async def update_window(self, is_open: bool):
        """Update window sensor status."""
        self.sensors.window_open = is_open
        self.sensors.last_updated = time.time()
        logger.debug(f"Window: {'open' if is_open else 'closed'}")
        await self.evaluate()

    async def update_co2(self, ppm: int):
        """Update CO2 reading."""
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
            "sensors": {
                "mac_active": self.sensors.mac_active,
                "external_monitor": self.sensors.external_monitor,
                "motion_detected": self.sensors.motion_detected,
                "door_open": self.sensors.door_open,
                "window_open": self.sensors.window_open,
                "co2_ppm": self.sensors.co2_ppm,
            }
        }
