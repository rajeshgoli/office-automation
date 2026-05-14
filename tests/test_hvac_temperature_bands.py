"""Unit tests for editable HVAC temperature bands."""

import pytest

from src.config import ThresholdsConfig
from src.hvac_temperature_bands import (
    apply_temperature_bands,
    get_default_temperature_bands,
    validate_temperature_bands,
)


def test_validate_temperature_bands_accepts_ordered_values():
    bands = validate_temperature_bands(
        {
            "heat_on_temp_f": 70,
            "heat_off_temp_f": 74,
            "cool_off_temp_f": 77,
            "cool_on_temp_f": 82,
        }
    )

    assert bands == {
        "heat_on_temp_f": 70,
        "heat_off_temp_f": 74,
        "cool_off_temp_f": 77,
        "cool_on_temp_f": 82,
    }


def test_validate_temperature_bands_rejects_collapsed_heat_band():
    with pytest.raises(ValueError, match="heat_on_temp_f must be below"):
        validate_temperature_bands(
            {
                "heat_on_temp_f": 75,
                "heat_off_temp_f": 75,
                "cool_off_temp_f": 78,
                "cool_on_temp_f": 81,
            }
        )


def test_validate_temperature_bands_rejects_collapsed_cool_band():
    with pytest.raises(ValueError, match="cool_off_temp_f must be below"):
        validate_temperature_bands(
            {
                "heat_on_temp_f": 71,
                "heat_off_temp_f": 75,
                "cool_off_temp_f": 81,
                "cool_on_temp_f": 81,
            }
        )


def test_apply_temperature_bands_updates_threshold_config():
    thresholds = ThresholdsConfig()
    bands = {
        "heat_on_temp_f": 69,
        "heat_off_temp_f": 73,
        "cool_off_temp_f": 76,
        "cool_on_temp_f": 80,
    }

    apply_temperature_bands(thresholds, bands)

    assert get_default_temperature_bands(thresholds) == bands
