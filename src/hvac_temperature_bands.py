"""Helpers for editable HVAC temperature hysteresis bands."""

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class TemperatureBandLimits:
    min_temp_f: int
    max_temp_f: int


HVAC_TEMPERATURE_BAND_LIMITS = {
    "heat_on_temp_f": TemperatureBandLimits(45, 85),
    "heat_off_temp_f": TemperatureBandLimits(46, 90),
    "cool_off_temp_f": TemperatureBandLimits(55, 95),
    "cool_on_temp_f": TemperatureBandLimits(56, 100),
}

HVAC_TEMPERATURE_BAND_KEYS = tuple(HVAC_TEMPERATURE_BAND_LIMITS.keys())


def get_default_temperature_bands(thresholds: Any) -> dict[str, int]:
    """Return HVAC temperature bands from the loaded threshold config."""
    return {
        "heat_on_temp_f": int(thresholds.hvac_heat_on_temp_f),
        "heat_off_temp_f": int(thresholds.hvac_heat_off_temp_f),
        "cool_off_temp_f": int(thresholds.hvac_cool_off_temp_f),
        "cool_on_temp_f": int(thresholds.hvac_cool_on_temp_f),
    }


def validate_temperature_bands(data: dict[str, Any]) -> dict[str, int]:
    """Validate and normalize editable HVAC temperature bands."""
    bands: dict[str, int] = {}
    for key in HVAC_TEMPERATURE_BAND_KEYS:
        if key not in data:
            raise ValueError(f"Missing temperature band: {key}")

        value = data[key]
        if isinstance(value, bool):
            raise ValueError(f"{key} must be a number")

        try:
            normalized = int(value)
        except (TypeError, ValueError) as exc:
            raise ValueError(f"{key} must be a number") from exc

        limits = HVAC_TEMPERATURE_BAND_LIMITS[key]
        if normalized < limits.min_temp_f or normalized > limits.max_temp_f:
            raise ValueError(
                f"{key} must be between {limits.min_temp_f} and {limits.max_temp_f}"
            )

        bands[key] = normalized

    if bands["heat_on_temp_f"] >= bands["heat_off_temp_f"]:
        raise ValueError("heat_on_temp_f must be below heat_off_temp_f")

    if bands["cool_off_temp_f"] >= bands["cool_on_temp_f"]:
        raise ValueError("cool_off_temp_f must be below cool_on_temp_f")

    return bands


def apply_temperature_bands(thresholds: Any, bands: dict[str, int]) -> None:
    """Apply validated HVAC temperature bands to the mutable threshold config."""
    thresholds.hvac_heat_on_temp_f = bands["heat_on_temp_f"]
    thresholds.hvac_heat_off_temp_f = bands["heat_off_temp_f"]
    thresholds.hvac_cool_off_temp_f = bands["cool_off_temp_f"]
    thresholds.hvac_cool_on_temp_f = bands["cool_on_temp_f"]
