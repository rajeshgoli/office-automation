import json
import sqlite3
import subprocess
from pathlib import Path

from session_stats_parser import collect_github_prs, import_history, import_session_meta
from src.database import Database
from src.telemetry_db import SESSION_OUTPUT_SCHEMA, telemetry_connection


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


def _runner_from_payloads(payloads_by_command: dict[tuple[str, ...], object]):
    def _runner(cmd, check, capture_output, text):
        key = tuple(cmd[1:])
        if key not in payloads_by_command:
            raise AssertionError(f"Unexpected gh command: {cmd}")

        return subprocess.CompletedProcess(
            args=cmd,
            returncode=0,
            stdout=json.dumps(payloads_by_command[key]),
            stderr="",
        )

    return _runner


def _write_session_meta(path, payload) -> None:
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle)


def _session_payload(**overrides):
    payload = {
        "session_id": "session-1",
        "project_path": "/Users/rajesh/Desktop/automation/office-automate",
        "start_time": "2026-01-15T02:30:00Z",
        "duration_minutes": 25,
        "lines_added": 120,
        "lines_removed": 15,
        "files_modified": 4,
        "git_commits": 2,
        "git_pushes": 1,
        "user_message_count": 3,
        "assistant_message_count": 6,
        "input_tokens": 1000,
        "output_tokens": 2000,
        "tool_counts": {"Read": 5, "Edit": 2},
        "languages": {"Python": 120},
        "first_prompt": "ship the feature",
    }
    payload.update(overrides)
    return payload


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


def test_collect_github_prs_imports_prs_across_repos(tmp_path):
    db_path = tmp_path / "history.db"
    runner = _runner_from_payloads({
        ("repo", "list", "rajeshgoli", "--json", "name", "--limit", "100"): [
            {"name": "office-automate"},
            {"name": "session-manager"},
        ],
        (
            "pr",
            "list",
            "--repo",
            "rajeshgoli/office-automate",
            "--state",
            "all",
            "--json",
            "number,title,state,additions,deletions,changedFiles,createdAt,mergedAt",
            "--limit",
            "500",
        ): [
            {
                "number": 26,
                "title": "B: GitHub PR pipeline",
                "state": "MERGED",
                "additions": 120,
                "deletions": 15,
                "changedFiles": 4,
                "createdAt": "2026-03-28T00:00:00Z",
                "mergedAt": "2026-03-28T02:30:00Z",
            },
        ],
        (
            "pr",
            "list",
            "--repo",
            "rajeshgoli/session-manager",
            "--state",
            "all",
            "--json",
            "number,title,state,additions,deletions,changedFiles,createdAt,mergedAt",
            "--limit",
            "500",
        ): [
            {
                "number": 101,
                "title": "Track telegram telemetry",
                "state": "OPEN",
                "additions": 45,
                "deletions": 3,
                "changedFiles": 2,
                "createdAt": "2026-03-27T18:00:00Z",
                "mergedAt": None,
            },
        ],
    })

    count = collect_github_prs(db_path=db_path, runner=runner)

    assert count == 2

    db = Database(db_path)
    with db._connection() as conn:
        rows = conn.execute("""
            SELECT repo, pr_number, state
            FROM github_prs
            ORDER BY repo, pr_number
        """).fetchall()

    assert [tuple(row) for row in rows] == [
        ("office-automate", 26, "MERGED"),
        ("session-manager", 101, "OPEN"),
    ]


def test_import_session_meta_basic_import_filters_zero_activity_artifacts(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload())
    _write_session_meta(
        session_meta_dir / "session-2.json",
        _session_payload(
            session_id="session-2",
            project_path="/Users/rajesh/Desktop/taskbar",
            start_time="2026-01-15T04:00:00Z",
            duration_minutes=10,
            user_message_count=1,
            lines_added=40,
            lines_removed=5,
            files_modified=2,
            git_commits=1,
            git_pushes=0,
            input_tokens=300,
            output_tokens=700,
            tool_counts={"Bash": 3},
            languages={"TypeScript": 45},
        ),
    )
    _write_session_meta(
        session_meta_dir / "artifact.json",
        _session_payload(
            session_id="artifact",
            duration_minutes=0,
            user_message_count=0,
            lines_added=0,
            lines_removed=0,
            files_modified=0,
            git_commits=0,
            git_pushes=0,
        ),
    )

    imported = import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    assert imported == 2

    with telemetry_connection(telemetry_db_path) as conn:
        rows = conn.execute("""
            SELECT session_id, project, start_time, lines_added, lines_removed, tool_counts, languages
            FROM session_output
            ORDER BY session_id
        """).fetchall()

    assert [tuple(row[:5]) for row in rows] == [
        ("session-1", "office-automate", "2026-01-14 18:30:00", 120, 15),
        ("session-2", "taskbar", "2026-01-14 20:00:00", 40, 5),
    ]
    assert json.loads(rows[0]["tool_counts"]) == {"Read": 5, "Edit": 2}
    assert json.loads(rows[1]["languages"]) == {"TypeScript": 45}


