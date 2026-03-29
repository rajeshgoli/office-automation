import sqlite3
import os
import subprocess
from datetime import datetime
from pathlib import Path

from collect_session_telemetry import collect_session_telemetry
from src.telemetry_db import replace_session_output_rows, telemetry_connection


def _create_tool_usage_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.execute(
        """
        CREATE TABLE tool_usage (
            session_id TEXT,
            session_name TEXT,
            project_name TEXT,
            tool_name TEXT,
            target_file TEXT,
            bash_command TEXT,
            timestamp TEXT,
            cwd TEXT,
            hook_type TEXT
        )
        """
    )
    conn.commit()
    conn.close()


def _insert_tool_usage_row(db_path: Path, row: tuple) -> None:
    conn = sqlite3.connect(db_path)
    conn.execute(
        """
        INSERT INTO tool_usage (
            session_id, session_name, project_name, tool_name, target_file,
            bash_command, timestamp, cwd, hook_type
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        row,
    )
    conn.commit()
    conn.close()


def _run(cmd: list[str], cwd: Path, env: dict[str, str] | None = None) -> None:
    subprocess.run(cmd, cwd=cwd, env=env, check=True, capture_output=True, text=True)


def _git_env(commit_time: str) -> dict[str, str]:
    env = os.environ.copy()
    env["GIT_AUTHOR_DATE"] = commit_time
    env["GIT_COMMITTER_DATE"] = commit_time
    return env


def _init_repo(repo: Path) -> None:
    _run(["git", "init"], repo)
    _run(["git", "config", "user.name", "Telemetry Test"], repo)
    _run(["git", "config", "user.email", "telemetry@example.com"], repo)


def test_collect_session_telemetry_attributes_commits_and_creates_synthetic_rows(tmp_path):
    repo = tmp_path / "office-automate"
    repo.mkdir()
    _init_repo(repo)

    tracked = repo / "tracked.py"
    tracked.write_text("print('v1')\n", encoding="utf-8")
    _run(["git", "add", "tracked.py"], repo)
    _run(["git", "commit", "-m", "tracked"], repo, env=_git_env("2026-03-27T09:00:10-07:00"))

    tracked.write_text("print('v1')\nprint('v2')\n", encoding="utf-8")
    _run(["git", "add", "tracked.py"], repo)
    _run(["git", "commit", "-m", "manual"], repo, env=_git_env("2026-03-27T11:15:00-07:00"))

    tool_db_path = tmp_path / "tool_usage.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    _create_tool_usage_db(tool_db_path)

    _insert_tool_usage_row(
        tool_db_path,
        (
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Read",
            None,
            None,
            "2026-03-27 08:55:00",
            str(repo),
            "PreToolUse",
        ),
    )
    _insert_tool_usage_row(
        tool_db_path,
        (
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            "git commit -m tracked",
            "2026-03-27 09:00:00",
            str(repo),
            "PreToolUse",
        ),
    )
    _insert_tool_usage_row(
        tool_db_path,
        (
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            "git push origin feature/test",
            "2026-03-27 09:01:00",
            str(repo),
            "PreToolUse",
        ),
    )

    stats = collect_session_telemetry(
        tool_db_path=tool_db_path,
        output_db_path=telemetry_db_path,
        repos=[repo],
        days=30,
        now=datetime(2026, 3, 28, 12, 0, 0),
    )

    assert stats == {
        "sessions": 1,
        "rows_written": 2,
        "synthetic_rows": 1,
        "matched_commits": 1,
    }

    with telemetry_connection(telemetry_db_path) as conn:
        rows = conn.execute(
            """
            SELECT
                session_id, project, lines_added, lines_removed, files_modified,
                git_commits, git_pushes, tool_counts, is_human_session
            FROM session_output
            ORDER BY session_id
            """
        ).fetchall()

    assert rows[0]["session_id"] == "session-1"
    assert rows[0]["project"] == "office-automate"
    assert rows[0]["lines_added"] == 1
    assert rows[0]["lines_removed"] == 0
    assert rows[0]["files_modified"] == 1
    assert rows[0]["git_commits"] == 1
    assert rows[0]["git_pushes"] == 1
    assert rows[0]["tool_counts"] == '{"Bash": 2, "Read": 1}'
    assert rows[0]["is_human_session"] == 1

    assert rows[1]["session_id"] == "unattributed-office-automate-2026-03-27"
    assert rows[1]["lines_added"] == 1
    assert rows[1]["git_commits"] == 1
    assert rows[1]["is_human_session"] == 0


def test_collect_session_telemetry_preserves_richer_session_meta_rows(tmp_path):
    repo = tmp_path / "office-automate"
    repo.mkdir()
    _init_repo(repo)

    tracked = repo / "tracked.py"
    tracked.write_text("print('v1')\n", encoding="utf-8")
    _run(["git", "add", "tracked.py"], repo)
    _run(["git", "commit", "-m", "tracked"], repo, env=_git_env("2026-03-27T09:00:10-07:00"))

    tool_db_path = tmp_path / "tool_usage.db"
    telemetry_db_path = tmp_path / "telemetry.db"
    _create_tool_usage_db(tool_db_path)
    _insert_tool_usage_row(
        tool_db_path,
        (
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            "git commit -m tracked",
            "2026-03-27 09:00:00",
            str(repo),
            "PreToolUse",
        ),
    )

    replace_session_output_rows(
        [
            (
                "session-1",
                "office-automate",
                "2026-03-27 09:00:00",
                25,
                999,
                100,
                4,
                2,
                1,
                3,
                6,
                1000,
                2000,
                '{"Read": 5}',
                '{"Python": 120}',
                1,
            )
        ],
        telemetry_db_path,
    )

    collect_session_telemetry(
        tool_db_path=tool_db_path,
        output_db_path=telemetry_db_path,
        repos=[repo],
        days=30,
        now=datetime(2026, 3, 28, 12, 0, 0),
    )

    with telemetry_connection(telemetry_db_path) as conn:
        row = conn.execute(
            """
            SELECT lines_added, lines_removed, git_commits, input_tokens, output_tokens
            FROM session_output
            WHERE session_id = 'session-1'
            """
        ).fetchone()

    assert tuple(row) == (999, 100, 2, 1000, 2000)
