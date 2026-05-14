"""Unit tests for runtime app settings persistence."""

from src.database import Database


def test_runtime_setting_round_trips_json(tmp_path):
    db = Database(db_path=tmp_path / "office.db", telemetry_db_path=tmp_path / "telemetry.db")

    value = {
        "heat_on_temp_f": 70,
        "heat_off_temp_f": 74,
        "cool_off_temp_f": 77,
        "cool_on_temp_f": 82,
    }
    db.set_setting("hvac_temperature_bands", value)

    assert db.get_setting("hvac_temperature_bands") == value
    assert db.get_setting("missing") is None
