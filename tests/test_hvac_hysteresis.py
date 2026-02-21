"""Unit tests for HVAC heat-band hysteresis decisions."""

from src.hvac_hysteresis import get_heat_band_action
from src.state_machine import OccupancyState


def test_pause_when_heat_reaches_upper_temp_band():
    action = get_heat_band_action(
        temp_f=75.2,
        hvac_mode="heat",
        temp_band_paused=False,
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
    )
    assert action == "pause"


def test_resume_when_temp_drops_to_lower_temp_band():
    action = get_heat_band_action(
        temp_f=70.9,
        hvac_mode="off",
        temp_band_paused=True,
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
    )
    assert action == "resume"


def test_no_resume_in_away_when_erv_running_and_temp_above_min():
    action = get_heat_band_action(
        temp_f=70.0,
        hvac_mode="off",
        temp_band_paused=True,
        state=OccupancyState.AWAY,
        erv_running=True,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
    )
    assert action is None


def test_no_resume_in_away_outside_occupancy_hours():
    action = get_heat_band_action(
        temp_f=70.0,
        hvac_mode="off",
        temp_band_paused=True,
        state=OccupancyState.AWAY,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=False,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
    )
    assert action is None
