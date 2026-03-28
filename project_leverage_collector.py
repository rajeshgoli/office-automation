"""Collect cross-project leverage metrics into the office climate database."""

from __future__ import annotations

import argparse
import logging
import re
import sqlite3
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional

from src.database import Database, DEFAULT_DB_PATH
from src.project_names import normalize_project_name

logger = logging.getLogger(__name__)

DEFAULT_TOOL_USAGE_DB_PATH = Path(__file__).parent / "data" / "tool_usage.db"
DEFAULT_ENGRAM_DB_PATH = Path(__file__).parent / "data" / "engram_state.db"
DEFAULT_ENGRAM_REGISTRY_PATH = Path(__file__).parent / "data" / "engram_concept_registry.md"
PERSONA_PROJECT_METRIC_PREFIX = "persona_project::"
CONCEPT_HEADER_RE = re.compile(r"^##\s+C\d{3}:.*\((ACTIVE|DEAD)\b", re.IGNORECASE)


def _connect(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    return conn


def _table_exists(conn: sqlite3.Connection, table_name: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
        (table_name,),
    ).fetchone()
    return row is not None


def _normalize_project_name(project_name: Optional[str]) -> str:
    return normalize_project_name(project_name)


def _parse_datetime(value: str) -> datetime:
    normalized = value.strip()
    if normalized.endswith("Z"):
        normalized = normalized[:-1] + "+00:00"

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
        else:
            raise

    if parsed.tzinfo is not None:
        parsed = parsed.astimezone().replace(tzinfo=None)
    return parsed


def _collect_tool_usage_metrics(tool_usage_db_path: Path) -> list[tuple[str, str, str, float]]:
    if not tool_usage_db_path.exists():
        logger.info("Skipping tool usage collection; DB not found at %s", tool_usage_db_path)
        return []

    rows: list[tuple[str, str, str, float]] = []
    with _connect(tool_usage_db_path) as conn:
        if not _table_exists(conn, "tool_usage"):
            logger.warning("Skipping tool usage collection; tool_usage table missing in %s", tool_usage_db_path)
            return rows

        sm_rows = conn.execute(
            """
            SELECT
                date(timestamp) AS date,
                SUM(CASE WHEN bash_command LIKE 'sm send%' THEN 1 ELSE 0 END) AS sm_sends,
                SUM(CASE WHEN bash_command LIKE 'sm dispatch%' THEN 1 ELSE 0 END) AS sm_dispatches,
                SUM(CASE WHEN bash_command LIKE 'sm remind%' THEN 1 ELSE 0 END) AS sm_reminds
            FROM tool_usage
            WHERE tool_name = 'Bash'
              AND hook_type = 'PreToolUse'
              AND bash_command LIKE 'sm %'
            GROUP BY date(timestamp)
            ORDER BY date(timestamp)
            """
        ).fetchall()
        for row in sm_rows:
            rows.extend([
                (row["date"], "session-manager", "sm_sends", float(row["sm_sends"] or 0)),
                (row["date"], "session-manager", "sm_dispatches", float(row["sm_dispatches"] or 0)),
                (row["date"], "session-manager", "sm_reminds", float(row["sm_reminds"] or 0)),
            ])

        active_session_rows = conn.execute(
            """
            SELECT
                date(timestamp) AS date,
                COUNT(DISTINCT session_id) AS active_sessions
            FROM tool_usage
            WHERE hook_type = 'PreToolUse'
            GROUP BY date(timestamp)
            ORDER BY date(timestamp)
            """
        ).fetchall()
        for row in active_session_rows:
            rows.append((
                row["date"],
                "session-manager",
                "sm_active_sessions",
                float(row["active_sessions"] or 0),
            ))

        persona_read_rows = conn.execute(
            """
            SELECT
                date(timestamp) AS date,
                COUNT(*) AS persona_reads
            FROM tool_usage
            WHERE tool_name = 'Read'
              AND target_file LIKE '%agent-os/personas/%'
            GROUP BY date(timestamp)
            ORDER BY date(timestamp)
            """
        ).fetchall()
        for row in persona_read_rows:
            rows.append((
                row["date"],
                "agent-os",
                "persona_reads",
                float(row["persona_reads"] or 0),
            ))

        persona_project_rows = conn.execute(
            """
            SELECT
                date(timestamp) AS date,
                COALESCE(NULLIF(project_name, ''), 'unknown') AS persona_project
            FROM tool_usage
            WHERE tool_name = 'Read'
              AND target_file LIKE '%agent-os/personas/%'
            ORDER BY date(timestamp), persona_project
            """
        ).fetchall()

        projects_by_date: dict[str, set[str]] = {}
        for row in persona_project_rows:
            date = row["date"]
            projects_by_date.setdefault(date, set()).add(_normalize_project_name(row["persona_project"]))

        for date, projects in projects_by_date.items():
            rows.append((
                date,
                "agent-os",
                "persona_projects",
                float(len(projects)),
            ))

        persona_project_detail_rows = conn.execute(
            """
            SELECT
                date(timestamp) AS date,
                COALESCE(NULLIF(project_name, ''), 'unknown') AS persona_project
            FROM tool_usage
            WHERE tool_name = 'Read'
              AND target_file LIKE '%agent-os/personas/%'
            GROUP BY date(timestamp), persona_project
            ORDER BY date(timestamp), persona_project
            """
        ).fetchall()
        for row in persona_project_detail_rows:
            metric = PERSONA_PROJECT_METRIC_PREFIX + _normalize_project_name(row["persona_project"])
            rows.append((row["date"], "agent-os", metric, 1.0))

        if _table_exists(conn, "telegram_telemetry"):
            telegram_rows = conn.execute(
                """
                SELECT
                    date(timestamp) AS date,
                    SUM(CASE WHEN direction = 'in' THEN 1 ELSE 0 END) AS telegram_in,
                    SUM(CASE WHEN direction = 'out' THEN 1 ELSE 0 END) AS telegram_out
                FROM telegram_telemetry
                GROUP BY date(timestamp)
                ORDER BY date(timestamp)
                """
            ).fetchall()
            for row in telegram_rows:
                rows.extend([
                    (row["date"], "session-manager", "sm_telegram_in", float(row["telegram_in"] or 0)),
                    (row["date"], "session-manager", "sm_telegram_out", float(row["telegram_out"] or 0)),
                ])
        else:
            logger.info("telegram_telemetry table not present; leaving Telegram metrics empty")

    return rows


def _count_active_concepts(concept_registry_path: Path) -> int:
    if not concept_registry_path.exists():
        logger.info("Skipping concept registry parse; file not found at %s", concept_registry_path)
        return 0

    active_count = 0
    for line in concept_registry_path.read_text(encoding="utf-8").splitlines():
        match = CONCEPT_HEADER_RE.match(line.strip())
        if not match:
            continue
        if match.group(1).upper() == "ACTIVE":
            active_count += 1
    return active_count


def _collect_engram_metrics(
    engram_db_path: Path,
    concept_registry_path: Path,
    now: datetime,
) -> list[tuple[str, str, str, float]]:
    if not engram_db_path.exists():
        logger.info("Skipping engram collection; DB not found at %s", engram_db_path)
        return []

    rows: list[tuple[str, str, str, float]] = []
    last_committed_fold: Optional[datetime] = None
    folds_7d = 0

    with _connect(engram_db_path) as conn:
        if not _table_exists(conn, "dispatches"):
            logger.warning("Skipping engram collection; dispatches table missing in %s", engram_db_path)
            return rows

        dispatch_rows = conn.execute(
            """
            SELECT created_at
            FROM dispatches
            WHERE state = 'committed'
            ORDER BY created_at DESC
            """
        ).fetchall()

    if dispatch_rows:
        parsed_times = [_parse_datetime(row["created_at"]) for row in dispatch_rows]
        last_committed_fold = max(parsed_times)
        threshold = now - timedelta(days=7)
        folds_7d = sum(1 for fold_time in parsed_times if fold_time >= threshold)

    date_str = now.strftime("%Y-%m-%d")
    active_concepts = _count_active_concepts(concept_registry_path)
    rows.append((date_str, "engram", "engram_folds_7d", float(folds_7d)))
    rows.append((date_str, "engram", "engram_active_concepts", float(active_concepts)))
    if last_committed_fold is not None:
        age_hours = (now - last_committed_fold).total_seconds() / 3600.0
        rows.append((date_str, "engram", "engram_last_fold_age_hours", age_hours))

    return rows


def _collect_office_automation_metrics(db_path: Path) -> list[tuple[str, str, str, float]]:
    rows: list[tuple[str, str, str, float]] = []
    with _connect(db_path) as conn:
        climate_rows = conn.execute(
            """
            SELECT date(timestamp) AS date, COUNT(*) AS actions
            FROM climate_actions
            GROUP BY date(timestamp)
            ORDER BY date(timestamp)
            """
        ).fetchall()
        for row in climate_rows:
            rows.append((row["date"], "office-automate", "automation_events", float(row["actions"] or 0)))

        occupancy_rows = conn.execute(
            """
            SELECT date(timestamp) AS date, COUNT(*) AS transitions
            FROM occupancy_log
            GROUP BY date(timestamp)
            ORDER BY date(timestamp)
            """
        ).fetchall()
        for row in occupancy_rows:
            rows.append((row["date"], "office-automate", "state_transitions", float(row["transitions"] or 0)))

    return rows


def collect_project_leverage(
    db_path: Path = DEFAULT_DB_PATH,
    tool_usage_db_path: Path = DEFAULT_TOOL_USAGE_DB_PATH,
    engram_db_path: Path = DEFAULT_ENGRAM_DB_PATH,
    concept_registry_path: Path = DEFAULT_ENGRAM_REGISTRY_PATH,
    now: Optional[datetime] = None,
) -> list[tuple[str, str, str, float]]:
    """Collect all project leverage metrics and upsert them into the office DB."""
    now = now or datetime.now()
    db = Database(db_path)
    rows: list[tuple[str, str, str, float]] = []
    rows.extend(_collect_tool_usage_metrics(tool_usage_db_path))
    rows.extend(_collect_engram_metrics(engram_db_path, concept_registry_path, now))
    rows.extend(_collect_office_automation_metrics(db_path))
    db.upsert_project_leverage(rows)
    logger.info("Upserted %s project leverage rows", len(rows))
    return rows


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db-path", type=Path, default=DEFAULT_DB_PATH)
    parser.add_argument("--tool-usage-db", type=Path, default=DEFAULT_TOOL_USAGE_DB_PATH)
    parser.add_argument("--engram-db", type=Path, default=DEFAULT_ENGRAM_DB_PATH)
    parser.add_argument("--engram-registry", type=Path, default=DEFAULT_ENGRAM_REGISTRY_PATH)
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%H:%M:%S",
    )
    collect_project_leverage(
        db_path=args.db_path,
        tool_usage_db_path=args.tool_usage_db,
        engram_db_path=args.engram_db,
        concept_registry_path=args.engram_registry,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
