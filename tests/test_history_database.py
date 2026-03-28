from datetime import datetime

from src.database import Database


def _set_fixed_now(monkeypatch, now: datetime) -> None:
    monkeypatch.setattr(Database, "_now", staticmethod(lambda: now))


def _insert_occupancy(db: Database, timestamp: str, state: str) -> None:
    with db._connection() as conn:
        conn.execute(
            """
            INSERT INTO occupancy_log (timestamp, state)
            VALUES (?, ?)
            """,
            (timestamp, state),
        )


def _insert_sensor(
    db: Database,
    timestamp: str,
    *,
    co2_ppm: int | None = None,
    temp_c: float | None = None,
) -> None:
    with db._connection() as conn:
        conn.execute(
            """
            INSERT INTO sensor_readings (timestamp, co2_ppm, temp_c)
            VALUES (?, ?, ?)
            """,
            (timestamp, co2_ppm, temp_c),
        )


def _insert_climate_action(db: Database, timestamp: str, system: str, action: str) -> None:
    with db._connection() as conn:
        conn.execute(
            """
            INSERT INTO climate_actions (timestamp, system, action)
            VALUES (?, ?, ?)
            """,
            (timestamp, system, action),
        )


def test_get_office_sessions_uses_now_for_active_day(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_occupancy(db, "2026-03-27 09:00:00", "present")
    _insert_occupancy(db, "2026-03-27 12:00:00", "away")
    _insert_occupancy(db, "2026-03-27 13:00:00", "present")

    result = db.get_office_sessions(days=1)

    assert result["sessions"] == [
        {
            "date": "2026-03-27",
            "arrival": "09:00:00",
            "departure": "15:00:00",
            "duration_hours": 5.0,
            "gaps": [
                {
                    "left": "12:00:00",
                    "returned": "13:00:00",
                    "duration_min": 60,
                }
            ],
        }
    ]


def test_short_range_history_queries_use_sqlite_timestamp_format(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_sensor(db, "2026-03-27 14:30:00", co2_ppm=900, temp_c=21.0)
    _insert_sensor(db, "2026-03-27 14:45:00", co2_ppm=950, temp_c=22.0)

    co2 = db.get_co2_ohlc(hours=1, bucket_minutes=60)
    temp = db.get_temperature_history(hours=1, bucket_minutes=60)

    assert len(co2["candles"]) == 1
    assert co2["candles"][0] == {
        "timestamp": co2["candles"][0]["timestamp"],
        "open": 900,
        "high": 950,
        "low": 900,
        "close": 950,
        "avg": 925,
        "readings": 2,
    }

    assert len(temp["points"]) == 1
    assert temp["points"][0] == {
        "timestamp": temp["points"][0]["timestamp"],
        "avg_f": 70.7,
        "min_f": 69.8,
        "max_f": 71.6,
        "readings": 2,
    }


def test_daily_stats_include_open_presence_and_runtime_intervals(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_occupancy(db, "2026-03-27 09:00:00", "present")
    _insert_climate_action(db, "2026-03-27 10:00:00", "erv", "quiet")
    _insert_climate_action(db, "2026-03-27 11:30:00", "hvac", "cool")

    stats = db.get_daily_stats(days=1)

    assert stats == [
        {
            "date": "2026-03-27",
            "door_events": 0,
            "erv_runtime_min": 300,
            "hvac_runtime_min": 210,
            "presence_hours": 6.0,
        }
    ]
