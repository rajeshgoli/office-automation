#!/usr/bin/env python3
"""Collect session telemetry from tool_usage.db and git into telemetry.db."""

from __future__ import annotations

import argparse
import json
import logging
import re
import sqlite3
import subprocess
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional
from zoneinfo import ZoneInfo

from src.config import load_config
from src.project_names import normalize_project_name
from src.telemetry_db import (
    DEFAULT_LEGACY_DB_PATH,
    DEFAULT_TELEMETRY_DB_PATH,
    migrate_legacy_session_output,
    upsert_collector_session_output_rows,
)

logger = logging.getLogger(__name__)

REPO_ROOT = Path(__file__).resolve().parent
DEFAULT_TOOL_DB_PATH = Path.home() / ".local" / "share" / "claude-sessions" / "tool_usage.db"
DATABASE_TIMEZONE = ZoneInfo("America/Los_Angeles")
SHORTSTAT_RE = re.compile(
    r"\s*(\d+) files? changed"
    r"(?:,\s*(\d+) insertions?\(\+\))?"
    r"(?:,\s*(\d+) deletions?\(-\))?"
)
HUMAN_SESSION_RE = re.compile(r"^claude-[0-9a-f]+$")


@dataclass
class GitCommand:
    timestamp: datetime
    repo: str
    bash_command: str


@dataclass
class SessionInfo:
    session_id: str
    session_name: str
    project_name: str
    start_time: datetime
    end_time: datetime
    tool_counts: dict[str, int] = field(default_factory=dict)
    git_commits: list[GitCommand] = field(default_factory=list)
    git_pushes: list[GitCommand] = field(default_factory=list)


@dataclass
class CommitStats:
    repo: str
    commit_hash: str
    author_date: datetime
    subject: str
    files_changed: int
    insertions: int
    deletions: int


def _connect(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    return conn


def _timestamp_to_sqlite(value: datetime) -> str:
    return value.strftime("%Y-%m-%d %H:%M:%S")


def _parse_datetime(value: str) -> datetime:
    normalized = value.strip()
    if normalized.endswith("Z"):
        normalized = normalized[:-1] + "+00:00"

    parsed: Optional[datetime] = None
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError:
        for fmt in (
            "%Y-%m-%d %H:%M:%S.%f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%dT%H:%M:%S.%f",
            "%Y-%m-%dT%H:%M:%S",
        ):
            try:
                parsed = datetime.strptime(normalized, fmt)
                break
            except ValueError:
                continue
    if parsed is None:
        raise ValueError(f"Unsupported timestamp: {value}")
    if parsed.tzinfo is not None:
        parsed = parsed.astimezone(DATABASE_TIMEZONE).replace(tzinfo=None)
    return parsed


def _normalize_repo_name(path_or_name: str) -> str:
    return normalize_project_name(path_or_name or "unknown")


def _is_human_session(session_name: str, session_id: str) -> int:
    if HUMAN_SESSION_RE.fullmatch(session_name or ""):
        return 1
    if not session_name or session_name == session_id:
        return 1
    return 0


def build_session_index(tool_db: Path, cutoff: datetime) -> dict[str, SessionInfo]:
    """Build per-session activity from tool_usage.db."""
    if not tool_db.exists():
        logger.warning("tool_usage DB not found at %s", tool_db)
        return {}

    sessions: dict[str, SessionInfo] = {}
    with _connect(tool_db) as conn:
        table = conn.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tool_usage'"
        ).fetchone()
        if table is None:
            logger.warning("tool_usage table missing in %s", tool_db)
            return sessions

        rows = conn.execute(
            """
            SELECT
                session_id,
                session_name,
                project_name,
                tool_name,
                target_file,
                bash_command,
                timestamp,
                cwd
            FROM tool_usage
            WHERE hook_type = 'PreToolUse'
              AND timestamp >= ?
            ORDER BY session_id, timestamp
            """,
            (_timestamp_to_sqlite(cutoff),),
        ).fetchall()

    for row in rows:
        timestamp = _parse_datetime(row["timestamp"])
        session_id = row["session_id"]
        session_name = row["session_name"] or session_id
        project_name = _normalize_repo_name(row["project_name"] or row["cwd"] or "unknown")
        info = sessions.get(session_id)
        if info is None:
            info = SessionInfo(
                session_id=session_id,
                session_name=session_name,
                project_name=project_name,
                start_time=timestamp,
                end_time=timestamp,
            )
            sessions[session_id] = info
        else:
            info.start_time = min(info.start_time, timestamp)
            info.end_time = max(info.end_time, timestamp)

        tool_name = row["tool_name"] or "unknown"
        info.tool_counts[tool_name] = info.tool_counts.get(tool_name, 0) + 1

        bash_command = (row["bash_command"] or "").strip()
        repo_name = _normalize_repo_name(row["cwd"] or project_name)
        if tool_name == "Bash" and bash_command.startswith("git commit"):
            info.git_commits.append(GitCommand(timestamp, repo_name, bash_command))
        elif tool_name == "Bash" and bash_command.startswith("git push"):
            info.git_pushes.append(GitCommand(timestamp, repo_name, bash_command))

    return sessions


