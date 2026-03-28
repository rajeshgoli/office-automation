import json

from session_stats_parser import import_session_meta
from src.database import Database


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


def test_import_session_meta_basic_import_filters_zero_activity_artifacts(tmp_path):
    db_path = tmp_path / "history.db"
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

    imported = import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    assert imported == 2

    db = Database(db_path)
    with db._connection() as conn:
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
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload())

    first_import = import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)
    second_import = import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    assert first_import == 1
    assert second_import == 1

    db = Database(db_path)
    with db._connection() as conn:
        assert conn.execute("SELECT COUNT(*) FROM session_output").fetchone()[0] == 1


def test_import_session_meta_upserts_when_file_updates(tmp_path):
    db_path = tmp_path / "history.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()
    session_path = session_meta_dir / "session-1.json"

    _write_session_meta(session_path, _session_payload(lines_added=10, git_commits=0))
    import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    _write_session_meta(session_path, _session_payload(lines_added=250, git_commits=3))
    import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    db = Database(db_path)
    with db._connection() as conn:
        row = conn.execute("""
            SELECT lines_added, git_commits
            FROM session_output
            WHERE session_id = 'session-1'
        """).fetchone()

    assert tuple(row) == (250, 3)


def test_import_session_meta_converts_utc_to_pacific_time(tmp_path):
    db_path = tmp_path / "history.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "session-1.json",
        _session_payload(start_time="2026-01-01T01:05:00Z"),
    )

    import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    db = Database(db_path)
    with db._connection() as conn:
        row = conn.execute("SELECT start_time FROM session_output").fetchone()

    assert row["start_time"] == "2025-12-31 17:05:00"


def test_import_session_meta_normalizes_project_names(tmp_path):
    db_path = tmp_path / "history.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(
        session_meta_dir / "session-1.json",
        _session_payload(project_path="/Users/rajesh/Desktop/automation/office-automation"),
    )

    import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    db = Database(db_path)
    with db._connection() as conn:
        row = conn.execute("SELECT project FROM session_output").fetchone()

    assert row["project"] == "office-automate"


def test_import_session_meta_marks_machine_generated_sessions(tmp_path):
    db_path = tmp_path / "history.db"
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

    import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    db = Database(db_path)
    with db._connection() as conn:
        rows = conn.execute("""
            SELECT session_id, is_human_session
            FROM session_output
            ORDER BY session_id
        """).fetchall()

    assert [tuple(row) for row in rows] == [("human-1", 1), ("machine-1", 0)]


def test_import_session_meta_skips_malformed_json_files(tmp_path, caplog):
    db_path = tmp_path / "history.db"
    session_meta_dir = tmp_path / "session-meta"
    session_meta_dir.mkdir()

    _write_session_meta(session_meta_dir / "session-1.json", _session_payload())
    with (session_meta_dir / "broken.json").open("w", encoding="utf-8") as handle:
        handle.write('{"session_id":"broken",')

    imported = import_session_meta(db_path=db_path, session_meta_dir=session_meta_dir)

    assert imported == 1
    assert "Skipping malformed JSON" in caplog.text

    db = Database(db_path)
    with db._connection() as conn:
        assert conn.execute("SELECT COUNT(*) FROM session_output").fetchone()[0] == 1
