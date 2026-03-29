#!/usr/bin/env python3
"""Import session-related metadata into the office climate database."""

from __future__ import annotations

import argparse
import json
import logging
import os
import sqlite3
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Iterable, Optional, Sequence
from zoneinfo import ZoneInfo

from src.database import DEFAULT_DB_PATH, Database
from src.project_names import normalize_project_name
from src.telemetry_db import (
    DEFAULT_TELEMETRY_DB_PATH,
    migrate_legacy_session_output,
    replace_session_output_rows,
)

logger = logging.getLogger(__name__)

REPO_ROOT = Path(__file__).resolve().parent
DEFAULT_CLAUDE_HISTORY = REPO_ROOT / "data" / "claude_history.jsonl"
DEFAULT_CODEX_HISTORY = REPO_ROOT / "data" / "codex_history.jsonl"
DEFAULT_CODEX_STATE = REPO_ROOT / "data" / "codex_state.sqlite"
DEFAULT_SESSION_META_DIR = REPO_ROOT / "data" / "session-meta"
DATABASE_TIMEZONE = ZoneInfo("America/Los_Angeles")
DEFAULT_GITHUB_OWNER = "rajeshgoli"
GH_REPO_LIMIT = 100
GH_PR_LIMIT = 500
GhRunner = Callable[..., subprocess.CompletedProcess[str]]


def is_machine_generated(text: str) -> bool:
    """Return True when a prompt/history entry was injected by session-manager."""
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
        return _normalize_project(row[0])


def _normalize_project(project: str) -> str:
    """Normalize project names across local path aliases."""
    return normalize_project_name(project)


def _timestamp_to_sqlite(ts: datetime) -> str:
    """Format timestamps the same way SQLite stores them in this database."""
    return ts.strftime("%Y-%m-%d %H:%M:%S")


def _utc_iso_to_local_sqlite(value: Optional[str]) -> Optional[str]:
    """Convert a UTC ISO timestamp to the database's Los Angeles local time."""
    if not value:
        return None

    timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return _timestamp_to_sqlite(timestamp.astimezone(DATABASE_TIMEZONE))


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
            _normalize_project(project),
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
    """Import Claude/Codex history and return per-tool counts."""
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


def _run_gh_json(args: Sequence[str], runner: GhRunner = subprocess.run) -> Any:
    """Execute a gh CLI command and parse its JSON response."""
    result = runner(
        ["gh", *args],
        check=True,
        capture_output=True,
        text=True,
    )

    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"gh returned invalid JSON for command: {' '.join(args)}") from exc


def _list_repo_names(owner: str, runner: GhRunner = subprocess.run) -> Optional[list[str]]:
    """Return GitHub repository names for the target owner, or None on global failure."""
    try:
        payload = _run_gh_json(
            ["repo", "list", owner, "--json", "name", "--limit", str(GH_REPO_LIMIT)],
            runner=runner,
        )
    except FileNotFoundError:
        logger.warning("GitHub CLI not available; skipping GitHub PR import")
        return None
    except subprocess.CalledProcessError as exc:
        message = (exc.stderr or exc.stdout or str(exc)).strip()
        logger.warning("GitHub CLI unavailable or unauthenticated; skipping GitHub PR import: %s", message)
        return None
    except RuntimeError as exc:
        logger.warning("Unable to parse GitHub repo list; skipping GitHub PR import: %s", exc)
        return None

    return [repo["name"] for repo in payload if repo.get("name")]