def test_import_session_meta_is_idempotent(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload())

    first_import = import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )
    second_import = import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    assert first_import == 1
    assert second_import == 1

    with telemetry_connection(telemetry_db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM session_output").fetchone()[0] == 1


def test_import_session_meta_upserts_when_file_updates(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()
    session_path = session_meta_dir / "session-1.json"

    _write_session_meta(session_path, _session_payload(lines_added=10, git_commits=0))
    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    _write_session_meta(session_path, _session_payload(lines_added=250, git_commits=3))
    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    with telemetry_connection(telemetry_db_path) as conn:
        row = conn.execute("""
            SELECT lines_added, git_commits
            FROM session_output
            WHERE session_id = 'session-1'
        """).fetchone()

    assert tuple(row) == (250, 3)


def test_import_session_meta_converts_utc_to_los_angeles_time(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "session-1.json",
        _session_payload(start_time="2026-01-01T01:05:00Z"),
    )
    _write_session_meta(
        session_meta_dir / "session-2.json",
        _session_payload(
            session_id="session-2",
            start_time="2026-07-01T01:05:00Z",
        ),
    )

    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    with telemetry_connection(telemetry_db_path) as conn:
        rows = conn.execute("""
            SELECT session_id, start_time
            FROM session_output
            ORDER BY session_id
        """).fetchall()

    assert [tuple(row) for row in rows] == [
        ("session-1", "2025-12-31 17:05:00"),
        ("session-2", "2026-06-30 18:05:00"),
    ]


def test_import_session_meta_normalizes_project_names(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "session-1.json",
        _session_payload(project_path="/Users/rajesh/Desktop/automation/office-automation"),
    )

    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    with telemetry_connection(telemetry_db_path) as conn:
        row = conn.execute("SELECT project FROM session_output").fetchone()

    assert row["project"] == "office-automate"


def test_import_session_meta_collapses_fractal_worktrees(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "session-1.json",
        _session_payload(project_path="/Users/rajesh/worktrees/fractal-1808-em"),
    )

    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    with telemetry_connection(telemetry_db_path) as conn:
        row = conn.execute("SELECT project FROM session_output").fetchone()

    assert row["project"] == "fractal"


def test_import_session_meta_marks_machine_generated_sessions(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "machine.json",
        _session_payload(
            session_id="machine-1",
            first_prompt="[Input from: agent-1 (abcd1234) via sm send]",
        ),
    )
    _write_session_meta(
        session_meta_dir / "human.json",
        _session_payload(
            session_id="human-1",
            first_prompt="implement the parser",
            start_time="2026-01-15T03:30:00Z",
        ),
    )

    import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    with telemetry_connection(telemetry_db_path) as conn:
        rows = conn.execute("""
            SELECT session_id, is_human_session
            FROM session_output
            ORDER BY session_id
        """).fetchall()

    assert [tuple(row) for row in rows] == [("human-1", 1), ("machine-1", 0)]


def test_import_session_meta_skips_malformed_json_files(tmp_path, caplog):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload())
    with (session_meta_dir / "broken.json").open("w", encoding="utf-8") as handle:
        handle.write('{"session_id":"broken",')

    imported = import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    assert imported == 1
    assert "Skipping malformed JSON" in caplog.text

    with telemetry_connection(telemetry_db_path) as conn:
        assert conn.execute("SELECT COUNT(*) FROM session_output").fetchone()[0] == 1


def test_import_session_meta_migrates_legacy_rows_to_telemetry_db(tmp_path):
    db_path = tmp_path / "history.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    db = Database(db_path)
    with db._connection() as conn:
        conn.executescript(SESSION_OUTPUT_SCHEMA)
        conn.execute(
            """
            INSERT INTO session_output (
                session_id, project, start_time, duration_minutes, lines_added, lines_removed,
                files_modified, git_commits, git_pushes, user_message_count,
                assistant_message_count, input_tokens, output_tokens, tool_counts,
                languages, is_human_session
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                "legacy-session",
                "office-automate",
                "2026-01-14 18:30:00",
                25,
                120,
                15,
                4,
                2,
                1,
                3,
                6,
                1000,
                2000,
                json.dumps({"Read": 5}),
                json.dumps({"Python": 120}),
                1,
            ),
        )

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload(session_id="fresh-session"))

    imported = import_session_meta(
        db_path=db_path,
        telemetry_db_path=telemetry_db_path,
        session_meta_dir=session_meta_dir,
    )

    assert imported == 1

    with telemetry_connection(telemetry_db_path) as conn:
        rows = conn.execute(
            "SELECT session_id FROM session_output ORDER BY session_id"
        ).fetchall()

    assert [row["session_id"] for row in rows] == ["fresh-session", "legacy-session"]

    with db._connection() as conn:
        row = conn.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_output'"
        ).fetchone()

    assert row is None
