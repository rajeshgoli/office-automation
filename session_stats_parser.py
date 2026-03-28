#!/usr/bin/env python3
"""Import Claude/Codex session history into the office climate database."""

from __future__ import annotations

import argparse
import json
import logging
import os
import sqlite3
from datetime import datetime
from pathlib import Path
from typing import Iterable, Optional

from src.database import Database, DEFAULT_DB_PATH

logger = logging.getLogger(__name__)

REPO_ROOT = Path(__file__).resolve().parent
DEFAULT_CLAUDE_HISTORY = REPO_ROOT / "data" / "claude_history.jsonl"
DEFAULT_CODEX_HISTORY = REPO_ROOT / "data" / "codex_history.jsonl"
DEFAULT_CODEX_STATE = REPO_ROOT / "data" / "codex_state.sqlite"


def is_machine_generated(text: str) -> bool:
    """Return True when a history entry was injected by session-manager."""
    return text.startswith("[Input from:") or text.startswith("[sm")


class CodexProjectResolver:
    """Resolve Codex session IDs to repo basenames via the state SQLite."""

    def __init__(self, db_path: Path):
        self.db_path = db_path
        self._conn: Optional[sqlite3.Connection] = None

    def __enter__(self) -> "CodexProjectResolver":
        self._conn = sqlite3.connect(self.db_path)
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        if self._conn is not None:
            self._conn.close()
            self._conn = None

    def project_for_session(self, session_id: str) -> str:
        """Return the basename for a Codex thread cwd, or unknown."""
        if self._conn is None:
            return "unknown"

        row = self._conn.execute(
            "SELECT cwd FROM threads WHERE id = ?",
            (session_id,),
        ).fetchone()
        if not row or not row[0]:
            return "unknown"
        return os.path.basename(row[0]) or "unknown"


def _timestamp_to_sqlite(ts: datetime) -> str:
    return ts.strftime("%Y-%m-%d %H:%M:%S")


def _iter_new_jsonl_rows(history_path: Path, last_line: int) -> Iterable[tuple[int, dict]]:
    """Yield new JSON objects after the parser checkpoint.

    If the parser encounters a truncated trailing line during rsync, it stops and leaves
    the checkpoint at the last known-good line so the next run can retry cleanly.
    """
    with history_path.open("r", encoding="utf-8") as handle:
        for line_no, raw_line in enumerate(handle, start=1):
            if line_no <= last_line:
                continue
            try:
                yield line_no, json.loads(raw_line)
            except json.JSONDecodeError:
                logger.warning("Skipping malformed JSONL line %s in %s", line_no, history_path)
                break


def _import_claude_history(db: Database, history_path: Path) -> int:
    source_key = str(history_path.resolve())
    last_line = db.get_parser_line_count(source_key)
    imported_rows = []
    max_imported_line = last_line

    for line_no, record in _iter_new_jsonl_rows(history_path, last_line):
        display = record.get("display", "")
        if is_machine_generated(display):
            max_imported_line = line_no
            continue

        timestamp = datetime.fromtimestamp(record["timestamp"] / 1000)
        project = record.get("project") or ""
        imported_rows.append((
            _timestamp_to_sqlite(timestamp),
            "claude",
            os.path.basename(project) or "unknown",
            record["sessionId"],
        ))
        max_imported_line = line_no

    db.insert_orchestration_activity(imported_rows)
    db.set_parser_line_count(source_key, max_imported_line)
    return len(imported_rows)


def _import_codex_history(db: Database, history_path: Path, state_db_path: Path) -> int:
    source_key = str(history_path.resolve())
    last_line = db.get_parser_line_count(source_key)
    imported_rows = []
    max_imported_line = last_line

    with CodexProjectResolver(state_db_path) as resolver:
        for line_no, record in _iter_new_jsonl_rows(history_path, last_line):
            text = record.get("text", "")
            if is_machine_generated(text):
                max_imported_line = line_no
                continue

            timestamp = datetime.fromtimestamp(record["ts"])
            imported_rows.append((
                _timestamp_to_sqlite(timestamp),
                "codex",
                resolver.project_for_session(record["session_id"]),
                record["session_id"],
            ))
            max_imported_line = line_no

    db.insert_orchestration_activity(imported_rows)
    db.set_parser_line_count(source_key, max_imported_line)
    return len(imported_rows)


def import_history(
    *,
    db_path: Path = DEFAULT_DB_PATH,
    claude_history_path: Path = DEFAULT_CLAUDE_HISTORY,
    codex_history_path: Path = DEFAULT_CODEX_HISTORY,
    codex_state_path: Path = DEFAULT_CODEX_STATE,
) -> dict[str, int]:
    """Import both history sources and return per-tool counts."""
    db = Database(db_path)
    counts = {"claude": 0, "codex": 0}

    if claude_history_path.exists():
        counts["claude"] = _import_claude_history(db, claude_history_path)
    else:
        logger.warning("Claude history file not found: %s", claude_history_path)

    if codex_history_path.exists() and codex_state_path.exists():
        counts["codex"] = _import_codex_history(db, codex_history_path, codex_state_path)
    else:
        if not codex_history_path.exists():
            logger.warning("Codex history file not found: %s", codex_history_path)
        if not codex_state_path.exists():
            logger.warning("Codex state DB not found: %s", codex_state_path)

    return counts


def main() -> None:
    parser = argparse.ArgumentParser(description="Import Claude/Codex session history into SQLite")
    parser.add_argument("--db-path", type=Path, default=DEFAULT_DB_PATH)
    parser.add_argument("--claude-history", type=Path, default=DEFAULT_CLAUDE_HISTORY)
    parser.add_argument("--codex-history", type=Path, default=DEFAULT_CODEX_HISTORY)
    parser.add_argument("--codex-state", type=Path, default=DEFAULT_CODEX_STATE)
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(levelname)s %(name)s: %(message)s",
    )

    counts = import_history(
        db_path=args.db_path,
        claude_history_path=args.claude_history,
        codex_history_path=args.codex_history,
        codex_state_path=args.codex_state,
    )
    logger.info("Imported orchestration activity: %s", counts)


if __name__ == "__main__":
    main()
