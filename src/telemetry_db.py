"""Helpers for the isolated session telemetry database."""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Iterable, Iterator, Optional

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TELEMETRY_DB_PATH = REPO_ROOT / "data" / "telemetry.db"
DEFAULT_LEGACY_DB_PATH = REPO_ROOT / "data" / "office_climate.db"

SESSION_OUTPUT_COLUMNS = """
    session_id,
    project,
    start_time,
    duration_minutes,
    lines_added,
    lines_removed,
    files_modified,
    git_commits,
    git_pushes,
    user_message_count,
    assistant_message_count,
    input_tokens,
    output_tokens,
    tool_counts,
    languages,
    is_human_session
"""

SESSION_OUTPUT_SCHEMA = f"""
    CREATE TABLE IF NOT EXISTS session_output (
        session_id TEXT PRIMARY KEY,
        project TEXT NOT NULL DEFAULT 'unknown',
        start_time DATETIME NOT NULL,
        duration_minutes INTEGER NOT NULL DEFAULT 0,
        lines_added INTEGER NOT NULL DEFAULT 0,
        lines_removed INTEGER NOT NULL DEFAULT 0,
        files_modified INTEGER NOT NULL DEFAULT 0,
        git_commits INTEGER NOT NULL DEFAULT 0,
        git_pushes INTEGER NOT NULL DEFAULT 0,
        user_message_count INTEGER NOT NULL DEFAULT 0,
        assistant_message_count INTEGER NOT NULL DEFAULT 0,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        tool_counts TEXT,
        languages TEXT,
        is_human_session INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX IF NOT EXISTS idx_session_output_start ON session_output(start_time);
    CREATE INDEX IF NOT EXISTS idx_session_output_project ON session_output(project);
"""

SessionOutputRow = tuple[
    str,
    str,
    str,
    int,
    int,
    int,
    int,
    int,
    int,
    int,
    int,
    int,
    int,
    Optional[str],
    Optional[str],
    int,
]


def ensure_telemetry_db(db_path: Path = DEFAULT_TELEMETRY_DB_PATH) -> None:
    """Create the telemetry DB and schema when needed."""
    db_path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(db_path) as conn:
        conn.executescript(SESSION_OUTPUT_SCHEMA)


@contextmanager
def telemetry_connection(
    db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
) -> Iterator[sqlite3.Connection]:
    """Yield a row-factory SQLite connection to telemetry.db."""
    ensure_telemetry_db(db_path)
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        yield conn
        conn.commit()
    finally:
        conn.close()


def replace_session_output_rows(
    rows: Iterable[SessionOutputRow],
    db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
) -> int:
    """Insert or replace session output rows."""
    payload = list(rows)
    if not payload:
        return 0

    with telemetry_connection(db_path) as conn:
        conn.executemany(
            f"""
            INSERT OR REPLACE INTO session_output ({SESSION_OUTPUT_COLUMNS})
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            payload,
        )
    return len(payload)


def upsert_collector_session_output_rows(
    rows: Iterable[SessionOutputRow],
    db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
) -> int:
    """Upsert collector-derived rows without overwriting richer session-meta rows."""
    payload = list(rows)
    if not payload:
        return 0

    with telemetry_connection(db_path) as conn:
        conn.executemany(
            f"""
            INSERT INTO session_output ({SESSION_OUTPUT_COLUMNS})
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(session_id) DO UPDATE SET
                project = excluded.project,
                start_time = excluded.start_time,
                duration_minutes = excluded.duration_minutes,
                lines_added = excluded.lines_added,
                lines_removed = excluded.lines_removed,
                files_modified = excluded.files_modified,
                git_commits = excluded.git_commits,
                git_pushes = excluded.git_pushes,
                tool_counts = excluded.tool_counts,
                is_human_session = excluded.is_human_session
            WHERE session_output.user_message_count = 0
              AND session_output.assistant_message_count = 0
              AND session_output.input_tokens = 0
              AND session_output.output_tokens = 0
            """,
            payload,
        )
    return len(payload)


def migrate_legacy_session_output(
    legacy_db_path: Path = DEFAULT_LEGACY_DB_PATH,
    telemetry_db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
) -> int:
    """Move legacy session_output rows out of office_climate.db and drop the old table."""
    if legacy_db_path.resolve() == telemetry_db_path.resolve():
        return 0

    ensure_telemetry_db(telemetry_db_path)
    if not legacy_db_path.exists():
        return 0

    conn = sqlite3.connect(legacy_db_path)
    attached = False
    try:
        row = conn.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_output'"
        ).fetchone()
        if row is None:
            return 0

        before = conn.execute(
            "SELECT COUNT(*) FROM session_output"
        ).fetchone()[0]
        conn.execute("ATTACH DATABASE ? AS telemetry", (str(telemetry_db_path),))
        attached = True
        conn.execute(f"""
            INSERT OR IGNORE INTO telemetry.session_output ({SESSION_OUTPUT_COLUMNS})
            SELECT {SESSION_OUTPUT_COLUMNS}
            FROM session_output
        """)
        conn.execute("DROP TABLE IF EXISTS session_output")
        conn.commit()
        return int(before or 0)
    finally:
        if attached:
            conn.execute("DETACH DATABASE telemetry")
        conn.close()
