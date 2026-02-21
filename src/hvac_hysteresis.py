"""HVAC heat-band hysteresis decision helper."""

from typing import Optional, Literal

from .state_machine import OccupancyState


def get_heat_band_action(
    *,
    temp_f: Optional[float],
    hvac_mode: str,
    temp_band_paused: bool,
    state: OccupancyState,
    erv_running: bool,
    min_temp_f: float,
    within_occupancy_hours: bool,
    heat_off_temp_f: float,
    heat_on_temp_f: float,
) -> Literal["pause", "resume"] | None:
    """Decide if heat should be paused/resumed for temperature comfort band."""
    if temp_f is None:
        return None

    if hvac_mode == "heat" and temp_f >= heat_off_temp_f:
        return "pause"

    if not (temp_band_paused and hvac_mode == "off" and temp_f <= heat_on_temp_f):
        return None

    if state == OccupancyState.AWAY:
        # Preserve existing AWAY coordination rules.
        if erv_running and temp_f > min_temp_f:
            return None
        if not within_occupancy_hours:
            return None

    return "resume"