def collect_git_stats(repos: list[Path], cutoff: datetime) -> dict[str, list[CommitStats]]:
    """Collect git shortstat output for watched repos."""
    commits_by_repo: dict[str, list[CommitStats]] = {}
    cutoff_arg = cutoff.isoformat(sep=" ", timespec="seconds")

    for repo in repos:
        expanded = repo.expanduser()
        if not expanded.exists():
            logger.warning("Skipping missing repo: %s", expanded)
            continue

        result = subprocess.run(
            [
                "git",
                "-C",
                str(expanded),
                "log",
                "--all",
                "--no-merges",
                "--format=COMMIT:%H|%aI|%s",
                "--shortstat",
                f"--after={cutoff_arg}",
            ],
            check=True,
            capture_output=True,
            text=True,
        )

        repo_name = _normalize_repo_name(expanded.name)
        commits: list[CommitStats] = []
        current: Optional[CommitStats] = None

        for raw_line in result.stdout.splitlines():
            line = raw_line.rstrip()
            if line.startswith("COMMIT:"):
                if current is not None:
                    commits.append(current)
                commit_hash, author_date, subject = line[len("COMMIT:"):].split("|", 2)
                current = CommitStats(
                    repo=repo_name,
                    commit_hash=commit_hash,
                    author_date=_parse_datetime(author_date),
                    subject=subject,
                    files_changed=0,
                    insertions=0,
                    deletions=0,
                )
                continue

            if current is None:
                continue

            match = SHORTSTAT_RE.match(line)
            if match:
                current.files_changed = int(match.group(1) or 0)
                current.insertions = int(match.group(2) or 0)
                current.deletions = int(match.group(3) or 0)

        if current is not None:
            commits.append(current)

        commits_by_repo[repo_name] = commits

    return commits_by_repo


def _match_commit(
    command: GitCommand,
    commits_by_repo: dict[str, list[CommitStats]],
    matched_hashes: set[str],
) -> Optional[CommitStats]:
    best_match: Optional[CommitStats] = None
    best_delta: Optional[float] = None

    for commit in commits_by_repo.get(command.repo, []):
        if commit.commit_hash in matched_hashes:
            continue
        delta = abs((commit.author_date - command.timestamp).total_seconds())
        if delta >= 60:
            continue
        if best_delta is None or delta < best_delta:
            best_match = commit
            best_delta = delta

    return best_match


