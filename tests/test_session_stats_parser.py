import json
import sqlite3
from pathlib import Path

from session_stats_parser import import_history
from src.database import Database


def _write_jsonl(path: Path, rows: list[object]) -> None:
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            if isinstance(row, str):
                handle.write(row)
            else:
                handle.write(json.dumps(row))
            handle.write("\n")


def _create_codex_state(path: Path, threads: dict[str, str]) -> None:
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE threads (id TEXT PRIMARY KEY, cwd TEXT)")
    for session_id, cwd in threads.items():
        conn.execute(
            "INSERT INTO threads (id, cwd) VALUES (?, ?)",
            (session_id, cwd),
        )
    conn.commit()
    conn.close()


def test_import_history_filters_machine_messages_and_is_idempotent(tmp_path):
    db_path = tmp_path / "history.db"
    claude_history = tmp_path / "claude_history.jsonl"
    codex_history = tmp_path / "codex_history.jsonl"
    codex_state = tmp_path / "codex_state.sqlite"

    _write_jsonl(claude_history, [
        {
            "display": "ship the feature",
            "pastedContents": {},
            "timestamp": 1774676562599,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        {
            "display": "[sm remind] Update your status",
            "pastedContents": {},
            "timestamp": 1774676563000,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        {
            "display": "[Input from: agent-1 (d7436972) via sm send]",
            "pastedContents": {},
            "timestamp": 1774676564000,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        {
            "display": "[sm] Scheduled reminder: check PR",
            "pastedContents": {},
            "timestamp": 1774676565000,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        {
            "display": "[Pasted text #1] some code here",
            "pastedContents": {},
            "timestamp": 1774676566000,
            "project": "/Users/rajesh/Desktop/automation/taskbar",
            "sessionId": "claude-2",
        },
        {
            "display": "[Image #1]",
            "pastedContents": {},
            "timestamp": 1774676567000,
            "project": "/Users/rajesh/Desktop/automation/taskbar",
            "sessionId": "claude-2",
        },
        {
            "display": "ship it",
            "pastedContents": {},
            "timestamp": 1774676568000,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-3",
        },
        {
            "display": "final prompt",
            "pastedContents": {},
            "timestamp": 1774676569000,
            "project": "/Users/rajesh/Desktop/automation/taskbar",
            "sessionId": "claude-4",
        },
    ])
    _write_jsonl(codex_history, [])
    _create_codex_state(codex_state, {})

    counts = import_history(
        db_path=db_path,
        claude_history_path=claude_history,
        codex_history_path=codex_history,
        codex_state_path=codex_state,
    )

    assert counts == {"claude": 5, "codex": 0}

    db = Database(db_path)
    with db._connection() as conn:
        rows = conn.execute("""
            SELECT timestamp, tool, project, session_id
            FROM orchestration_activity
            ORDER BY timestamp
        """).fetchall()

    assert [tuple(row) for row in rows] == [
        ("2026-03-27 22:42:42", "claude", "office-automate", "claude-1"),
        ("2026-03-27 22:42:46", "claude", "taskbar", "claude-2"),
        ("2026-03-27 22:42:47", "claude", "taskbar", "claude-2"),
        ("2026-03-27 22:42:48", "claude", "office-automate", "claude-3"),
        ("2026-03-27 22:42:49", "claude", "taskbar", "claude-4"),
    ]

    second_counts = import_history(
        db_path=db_path,
        claude_history_path=claude_history,
        codex_history_path=codex_history,
        codex_state_path=codex_state,
    )

    assert second_counts == {"claude": 0, "codex": 0}
    with db._connection() as conn:
        assert conn.execute("SELECT COUNT(*) FROM orchestration_activity").fetchone()[0] == 5


def test_import_history_retries_after_malformed_trailing_line(tmp_path, caplog):
    db_path = tmp_path / "history.db"
    claude_history = tmp_path / "claude_history.jsonl"
    codex_history = tmp_path / "codex_history.jsonl"
    codex_state = tmp_path / "codex_state.sqlite"

    _write_jsonl(claude_history, [
        {
            "display": "good line",
            "pastedContents": {},
            "timestamp": 1774676562599,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        '{"display":"truncated","times',
    ])
    _write_jsonl(codex_history, [])
    _create_codex_state(codex_state, {})

    counts = import_history(
        db_path=db_path,
        claude_history_path=claude_history,
        codex_history_path=codex_history,
        codex_state_path=codex_state,
    )
    assert counts == {"claude": 1, "codex": 0}
    assert "Skipping malformed JSONL line 2" in caplog.text

    _write_jsonl(claude_history, [
        {
            "display": "good line",
            "pastedContents": {},
            "timestamp": 1774676562599,
            "project": "/Users/rajesh/Desktop/automation/office-automate",
            "sessionId": "claude-1",
        },
        {
            "display": "new valid line",
            "pastedContents": {},
            "timestamp": 1774676570000,
            "project": "/Users/rajesh/Desktop/automation/taskbar",
            "sessionId": "claude-2",
        },
    ])

    second_counts = import_history(
        db_path=db_path,
        claude_history_path=claude_history,
        codex_history_path=codex_history,
        codex_state_path=codex_state,
    )
    assert second_counts == {"claude": 1, "codex": 0}

    db = Database(db_path)
    with db._connection() as conn:
        rows = conn.execute(
            "SELECT session_id FROM orchestration_activity ORDER BY timestamp"
        ).fetchall()
    assert [row["session_id"] for row in rows] == ["claude-1", "claude-2"]


def test_import_history_uses_unknown_project_for_missing_codex_thread(tmp_path):
    db_path = tmp_path / "history.db"
    claude_history = tmp_path / "claude_history.jsonl"
    codex_history = tmp_path / "codex_history.jsonl"
    codex_state = tmp_path / "codex_state.sqlite"

    _write_jsonl(claude_history, [])
    _write_jsonl(codex_history, [
        {
            "session_id": "codex-1",
            "ts": 1774674087,
            "text": "ok PR the changes you made for crash fix",
        }
    ])
    _create_codex_state(codex_state, {})

    counts = import_history(
        db_path=db_path,
        claude_history_path=claude_history,
        codex_history_path=codex_history,
        codex_state_path=codex_state,
    )

    assert counts == {"claude": 0, "codex": 1}

    db = Database(db_path)
    with db._connection() as conn:
        row = conn.execute("""
            SELECT timestamp, tool, project, session_id
            FROM orchestration_activity
        """).fetchone()

    assert tuple(row) == ("2026-03-27 22:01:27", "codex", "unknown", "codex-1")
