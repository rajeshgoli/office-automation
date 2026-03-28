#!/usr/bin/env python3
"""Import Claude session-meta JSON into the office climate database."""

from __future__ import annotations

import argparse
import json
import logging
import os
from datetime import datetime
from pathlib import Path

from src.database import Database, DEFAULT_DB_PATH

logger = logging.getLogger(__name__)

REPO_ROOT = Path(__file__).resolve().parent
DEFAULT_SESSION_META_DIR = REPO_ROOT / "data" / "session-meta"
_PROJECT_ALIASES = {
    "office-automation": "office-automate",
}


def is_machine_generated(text: str) -> bool:
    """Return True when a prompt was injected by session-manager."""
    return text.startswith("[Input from:") or text.startswith("[sm")


def _normalize_project(project: str) -> str:
    """Normalize project names across local path aliases."""
    if not project:
        return "unknown"

    normalized = project.strip().rstrip("/")
    if not normalized:
        return "unknown"

    basename = os.path.basename(normalized) or normalized
    return _PROJECT_ALIASES.get(basename, basename)


def _timestamp_to_sqlite(ts: datetime) -> str:
    """Format timestamps the same way SQLite stores them in this database."""
    return ts.strftime("%Y-%m-%d %H:%M:%S")


def _utc_iso_to_local_sqlite(value: str) -> str:
    """Convert a UTC ISO 8601 timestamp to local Pacific database format."""
    timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return _timestamp_to_sqlite(timestamp.astimezone())


def import_session_meta(
    *,
    db_path: Path = DEFAULT_DB_PATH,
    session_meta_dir: Path = DEFAULT_SESSION_META_DIR,
) -> int:
    """Import all session-meta JSON files into SQLite using replace semantics."""
    db = Database(db_path)
    rows = []

    for path in sorted(session_meta_dir.glob("*.json")):
        try:
            with path.open("r", encoding="utf-8") as handle:
                record = json.load(handle)
        except json.JSONDecodeError:
            logger.warning("Skipping malformed JSON in %s", path)
            continue

        if (
            int(record.get("duration_minutes", 0) or 0) == 0
            and int(record.get("user_message_count", 0) or 0) == 0
        ):
            continue

        session_id = record.get("session_id")
        start_time = record.get("start_time")
        if not session_id or not start_time:
            logger.warning("Skipping session-meta file missing session_id/start_time: %s", path)
            continue

        project_path = record.get("project_path") or ""
        project = _normalize_project(os.path.basename(project_path))
        first_prompt = record.get("first_prompt") or ""
        rows.append((
            session_id,
            project,
            _utc_iso_to_local_sqlite(start_time),
            int(record.get("duration_minutes", 0) or 0),
            int(record.get("lines_added", 0) or 0),
            int(record.get("lines_removed", 0) or 0),
            int(record.get("files_modified", 0) or 0),
            int(record.get("git_commits", 0) or 0),
            int(record.get("git_pushes", 0) or 0),
            int(record.get("user_message_count", 0) or 0),
            int(record.get("assistant_message_count", 0) or 0),
            int(record.get("input_tokens", 0) or 0),
            int(record.get("output_tokens", 0) or 0),
            json.dumps(record.get("tool_counts")) if record.get("tool_counts") is not None else None,
            json.dumps(record.get("languages")) if record.get("languages") is not None else None,
            0 if is_machine_generated(first_prompt) else 1,
        ))

    db.replace_session_output(rows)
    return len(rows)


def main() -> None:
    parser = argparse.ArgumentParser(description="Import Claude session-meta JSON into SQLite")
    parser.add_argument("--db-path", type=Path, default=DEFAULT_DB_PATH)
    parser.add_argument("--session-meta-dir", type=Path, default=DEFAULT_SESSION_META_DIR)
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(levelname)s %(name)s: %(message)s",
    )

    imported = import_session_meta(
        db_path=args.db_path,
        session_meta_dir=args.session_meta_dir,
    )
    logger.info("Imported session-meta rows: %s", imported)


if __name__ == "__main__":
    main()