def _session_row(session: SessionInfo, matched_commits: list[CommitStats]) -> tuple:
    duration_minutes = max(
        0,
        int((session.end_time - session.start_time).total_seconds() // 60),
    )
    lines_added = sum(commit.insertions for commit in matched_commits)
    lines_removed = sum(commit.deletions for commit in matched_commits)
    files_modified = sum(commit.files_changed for commit in matched_commits)
    git_pushes = sum(1 for push in session.git_pushes if "--delete" not in push.bash_command)

    return (
        session.session_id,
        session.project_name,
        _timestamp_to_sqlite(session.start_time),
        duration_minutes,
        lines_added,
        lines_removed,
        files_modified,
        len(session.git_commits),
        git_pushes,
        0,
        0,
        0,
        0,
        json.dumps(session.tool_counts, sort_keys=True),
        None,
        _is_human_session(session.session_name, session.session_id),
    )


def _synthetic_rows(
    commits_by_repo: dict[str, list[CommitStats]],
    matched_hashes: set[str],
) -> list[tuple]:
    grouped: dict[tuple[str, str], list[CommitStats]] = {}
    for repo, commits in commits_by_repo.items():
        for commit in commits:
            if commit.commit_hash in matched_hashes:
                continue
            date_key = commit.author_date.strftime("%Y-%m-%d")
            grouped.setdefault((repo, date_key), []).append(commit)

    rows = []
    for (repo, date_key), commits in sorted(grouped.items()):
        earliest = min(commit.author_date for commit in commits)
        rows.append((
            f"unattributed-{repo}-{date_key}",
            repo,
            _timestamp_to_sqlite(earliest),
            0,
            sum(commit.insertions for commit in commits),
            sum(commit.deletions for commit in commits),
            sum(commit.files_changed for commit in commits),
            len(commits),
            0,
            0,
            0,
            0,
            0,
            None,
            None,
            0,
        ))
    return rows


def _load_repo_paths() -> list[Path]:
    config = load_config(REPO_ROOT / "config.yaml")
    if config.telemetry is None or not config.telemetry.repos:
        raise ValueError("config.yaml is missing telemetry.repos")
    return [Path(path).expanduser() for path in config.telemetry.repos]


def collect_session_telemetry(
    *,
    tool_db_path: Path = DEFAULT_TOOL_DB_PATH,
    output_db_path: Path = DEFAULT_TELEMETRY_DB_PATH,
    repos: Optional[list[Path]] = None,
    days: int = 2,
    dry_run: bool = False,
    now: Optional[datetime] = None,
) -> dict[str, int]:
    """Collect telemetry rows and optionally write them to telemetry.db."""
    current_time = now or datetime.now()
    cutoff = current_time - timedelta(days=max(days, 1))
    repo_paths = repos or _load_repo_paths()

    migrate_legacy_session_output(DEFAULT_LEGACY_DB_PATH, output_db_path)
    sessions = build_session_index(tool_db_path, cutoff)
    commits_by_repo = collect_git_stats(repo_paths, cutoff)

    matched_hashes: set[str] = set()
    rows = []
    for session in sessions.values():
        matched_commits: list[CommitStats] = []
        for command in session.git_commits:
            match = _match_commit(command, commits_by_repo, matched_hashes)
            if match is None:
                continue
            matched_hashes.add(match.commit_hash)
            matched_commits.append(match)
        rows.append(_session_row(session, matched_commits))

    rows.extend(_synthetic_rows(commits_by_repo, matched_hashes))

    if not dry_run:
        upsert_collector_session_output_rows(rows, output_db_path)

    synthetic_count = sum(1 for row in rows if str(row[0]).startswith("unattributed-"))
    return {
        "sessions": len(sessions),
        "rows_written": len(rows),
        "synthetic_rows": synthetic_count,
        "matched_commits": len(matched_hashes),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Collect session telemetry into telemetry.db")
    parser.add_argument("--tool-db", type=Path, default=DEFAULT_TOOL_DB_PATH)
    parser.add_argument("--output", type=Path, default=DEFAULT_TELEMETRY_DB_PATH)
    parser.add_argument("--days", type=int, default=2)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--log-level", default="INFO")
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(levelname)s %(name)s: %(message)s",
    )

    stats = collect_session_telemetry(
        tool_db_path=args.tool_db,
        output_db_path=args.output,
        days=args.days,
        dry_run=args.dry_run,
    )
    logger.info("Session telemetry collection complete: %s", stats)


if __name__ == "__main__":
    main()
