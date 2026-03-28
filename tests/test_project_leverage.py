import asyncio
import json
import sqlite3
import sys
import types
from datetime import datetime
from pathlib import Path

import pytest
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

from project_leverage_collector import collect_project_leverage
from src.database import Database
from src.orchestrator import Orchestrator


def _create_tool_usage_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.execute(
        """
        CREATE TABLE tool_usage (
            session_id TEXT,
            session_name TEXT,
            project_name TEXT,
            tool_name TEXT,
            bash_command TEXT,
            target_file TEXT,
            hook_type TEXT,
            timestamp TEXT
        )
        """
    )
    conn.commit()
    conn.close()


def _insert_tool_usage(
    path: Path,
    *,
    session_id: str,
    project_name: str,
    tool_name: str,
    timestamp: str,
    bash_command: str | None = None,
    target_file: str | None = None,
    hook_type: str = "PreToolUse",
) -> None:
    conn = sqlite3.connect(path)
    conn.execute(
        """
        INSERT INTO tool_usage (
            session_id,
            session_name,
            project_name,
            tool_name,
            bash_command,
            target_file,
            hook_type,
            timestamp
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            session_id,
            "engineer",
            project_name,
            tool_name,
            bash_command,
            target_file,
            hook_type,
            timestamp,
        ),
    )
    conn.commit()
    conn.close()


def _create_engram_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.execute(
        """
        CREATE TABLE dispatches (
            created_at TEXT,
            state TEXT
        )
        """
    )
    conn.commit()
    conn.close()


def _insert_dispatch(path: Path, created_at: str, state: str) -> None:
    conn = sqlite3.connect(path)
    conn.execute(
        "INSERT INTO dispatches (created_at, state) VALUES (?, ?)",
        (created_at, state),
    )
    conn.commit()
    conn.close()


def _project_metric(db: Database, date: str, project: str, metric: str) -> float | None:
    with db._connection() as conn:
        row = conn.execute(
            """
            SELECT value
            FROM project_leverage
            WHERE date = ? AND project = ? AND metric = ?
            """,
            (date, project, metric),
        ).fetchone()
    return None if row is None else row["value"]


def test_collects_sm_command_metrics(tmp_path):
    db_path = tmp_path / "office.db"
    tool_usage_db = tmp_path / "tool_usage.db"
    Database(db_path)
    _create_tool_usage_db(tool_usage_db)

    for index in range(5):
        _insert_tool_usage(
            tool_usage_db,
            session_id=f"s{index}",
            project_name="office-automate",
            tool_name="Bash",
            bash_command=f"sm send agent-{index} hello",
            timestamp="2026-03-27 09:00:00",
        )
    for index in range(3):
        _insert_tool_usage(
            tool_usage_db,
            session_id=f"d{index}",
            project_name="office-automate",
            tool_name="Bash",
            bash_command="sm dispatch engineer ticket-30",
            timestamp="2026-03-27 10:00:00",
        )
    _insert_tool_usage(
        tool_usage_db,
        session_id="ignored",
        project_name="office-automate",
        tool_name="Bash",
        bash_command="sm send ignored post-tool",
        hook_type="PostToolUse",
        timestamp="2026-03-27 11:00:00",
    )

    collect_project_leverage(
        db_path=db_path,
        tool_usage_db_path=tool_usage_db,
        engram_db_path=tmp_path / "missing_engram.db",
        concept_registry_path=tmp_path / "missing_registry.md",
        now=datetime(2026, 3, 27, 12, 0, 0),
    )

    db = Database(db_path)
    assert _project_metric(db, "2026-03-27", "session-manager", "sm_sends") == 5
    assert _project_metric(db, "2026-03-27", "session-manager", "sm_dispatches") == 3


def test_collects_agent_os_persona_reads(tmp_path):
    db_path = tmp_path / "office.db"
    tool_usage_db = tmp_path / "tool_usage.db"
    Database(db_path)
    _create_tool_usage_db(tool_usage_db)

    for session_id, project_name in [
        ("s1", "office-automate"),
        ("s2", "office-automate"),
        ("s3", "session-manager"),
        ("s4", "session-manager"),
    ]:
        _insert_tool_usage(
            tool_usage_db,
            session_id=session_id,
            project_name=project_name,
            tool_name="Read",
            target_file="/Users/rajesh/.agent-os/personas/engineer.md",
            timestamp="2026-03-27 09:30:00",
        )

    collect_project_leverage(
        db_path=db_path,
        tool_usage_db_path=tool_usage_db,
        engram_db_path=tmp_path / "missing_engram.db",
        concept_registry_path=tmp_path / "missing_registry.md",
        now=datetime(2026, 3, 27, 12, 0, 0),
    )

    db = Database(db_path)
    assert _project_metric(db, "2026-03-27", "agent-os", "persona_reads") == 4
    assert _project_metric(db, "2026-03-27", "agent-os", "persona_projects") == 2


def test_collects_agent_os_persona_projects_with_fractal_normalization(tmp_path):
    db_path = tmp_path / "office.db"
    tool_usage_db = tmp_path / "tool_usage.db"
    Database(db_path)
    _create_tool_usage_db(tool_usage_db)

    for session_id, project_name in [
        ("s1", "fractal-market-simulator"),
        ("s2", "fractal-1808-em"),
        ("s3", "fractal-1812-fix"),
    ]:
        _insert_tool_usage(
            tool_usage_db,
            session_id=session_id,
            project_name=project_name,
            tool_name="Read",
            target_file="/Users/rajesh/.agent-os/personas/engineer.md",
            timestamp="2026-03-27 09:30:00",
        )

    collect_project_leverage(
        db_path=db_path,
        tool_usage_db_path=tool_usage_db,
        engram_db_path=tmp_path / "missing_engram.db",
        concept_registry_path=tmp_path / "missing_registry.md",
        now=datetime(2026, 3, 27, 12, 0, 0),
    )

    db = Database(db_path)
    assert _project_metric(db, "2026-03-27", "agent-os", "persona_projects") == 1
    assert _project_metric(db, "2026-03-27", "agent-os", "persona_project::fractal") == 1


def test_collects_engram_fold_metrics(tmp_path):
    db_path = tmp_path / "office.db"
    engram_db = tmp_path / "engram_state.db"
    registry_path = tmp_path / "engram_concept_registry.md"
    Database(db_path)
    _create_engram_db(engram_db)
    _insert_dispatch(engram_db, "2026-03-27 10:00:00", "committed")
    _insert_dispatch(engram_db, "2026-03-20 10:00:00", "building")
    registry_path.write_text(
        "\n".join([
            "## C001: First concept (ACTIVE)",
            "## C002: Retired concept (DEAD)",
            "## C003: Second concept (ACTIVE)",
        ]),
        encoding="utf-8",
    )

    collect_project_leverage(
        db_path=db_path,
        tool_usage_db_path=tmp_path / "missing_tool_usage.db",
        engram_db_path=engram_db,
        concept_registry_path=registry_path,
        now=datetime(2026, 3, 27, 12, 0, 0),
    )

    db = Database(db_path)
    assert _project_metric(db, "2026-03-27", "engram", "engram_last_fold_age_hours") == pytest.approx(2.0, rel=1e-3)
    assert _project_metric(db, "2026-03-27", "engram", "engram_folds_7d") == 1
    assert _project_metric(db, "2026-03-27", "engram", "engram_active_concepts") == 2


def test_project_leverage_endpoint_returns_all_projects(monkeypatch, tmp_path):
    db_path = tmp_path / "office.db"
    db = Database(db_path)
    fixed_now = datetime(2026, 3, 27, 12, 0, 0)
    monkeypatch.setattr(Database, "_now", staticmethod(lambda: fixed_now))

    db.upsert_project_leverage([
        ("2026-03-27", "session-manager", "sm_dispatches", 12),
        ("2026-03-27", "session-manager", "sm_sends", 45),
        ("2026-03-27", "session-manager", "sm_reminds", 8),
        ("2026-03-27", "session-manager", "sm_active_sessions", 18),
        ("2026-03-27", "session-manager", "sm_telegram_in", 23),
        ("2026-03-27", "session-manager", "sm_telegram_out", 19),
        ("2026-03-27", "engram", "engram_last_fold_age_hours", 3.5),
        ("2026-03-27", "engram", "engram_folds_7d", 12),
        ("2026-03-27", "engram", "engram_active_concepts", 42),
        ("2026-03-27", "agent-os", "persona_reads", 28),
        ("2026-03-27", "agent-os", "persona_projects", 4),
        ("2026-03-27", "agent-os", "persona_project::office-automate", 1),
        ("2026-03-27", "agent-os", "persona_project::session-manager", 1),
        ("2026-03-27", "agent-os", "persona_project::engram", 1),
        ("2026-03-27", "agent-os", "persona_project::taskbar", 1),
        ("2026-03-27", "office-automate", "automation_events", 45),
        ("2026-03-27", "office-automate", "state_transitions", 12),
    ])

    orchestrator = Orchestrator.__new__(Orchestrator)
    orchestrator.db = db
    request = make_mocked_request("GET", "/history/project-leverage?days=7")

    response = asyncio.run(orchestrator._handle_history_project_leverage_get(request))
    payload = json.loads(response.body.decode("utf-8"))

    assert response.status == 200
    assert set(payload["projects"]) == {
        "session-manager",
        "engram",
        "agent-os",
        "office-automate",
    }
    assert payload["projects"]["session-manager"]["week"]["sm_dispatches"] == 12
    assert payload["projects"]["engram"]["current"] == {
        "last_fold_age_hours": 3.5,
        "folds_7d": 12,
        "active_concepts": 42,
    }
    assert payload["projects"]["agent-os"]["week"] == {
        "persona_reads": 28,
        "persona_projects": 4,
    }
    assert payload["projects"]["office-automate"]["week"] == {
        "automation_events": 45,
        "state_transitions": 12,
    }
