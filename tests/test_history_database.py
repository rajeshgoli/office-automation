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


def _insert_device_event(db: Database, timestamp: str, device_type: str, event: str) -> None:
    with db._connection() as conn:
        conn.execute(
            """
            INSERT INTO device_events (timestamp, device_type, event)
            VALUES (?, ?, ?)
            """,
            (timestamp, device_type, event),
        )


def _insert_orchestration(
    db: Database,
    timestamp: str,
    tool: str,
    project: str,
    session_id: str,
) -> None:
    with db._connection() as conn:
        conn.execute(
            """
            INSERT INTO orchestration_activity (timestamp, tool, project, session_id)
            VALUES (?, ?, ?, ?)
            """,
            (timestamp, tool, project, session_id),
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


def test_daily_stats_seed_open_state_from_before_cutoff(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_occupancy(db, "2026-03-26 14:00:00", "present")
    _insert_occupancy(db, "2026-03-26 16:00:00", "away")
    _insert_climate_action(db, "2026-03-26 14:00:00", "erv", "quiet")
    _insert_climate_action(db, "2026-03-26 16:00:00", "erv", "off")
    _insert_climate_action(db, "2026-03-26 14:30:00", "hvac", "cool")
    _insert_climate_action(db, "2026-03-26 16:00:00", "hvac", "off")

    stats = db.get_daily_stats(days=1)

    assert stats == [
        {
            "date": "2026-03-26",
            "door_events": 0,
            "erv_runtime_min": 60,
            "hvac_runtime_min": 60,
            "presence_hours": 1.0,
        }
    ]


def test_orchestration_activity_returns_daily_counts(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_orchestration(db, "2026-03-25 09:15:00", "claude", "office-automate", "s1")
    _insert_orchestration(db, "2026-03-25 10:02:00", "codex", "office-automate", "s1")
    _insert_orchestration(db, "2026-03-26 11:30:00", "claude", "taskbar", "s2")
    _insert_orchestration(db, "2026-03-27 08:45:00", "claude", "office-automate", "s3")
    _insert_orchestration(db, "2026-03-27 14:20:00", "codex", "taskbar", "s4")

    result = db.get_orchestration_activity(days=3)

    assert [day["date"] for day in result] == ["2026-03-25", "2026-03-26", "2026-03-27"]
    assert [day["messages"] for day in result] == [2, 1, 2]
    assert result[0]["sessions"] == 1
    assert result[2]["sessions"] == 2
    assert result[0]["first_prompt"] == "09:15"
    assert result[0]["last_prompt"] == "10:02"
    assert result[2]["by_tool"] == {"claude": 1, "codex": 1}
    assert result[2]["timestamps"] == [
        {"time": "08:45", "tool": "claude"},
        {"time": "14:20", "tool": "codex"},
    ]


def test_project_focus_returns_daily_project_mix(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_orchestration(db, "2026-03-25 09:15:00", "claude", "office-automate", "s1")
    _insert_orchestration(db, "2026-03-25 10:02:00", "codex", "office-automate", "s1")
    _insert_orchestration(db, "2026-03-26 11:30:00", "claude", "taskbar", "s2")
    _insert_orchestration(db, "2026-03-27 08:45:00", "claude", "office-automate", "s3")
    _insert_orchestration(db, "2026-03-27 14:20:00", "codex", "taskbar", "s4")
    _insert_orchestration(db, "2026-03-27 14:45:00", "claude", "taskbar", "s4")

    result = db.get_project_focus(days=3)

    assert [day["date"] for day in result] == ["2026-03-25", "2026-03-26", "2026-03-27"]
    assert result[0]["total"] == 2
    assert result[0]["projects"] == [{
        "name": "office-automate",
        "messages": 2,
        "first_prompt": "09:15",
        "last_prompt": "10:02",
    }]
    assert result[2]["total"] == 3
    assert result[2]["projects"] == [
        {
            "name": "taskbar",
            "messages": 2,
            "first_prompt": "14:20",
            "last_prompt": "14:45",
        },
        {
            "name": "office-automate",
            "messages": 1,
            "first_prompt": "08:45",
            "last_prompt": "08:45",
        },
    ]


def test_project_focus_collapses_fractal_worktrees(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_orchestration(db, "2026-03-27 09:00:00", "claude", "fractal-market-simulator", "s1")
    _insert_orchestration(db, "2026-03-27 11:30:00", "codex", "fractal-1808-em", "s2")
    _insert_orchestration(db, "2026-03-27 13:45:00", "claude", "fractal-1812-fix", "s3")

    result = db.get_project_focus(days=1)

    assert result == [{
        "date": "2026-03-27",
        "total": 3,
        "projects": [{
            "name": "fractal",
            "messages": 3,
            "first_prompt": "09:00",
            "last_prompt": "13:45",
        }],
    }]


def test_get_openings_handles_unclosed_opening(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_device_event(db, "2026-03-27 09:00:00", "door", "open")
    _insert_device_event(db, "2026-03-27 09:05:00", "door", "closed")
    _insert_device_event(db, "2026-03-27 12:00:00", "door", "open")

    result = db.get_openings(days=1)

    assert result == [
        {
            "date": "2026-03-27",
            "door": [
                {"open": "09:00", "close": "09:05"},
                {"open": "12:00", "close": "15:00"},
            ],
            "window": [],
        }
    ]


def test_get_openings_seeds_from_pre_cutoff_open(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_device_event(db, "2026-03-26 23:00:00", "door", "open")
    _insert_device_event(db, "2026-03-27 09:00:00", "door", "closed")

    result = db.get_openings(days=1)

    assert result == [
        {
            "date": "2026-03-27",
            "door": [{"open": "00:00", "close": "09:00"}],
            "window": [],
        }
    ]


def test_get_openings_splits_midnight_span(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    fixed_now = datetime(2026, 3, 27, 15, 0, 0)
    _set_fixed_now(monkeypatch, fixed_now)

    _insert_device_event(db, "2026-03-26 22:00:00", "door", "open")
    _insert_device_event(db, "2026-03-27 02:00:00", "door", "closed")

    result = db.get_openings(days=2)

    assert result == [
        {
            "date": "2026-03-26",
            "door": [{"open": "22:00", "close": "23:59"}],
            "window": [],
        },
        {
            "date": "2026-03-27",
            "door": [{"open": "00:00", "close": "02:00"}],
            "window": [],
        },
    ]
