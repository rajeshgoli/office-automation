import asyncio
import json
import sys
import types
from datetime import datetime
from pathlib import Path

from aiohttp.test_utils import make_mocked_request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.modules.setdefault(
    "aiomqtt",
    types.SimpleNamespace(Message=object, Client=object),
)
sys.modules.setdefault(
    "tinytuya",
    types.SimpleNamespace(Device=object, Cloud=object),
)

from src.database import Database
from src.orchestrator import Orchestrator


def _set_fixed_now(monkeypatch, now: datetime) -> None:
    monkeypatch.setattr(Database, "_now", staticmethod(lambda: now))


def _insert_orchestration(
    db: Database,
    timestamp: str,
    *,
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


def _session_row(
    session_id: str,
    start_time: str,
    *,
    duration_minutes: int,
    lines_added: int,
    lines_removed: int,
    files_modified: int,
    git_commits: int,
    is_human_session: int = 1,
) -> tuple[str, str, str, int, int, int, int, int, int, int, int, int, int, None, None, int]:
    return (
        session_id,
        "office-automate",
        start_time,
        duration_minutes,
        lines_added,
        lines_removed,
        files_modified,
        git_commits,
        0,
        1,
        1,
        0,
        0,
        None,
        None,
        is_human_session,
    )


def _pr_row(
    pr_number: int,
    created_at: str,
    merged_at: str | None,
    *,
    state: str = "MERGED",
) -> tuple[str, int, str, str, int, int, int, str, str | None]:
    return (
        "rajeshgoli/office-automation",
        pr_number,
        f"PR {pr_number}",
        state,
        0,
        0,
        0,
        created_at,
        merged_at,
    )


def _call_leverage_endpoint(db: Database, days: int) -> tuple[int, dict]:
    orchestrator = Orchestrator.__new__(Orchestrator)
    orchestrator.db = db
    request = make_mocked_request("GET", f"/history/leverage?days={days}")
    response = asyncio.run(orchestrator._handle_history_leverage_get(request))
    return response.status, json.loads(response.body.decode("utf-8"))


def test_leverage_endpoint_basic_computation(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    _set_fixed_now(monkeypatch, datetime(2026, 3, 27, 12, 0, 0))

    for minute in range(5):
        _insert_orchestration(
            db,
            f"2026-03-27 09:0{minute}:00",
            tool="claude",
            project="office-automate",
            session_id="human-session",
        )
        _insert_orchestration(
            db,
            f"2026-03-27 10:0{minute}:00",
            tool="codex",
            project="office-automate",
            session_id="agent-session",
        )

    db.replace_session_output([
        _session_row(
            "human-session",
            "2026-03-27 09:00:00",
            duration_minutes=30,
            lines_added=150,
            lines_removed=20,
            files_modified=4,
            git_commits=5,
        ),
        _session_row(
            "agent-session",
            "2026-03-27 10:00:00",
            duration_minutes=20,
            lines_added=50,
            lines_removed=30,
            files_modified=2,
            git_commits=3,
            is_human_session=0,
        ),
    ])
    db.upsert_github_prs([
        _pr_row(27, "2026-03-27 08:00:00", "2026-03-27 10:00:00"),
    ])

    status, payload = _call_leverage_endpoint(db, 1)

    assert status == 200
    assert payload["ok"] is True
    assert payload["days"] == [
        {
            "date": "2026-03-27",
            "prompts": 10,
            "sessions": 2,
            "lines_added": 200,
            "lines_removed": 50,
            "lines_changed": 250,
            "files_modified": 6,
            "commits": 8,
            "prs_merged": 1,
            "prs_opened": 1,
            "avg_pr_cycle_hours": 2.0,
            "lines_per_prompt": 25.0,
            "commits_per_prompt": 0.8,
            "lines_per_session_minute": 5.0,
        }
    ]
    assert payload["week"] == {
        "prompts": 10,
        "sessions": 2,
        "lines_added": 200,
        "lines_removed": 50,
        "lines_changed": 250,
        "files_modified": 6,
        "commits": 8,
        "prs_merged": 1,
        "prs_opened": 1,
        "avg_pr_cycle_hours": 2.0,
        "lines_per_prompt": 25.0,
        "commits_per_prompt": 0.8,
        "lines_per_session_minute": 5.0,
        "active_days": 1,
    }


def test_leverage_endpoint_returns_null_ratios_when_prompt_count_is_zero(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    _set_fixed_now(monkeypatch, datetime(2026, 3, 27, 12, 0, 0))

    db.replace_session_output([
        _session_row(
            "machine-session",
            "2026-03-27 09:00:00",
            duration_minutes=20,
            lines_added=60,
            lines_removed=15,
            files_modified=3,
            git_commits=2,
            is_human_session=0,
        ),
    ])
    db.upsert_github_prs([
        _pr_row(28, "2026-03-27 09:00:00", "2026-03-27 11:00:00"),
    ])

    status, payload = _call_leverage_endpoint(db, 1)
    day = payload["days"][0]

    assert status == 200
    assert day["prompts"] == 0
    assert day["sessions"] == 0
    assert day["lines_changed"] == 75
    assert day["commits"] == 2
    assert day["prs_merged"] == 1
    assert day["lines_per_prompt"] is None
    assert day["commits_per_prompt"] is None
    assert day["lines_per_session_minute"] == 3.75


def test_leverage_endpoint_averages_pr_cycle_hours_for_merged_day(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    _set_fixed_now(monkeypatch, datetime(2026, 3, 27, 12, 0, 0))

    db.upsert_github_prs([
        _pr_row(29, "2026-03-27 08:00:00", "2026-03-27 10:00:00"),
        _pr_row(30, "2026-03-27 09:00:00", "2026-03-27 13:00:00"),
    ])

    status, payload = _call_leverage_endpoint(db, 1)

    assert status == 200
    assert payload["days"][0]["prs_merged"] == 2
    assert payload["days"][0]["avg_pr_cycle_hours"] == 3.0
    assert payload["week"]["avg_pr_cycle_hours"] == 3.0


def test_leverage_endpoint_aggregates_week_from_day_totals(monkeypatch, tmp_path):
    db = Database(tmp_path / "history.db")
    _set_fixed_now(monkeypatch, datetime(2026, 3, 27, 12, 0, 0))

    per_day = [
        ("2026-03-23", 1, 8, 2, 1, 5, "s1"),
        ("2026-03-24", 2, 15, 5, 1, 10, "s2"),
        ("2026-03-25", 3, 30, 15, 2, 15, "s3"),
        ("2026-03-26", 1, 10, 5, 1, 10, "s4"),
        ("2026-03-27", 2, 7, 3, 1, 10, "s5"),
    ]

    session_rows = []
    for date_str, prompts, lines_added, lines_removed, commits, duration, session_id in per_day:
        for prompt_index in range(prompts):
            _insert_orchestration(
                db,
                f"{date_str} 09:{prompt_index:02d}:00",
                tool="claude" if prompt_index % 2 == 0 else "codex",
                project="office-automate",
                session_id=session_id,
            )
        session_rows.append(
            _session_row(
                session_id,
                f"{date_str} 10:00:00",
                duration_minutes=duration,
                lines_added=lines_added,
                lines_removed=lines_removed,
                files_modified=1,
                git_commits=commits,
            )
        )

    db.replace_session_output(session_rows)
    db.upsert_github_prs([
        _pr_row(31, "2026-03-24 09:00:00", "2026-03-24 11:00:00"),
        _pr_row(32, "2026-03-25 08:00:00", "2026-03-25 12:00:00"),
        _pr_row(33, "2026-03-26 10:00:00", None, state="OPEN"),
        _pr_row(34, "2026-03-26 11:00:00", None, state="OPEN"),
        _pr_row(35, "2026-03-27 12:00:00", "2026-03-27 13:00:00"),
    ])

    status, payload = _call_leverage_endpoint(db, 7)

    assert status == 200
    assert [day["date"] for day in payload["days"]] == [
        "2026-03-21",
        "2026-03-22",
        "2026-03-23",
        "2026-03-24",
        "2026-03-25",
        "2026-03-26",
        "2026-03-27",
    ]
    assert payload["week"] == {
        "prompts": 9,
        "sessions": 5,
        "lines_added": 70,
        "lines_removed": 30,
        "lines_changed": 100,
        "files_modified": 5,
        "commits": 6,
        "prs_merged": 3,
        "prs_opened": 5,
        "avg_pr_cycle_hours": 2.33,
        "lines_per_prompt": 11.11,
        "commits_per_prompt": 0.67,
        "lines_per_session_minute": 2.0,
        "active_days": 5,
    }
