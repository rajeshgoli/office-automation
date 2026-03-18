"""HVAC temperature-band hysteresis decision helper."""

from typing import Optional, Literal

from .state_machine import OccupancyState


def get_hvac_band_action(
    *,
    temp_f: Optional[float],
    hvac_mode: str,
    temp_band_mode: Optional[str],
    state: OccupancyState,
    erv_running: bool,
    min_temp_f: float,
    within_occupancy_hours: bool,
    heat_off_temp_f: float,
    heat_on_temp_f: float,
    cool_on_temp_f: float,
    cool_off_temp_f: float,
) -> Literal["pause_heat", "resume_heat", "start_cool", "stop_cool"] | None:
    """Decide if HVAC should change mode for heat/cool comfort bands."""
    if temp_f is None:
        return None

    if hvac_mode == "heat" and temp_f >= heat_off_temp_f:
        return "pause_heat"

    if hvac_mode == "cool" and temp_f <= cool_off_temp_f:
        return "stop_cool"

    if hvac_mode != "off":
        return None

    if temp_band_mode == "heat" and temp_f <= heat_on_temp_f:
        if state == OccupancyState.AWAY:
            # Preserve existing AWAY coordination rules.
            if erv_running and temp_f > min_temp_f:
                return None
            if not within_occupancy_hours:
                return None

        return "resume_heat"

    if state == OccupancyState.PRESENT and temp_f > cool_on_temp_f:
        return "start_cool"

    return None