def _list_repo_prs(
    owner: str,
    repo: str,
    runner: GhRunner = subprocess.run,
) -> Optional[list[dict[str, Any]]]:
    """Return PRs for a single repo, or None when that repo cannot be queried."""
    try:
        payload = _run_gh_json(
            [
                "pr",
                "list",
                "--repo",
                f"{owner}/{repo}",
                "--state",
                "all",
                "--json",
                "number,title,state,additions,deletions,changedFiles,createdAt,mergedAt",
                "--limit",
                str(GH_PR_LIMIT),
            ],
            runner=runner,
        )
    except subprocess.CalledProcessError as exc:
        message = (exc.stderr or exc.stdout or str(exc)).strip()
        logger.warning("Skipping GitHub PR import for %s: %s", repo, message)
        return None
    except RuntimeError as exc:
        logger.warning("Skipping GitHub PR import for %s due to invalid JSON: %s", repo, exc)
        return None

    if len(payload) == GH_PR_LIMIT:
        logger.warning(
            "GitHub PR list for %s hit the %s PR limit; pagination may be needed",
            repo,
            GH_PR_LIMIT,
        )

    return payload


def collect_github_prs(
    *,
    db_path: Path = DEFAULT_DB_PATH,
    owner: str = DEFAULT_GITHUB_OWNER,
    runner: GhRunner = subprocess.run,
) -> int:
    """Collect GitHub PR metadata for all repos under an owner and upsert it into SQLite."""
    repo_names = _list_repo_names(owner, runner=runner)
    if repo_names is None:
        return 0

    db = Database(db_path)
    total_imported = 0

    for repo in repo_names:
        prs = _list_repo_prs(owner, repo, runner=runner)
        if prs is None:
            continue

        rows = []
        for pr in prs:
            rows.append((
                repo,
                int(pr["number"]),
                pr.get("title"),
                pr["state"],
                int(pr.get("additions") or 0),
                int(pr.get("deletions") or 0),
                int(pr.get("changedFiles") or 0),
                _utc_iso_to_local_sqlite(pr["createdAt"]),
                _utc_iso_to_local_sqlite(pr.get("mergedAt")),
            ))

        db.upsert_github_prs(rows)
        total_imported += len(rows)

    return total_imported


def import_session_meta(
    *,
    db_path: Path = DEFAULT_DB_PATH,
    telemetry_db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
    session_meta_dir: Path = DEFAULT_SESSION_META_DIR,
) -> int:
    """Import all session-meta JSON files into telemetry.db using replace semantics."""
    migrate_legacy_session_output(db_path, telemetry_db_path)
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
        project = _normalize_project(project_path)
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

    return replace_session_output_rows(rows, telemetry_db_path)


def main() -> None:
    parser = argparse.ArgumentParser(description="Import session-related data into SQLite")
    parser.add_argument(
        "--mode",
        choices=("history", "github-prs", "session-meta"),
        default="history",
    )
    parser.add_argument("--db-path", type=Path, default=DEFAULT_DB_PATH)
    parser.add_argument("--claude-history", type=Path, default=DEFAULT_CLAUDE_HISTORY)
    parser.add_argument("--codex-history", type=Path, default=DEFAULT_CODEX_HISTORY)
    parser.add_argument("--codex-state", type=Path, default=DEFAULT_CODEX_STATE)
    parser.add_argument("--owner", default=DEFAULT_GITHUB_OWNER)
    parser.add_argument("--session-meta-dir", type=Path, default=DEFAULT_SESSION_META_DIR)
    parser.add_argument("--telemetry-db-path", type=Path, default=DEFAULT_TELEMETRY_DB_PATH)
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(levelname)s %(name)s: %(message)s",
    )

    if args.mode == "history":
        counts = import_history(
            db_path=args.db_path,
            claude_history_path=args.claude_history,
            codex_history_path=args.codex_history,
            codex_state_path=args.codex_state,
        )
        logger.info("Imported orchestration activity: %s", counts)
    elif args.mode == "github-prs":
        count = collect_github_prs(db_path=args.db_path, owner=args.owner)
        logger.info("Imported %s GitHub PRs", count)
    else:
        imported = import_session_meta(
            db_path=args.db_path,
            telemetry_db_path=args.telemetry_db_path,
            session_meta_dir=args.session_meta_dir,
        )
        logger.info("Imported session-meta rows: %s", imported)


if __name__ == "__main__":
    main()
