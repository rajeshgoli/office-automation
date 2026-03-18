"""Unit tests for HVAC temperature-band hysteresis decisions."""

from src.hvac_hysteresis import get_hvac_band_action
from src.state_machine import OccupancyState


def test_pause_when_heat_reaches_upper_temp_band():
    action = get_hvac_band_action(
        temp_f=75.2,
        hvac_mode="heat",
        temp_band_mode=None,
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action == "pause_heat"


def test_resume_when_temp_drops_to_lower_temp_band():
    action = get_hvac_band_action(
        temp_f=70.9,
        hvac_mode="off",
        temp_band_mode="heat",
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action == "resume_heat"


def test_no_resume_in_away_when_erv_running_and_temp_above_min():
    action = get_hvac_band_action(
        temp_f=70.0,
        hvac_mode="off",
        temp_band_mode="heat",
        state=OccupancyState.AWAY,
        erv_running=True,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action is None


def test_no_resume_in_away_outside_occupancy_hours():
    action = get_hvac_band_action(
        temp_f=70.0,
        hvac_mode="off",
        temp_band_mode="heat",
        state=OccupancyState.AWAY,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=False,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action is None


def test_start_cooling_when_present_and_above_upper_temp_band():
    action = get_hvac_band_action(
        temp_f=81.1,
        hvac_mode="off",
        temp_band_mode=None,
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action == "start_cool"


def test_stop_cooling_when_temp_reaches_lower_temp_band():
    action = get_hvac_band_action(
        temp_f=78.0,
        hvac_mode="cool",
        temp_band_mode=None,
        state=OccupancyState.PRESENT,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action == "stop_cool"


def test_no_auto_cooling_start_when_away():
    action = get_hvac_band_action(
        temp_f=85.0,
        hvac_mode="off",
        temp_band_mode=None,
        state=OccupancyState.AWAY,
        erv_running=False,
        min_temp_f=68,
        within_occupancy_hours=True,
        heat_off_temp_f=75,
        heat_on_temp_f=71,
        cool_on_temp_f=81,
        cool_off_temp_f=78,
    )
    assert action is None
