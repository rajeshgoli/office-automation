"""
SQLite database for office climate automation.

Stores:
- Sensor readings (CO2, temp, humidity) for analysis
- State changes (occupancy, ERV, HVAC)
- Device events (door, window, motion)

Enables:
- Persistence across restarts
- Historical analysis and pattern detection
- Future automations based on learned patterns
"""

import sqlite3
import json
import logging
import math
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional, Dict, Any, List
from contextlib import contextmanager

logger = logging.getLogger(__name__)

DEFAULT_DB_PATH = Path(__file__).parent.parent / "data" / "office_climate.db"


class Database:
    """SQLite database for office climate data."""

    def __init__(self, db_path: Optional[Path] = None):
        self.db_path = db_path or DEFAULT_DB_PATH
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._init_schema()

    @staticmethod
    def _now() -> datetime:
        """Return the current local time."""
        return datetime.now()

    @contextmanager
    def _connection(self):
        """Context manager for database connections."""
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row
        try:
            yield conn
            conn.commit()
        finally:
            conn.close()

    def _init_schema(self):
        """Initialize database schema."""
        with self._connection() as conn:
            conn.executescript("""
                -- Sensor readings (Qingping air monitor)
                CREATE TABLE IF NOT EXISTS sensor_readings (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                    co2_ppm INTEGER,
                    temp_c REAL,
                    humidity REAL,
                    pm25 INTEGER,
                    pm10 INTEGER,
                    tvoc INTEGER,
                    source TEXT DEFAULT 'qingping'
                );
                CREATE INDEX IF NOT EXISTS idx_sensor_timestamp ON sensor_readings(timestamp);

                -- Occupancy state changes
                CREATE TABLE IF NOT EXISTS occupancy_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                    state TEXT NOT NULL,  -- 'present' or 'away'
                    trigger TEXT,         -- what caused the change (door, motion, mac, etc)
                    co2_ppm INTEGER,      -- CO2 at time of change
                    details TEXT          -- JSON with additional context
                );
                CREATE INDEX IF NOT EXISTS idx_occupancy_timestamp ON occupancy_log(timestamp);

                -- Device events (door, window, motion sensors)
                CREATE TABLE IF NOT EXISTS device_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                    device_type TEXT NOT NULL,  -- 'door', 'window', 'motion'
                    device_name TEXT,
                    event TEXT NOT NULL,        -- 'open', 'closed', 'detected', etc
                    details TEXT                -- JSON with raw event data
                );
                CREATE INDEX IF NOT EXISTS idx_device_timestamp ON device_events(timestamp);

                -- Climate control actions (ERV, HVAC)
                CREATE TABLE IF NOT EXISTS climate_actions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                    system TEXT NOT NULL,       -- 'erv', 'hvac'
                    action TEXT NOT NULL,       -- 'on', 'off', 'heat', 'cool', etc
                    setpoint REAL,              -- temperature setpoint if applicable
                    co2_ppm INTEGER,            -- CO2 at time of action
                    reason TEXT                 -- why this action was taken
                );
                CREATE INDEX IF NOT EXISTS idx_climate_timestamp ON climate_actions(timestamp);

                -- Orchestration activity imported from Claude/Codex history
                CREATE TABLE IF NOT EXISTS orchestration_activity (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp DATETIME NOT NULL,
                    tool TEXT NOT NULL CHECK(tool IN ('claude', 'codex')),
                    project TEXT NOT NULL DEFAULT 'unknown',
                    session_id TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_orch_timestamp ON orchestration_activity(timestamp);
                CREATE INDEX IF NOT EXISTS idx_orch_date ON orchestration_activity(date(timestamp));

                -- Incremental parser bookkeeping
                CREATE TABLE IF NOT EXISTS session_parser_state (
                    source TEXT PRIMARY KEY,
                    last_line INTEGER NOT NULL DEFAULT 0
                );

                -- Cross-project leverage metrics (EAV)
                CREATE TABLE IF NOT EXISTS project_leverage (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    date TEXT NOT NULL,
                    project TEXT NOT NULL,
                    metric TEXT NOT NULL,
                    value REAL NOT NULL DEFAULT 0,
                    UNIQUE(date, project, metric)
                );
                CREATE INDEX IF NOT EXISTS idx_proj_lev_date ON project_leverage(date);

                -- GitHub PR activity imported via gh CLI
                CREATE TABLE IF NOT EXISTS github_prs (
                    repo TEXT NOT NULL,
                    pr_number INTEGER NOT NULL,
                    title TEXT,
                    state TEXT NOT NULL,
                    additions INTEGER NOT NULL DEFAULT 0,
                    deletions INTEGER NOT NULL DEFAULT 0,
                    changed_files INTEGER NOT NULL DEFAULT 0,
                    created_at DATETIME NOT NULL,
                    merged_at DATETIME,
                    PRIMARY KEY (repo, pr_number)
                );
                CREATE INDEX IF NOT EXISTS idx_prs_created ON github_prs(created_at);
                CREATE INDEX IF NOT EXISTS idx_prs_merged ON github_prs(merged_at);

                -- Claude session output imported from session-meta JSON
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
            """)
            logger.info(f"Database initialized at {self.db_path}")

    @staticmethod
    def _now() -> datetime:
        """Return the current local time."""
        return datetime.now()

    @staticmethod
    def _format_timestamp(ts: datetime) -> str:
        """Format timestamps the same way SQLite stores them in this database."""
        return ts.strftime("%Y-%m-%d %H:%M:%S")

    @staticmethod
    def _parse_timestamp(value: str) -> datetime:
        """Parse timestamps read back from SQLite."""
        return datetime.strptime(value, "%Y-%m-%d %H:%M:%S")

    @staticmethod
    def _accumulate_duration_by_date(
        durations: Dict[str, float],
        start: datetime,
        end: datetime,
        seconds_per_unit: float,
    ) -> None:
        """Split a duration across calendar days and accumulate it in-place."""
        if end <= start:
            return

        cursor = start
        while cursor.date() < end.date():
            next_midnight = datetime.combine(
                cursor.date() + timedelta(days=1),
                datetime.min.time(),
            )
            date_str = cursor.strftime("%Y-%m-%d")
            durations[date_str] = durations.get(date_str, 0) + (
                (next_midnight - cursor).total_seconds() / seconds_per_unit
            )
            cursor = next_midnight

        date_str = cursor.strftime("%Y-%m-%d")
        durations[date_str] = durations.get(date_str, 0) + (
            (end - cursor).total_seconds() / seconds_per_unit
        )

    @staticmethod
    def _split_interval_by_date(start: datetime, end: datetime) -> List[tuple[str, str, str]]:
        """Split an interval into per-day HH:MM segments."""
        if end <= start:
            return []

        segments: List[tuple[str, str, str]] = []
        cursor = start
        while cursor.date() < end.date():
            day_end = datetime.combine(cursor.date(), datetime.max.time()).replace(
                hour=23,
                minute=59,
                second=59,
                microsecond=0,
            )
            segments.append((
                cursor.strftime("%Y-%m-%d"),
                cursor.strftime("%H:%M"),
                day_end.strftime("%H:%M"),
            ))
            cursor = datetime.combine(cursor.date() + timedelta(days=1), datetime.min.time())

        segments.append((
            cursor.strftime("%Y-%m-%d"),
            cursor.strftime("%H:%M"),
            end.strftime("%H:%M"),
        ))
        return segments

    @staticmethod
    def _build_day_range(now: datetime, days: int) -> List[str]:
        """Return YYYY-MM-DD strings for the inclusive trailing day range."""
        start_date = now.date() - timedelta(days=max(days - 1, 0))
        return [
            (start_date + timedelta(days=offset)).strftime("%Y-%m-%d")
            for offset in range(days)
        ]

    # --- Sensor readings ---

    def log_sensor_reading(
        self,
        co2_ppm: Optional[int] = None,
        temp_c: Optional[float] = None,
        humidity: Optional[float] = None,
        pm25: Optional[int] = None,
        pm10: Optional[int] = None,
        tvoc: Optional[int] = None,
        noise_db: Optional[int] = None,
        source: str = "qingping"
    ):
        """Log a sensor reading."""
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        with self._connection() as conn:
            conn.execute("""
                INSERT INTO sensor_readings (timestamp, co2_ppm, temp_c, humidity, pm25, pm10, tvoc, noise_db, source)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """, (now, co2_ppm, temp_c, humidity, pm25, pm10, tvoc, noise_db, source))

    def get_latest_sensor_reading(self) -> Optional[Dict[str, Any]]:
        """Get the most recent sensor reading."""
        with self._connection() as conn:
            row = conn.execute("""
                SELECT * FROM sensor_readings
                ORDER BY timestamp DESC LIMIT 1
            """).fetchone()
            if row:
                return dict(row)
        return None

    def get_sensor_readings(
        self,
        hours: int = 24,
        limit: int = 1000
    ) -> List[Dict[str, Any]]:
        """Get sensor readings for the past N hours."""
        since = datetime.now() - timedelta(hours=hours)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT * FROM sensor_readings
                WHERE timestamp > ?
                ORDER BY timestamp DESC
                LIMIT ?
            """, (since.isoformat(), limit)).fetchall()
            return [dict(row) for row in rows]

    # --- Occupancy log ---

    def log_occupancy_change(
        self,
        state: str,
        trigger: Optional[str] = None,
        co2_ppm: Optional[int] = None,
        details: Optional[Dict] = None
    ):
        """Log an occupancy state change."""
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        with self._connection() as conn:
            conn.execute("""
                INSERT INTO occupancy_log (timestamp, state, trigger, co2_ppm, details)
                VALUES (?, ?, ?, ?, ?)
            """, (now, state, trigger, co2_ppm, json.dumps(details) if details else None))

    def get_occupancy_history(
        self,
        hours: int = 24,
        limit: int = 100
    ) -> List[Dict[str, Any]]:
        """Get occupancy changes for the past N hours."""
        since = datetime.now() - timedelta(hours=hours)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT * FROM occupancy_log
                WHERE timestamp > ?
                ORDER BY timestamp DESC
                LIMIT ?
            """, (since.isoformat(), limit)).fetchall()
            return [dict(row) for row in rows]

    # --- Device events ---

    def log_device_event(
        self,
        device_type: str,
        event: str,
        device_name: Optional[str] = None,
        details: Optional[Dict] = None
    ):
        """Log a device event (door, window, motion)."""
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        with self._connection() as conn:
            conn.execute("""
                INSERT INTO device_events (timestamp, device_type, device_name, event, details)
                VALUES (?, ?, ?, ?, ?)
            """, (now, device_type, device_name, event, json.dumps(details) if details else None))

    def get_device_events(
        self,
        device_type: Optional[str] = None,
        hours: int = 24,
        limit: int = 100
    ) -> List[Dict[str, Any]]:
        """Get device events for the past N hours."""
        since = datetime.now() - timedelta(hours=hours)
        with self._connection() as conn:
            if device_type:
                rows = conn.execute("""
                    SELECT * FROM device_events
                    WHERE timestamp > ? AND device_type = ?
                    ORDER BY timestamp DESC
                    LIMIT ?
                """, (since.isoformat(), device_type, limit)).fetchall()
            else:
                rows = conn.execute("""
                    SELECT * FROM device_events
                    WHERE timestamp > ?
                    ORDER BY timestamp DESC
                    LIMIT ?
                """, (since.isoformat(), limit)).fetchall()
            return [dict(row) for row in rows]

    def get_latest_device_state(self, device_type: str) -> Optional[str]:
        """Get the most recent event state for a device type (door, window, motion).

        Returns:
            Event state string (e.g., "open", "closed", "detected", "clear") or None if no events found.
        """
        with self._connection() as conn:
            row = conn.execute("""
                SELECT event FROM device_events
                WHERE device_type = ?
                ORDER BY timestamp DESC
                LIMIT 1
            """, (device_type,)).fetchone()
            return row["event"] if row else None

    # --- Climate actions ---

    def log_climate_action(
        self,
        system: str,
        action: str,
        setpoint: Optional[float] = None,
        co2_ppm: Optional[int] = None,
        reason: Optional[str] = None
    ):
        """Log a climate control action (ERV on/off, HVAC change)."""
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        with self._connection() as conn:
            conn.execute("""
                INSERT INTO climate_actions (timestamp, system, action, setpoint, co2_ppm, reason)
                VALUES (?, ?, ?, ?, ?, ?)
            """, (now, system, action, setpoint, co2_ppm, reason))

    def get_climate_actions(
        self,
        system: Optional[str] = None,
        hours: int = 24,
        limit: int = 100
    ) -> List[Dict[str, Any]]:
        """Get climate actions for the past N hours."""
        since = datetime.now() - timedelta(hours=hours)
        with self._connection() as conn:
            if system:
                rows = conn.execute("""
                    SELECT * FROM climate_actions
                    WHERE timestamp > ? AND system = ?
                    ORDER BY timestamp DESC
                    LIMIT ?
                """, (since.isoformat(), system, limit)).fetchall()
            else:
                rows = conn.execute("""
                    SELECT * FROM climate_actions
                    WHERE timestamp > ?
                    ORDER BY timestamp DESC
                LIMIT ?
            """, (since.isoformat(), limit)).fetchall()
            return [dict(row) for row in rows]

    def upsert_github_prs(
        self,
        rows: List[tuple[str, int, Optional[str], str, int, int, int, str, Optional[str]]]
    ) -> None:
        """Insert or update GitHub PR rows keyed by repo and PR number."""
        if not rows:
            return

        with self._connection() as conn:
            conn.executemany("""
                INSERT INTO github_prs (
                    repo,
                    pr_number,
                    title,
                    state,
                    additions,
                    deletions,
                    changed_files,
                    created_at,
                    merged_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(repo, pr_number) DO UPDATE SET
                    title = excluded.title,
                    state = excluded.state,
                    additions = excluded.additions,
                    deletions = excluded.deletions,
                    changed_files = excluded.changed_files,
                    created_at = excluded.created_at,
                    merged_at = excluded.merged_at
            """, rows)

    def replace_session_output(
        self,
        rows: List[tuple[
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
        ]],
    ) -> None:
        """Replace session-meta rows keyed by session ID."""
        if not rows:
            return

        with self._connection() as conn:
            conn.executemany("""
                INSERT OR REPLACE INTO session_output (
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
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """, rows)

    def upsert_project_leverage(
        self,
        rows: List[tuple[str, str, str, float]],
    ) -> None:
        """Insert or update project leverage rows keyed by date, project, and metric."""
        if not rows:
            return

        with self._connection() as conn:
            conn.executemany("""
                INSERT INTO project_leverage (date, project, metric, value)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(date, project, metric) DO UPDATE SET
                    value = excluded.value
            """, rows)

    def get_project_leverage_rows(self, days: int = 7) -> List[Dict[str, Any]]:
        """Return raw project leverage rows for the trailing day window."""
        days = min(max(1, days), 30)
        since = (self._now() - timedelta(days=days - 1)).strftime("%Y-%m-%d")
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT date, project, metric, value
                FROM project_leverage
                WHERE date >= ?
                ORDER BY date ASC, project ASC, metric ASC
            """, (since,)).fetchall()
            return [dict(row) for row in rows]

    # --- Session history / orchestration imports ---

    def get_parser_line_count(self, source: str) -> int:
        """Return the last imported line number for a parser source."""
        with self._connection() as conn:
            row = conn.execute("""
                SELECT last_line
                FROM session_parser_state
                WHERE source = ?
            """, (source,)).fetchone()
            return int(row["last_line"]) if row else 0

    def set_parser_line_count(self, source: str, last_line: int) -> None:
        """Persist the parser checkpoint for a source file."""
        with self._connection() as conn:
            conn.execute("""
                INSERT INTO session_parser_state (source, last_line)
                VALUES (?, ?)
                ON CONFLICT(source) DO UPDATE SET last_line = excluded.last_line
            """, (source, last_line))

    def insert_orchestration_activity(self, rows: List[tuple[str, str, str, str]]) -> None:
        """Insert imported orchestration activity rows."""
        if not rows:
            return

        with self._connection() as conn:
            conn.executemany("""
                INSERT INTO orchestration_activity (timestamp, tool, project, session_id)
                VALUES (?, ?, ?, ?)
            """, rows)

    # --- Analysis helpers ---

    def get_daily_co2_stats(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get daily CO2 statistics for pattern analysis."""
        since = datetime.now() - timedelta(days=days)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT
                    date(timestamp) as date,
                    MIN(co2_ppm) as min_co2,
                    MAX(co2_ppm) as max_co2,
                    AVG(co2_ppm) as avg_co2,
                    COUNT(*) as readings
                FROM sensor_readings
                WHERE timestamp > ? AND co2_ppm IS NOT NULL
                GROUP BY date(timestamp)
                ORDER BY date DESC
            """, (since.isoformat(),)).fetchall()
            return [dict(row) for row in rows]

    def get_occupancy_patterns(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get occupancy patterns by hour of day."""
        since = datetime.now() - timedelta(days=days)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT
                    strftime('%H', timestamp) as hour,
                    state,
                    COUNT(*) as count
                FROM occupancy_log
                WHERE timestamp > ?
                GROUP BY hour, state
                ORDER BY hour
            """, (since.isoformat(),)).fetchall()
            return [dict(row) for row in rows]

    # --- History / trends ---

    def get_office_sessions(self, days: int = 7) -> Dict[str, Any]:
        """Get daily office sessions (arrival/departure/gaps) for the past N days."""
        now = self._now()
        since = now - timedelta(days=days)
        cutoff = self._format_timestamp(since)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT timestamp, state FROM occupancy_log
                WHERE timestamp > ?
                ORDER BY timestamp ASC
            """, (cutoff,)).fetchall()

        # Group transitions by date
        by_date: Dict[str, List] = {}
        for row in rows:
            ts = self._parse_timestamp(row["timestamp"])
            date_str = ts.strftime("%Y-%m-%d")
            by_date.setdefault(date_str, []).append((ts, row["state"]))

        sessions = []
        arrival_minutes = []
        departure_minutes = []

        for date_str, transitions in sorted(by_date.items()):
            # Find first PRESENT after 5am (ignore overnight sensor noise)
            first_present = None
            for ts, state in transitions:
                if state == "present" and first_present is None and ts.hour >= 5:
                    first_present = ts

            if first_present is None:
                continue

            arrival = first_present
            departure, departure_state = transitions[-1]
            if departure_state == "present" and arrival.date() == now.date():
                departure = now
            duration = (departure - arrival).total_seconds() / 3600

            # Find gaps (AWAY periods between arrival and departure)
            gaps = []
            gap_start = None
            for ts, state in transitions:
                if ts <= arrival or ts > departure:
                    continue
                if state == "away" and gap_start is None:
                    gap_start = ts
                elif state == "present" and gap_start is not None:
                    gap_dur = (ts - gap_start).total_seconds() / 60
                    if gap_dur >= 2:  # Only count gaps >= 2 min
                        gaps.append({
                            "left": gap_start.strftime("%H:%M:%S"),
                            "returned": ts.strftime("%H:%M:%S"),
                            "duration_min": round(gap_dur),
                        })
                        duration -= gap_dur / 60  # Subtract gap from duration
                    gap_start = None

            sessions.append({
                "date": date_str,
                "arrival": arrival.strftime("%H:%M:%S"),
                "departure": departure.strftime("%H:%M:%S"),
                "duration_hours": round(duration, 1),
                "gaps": gaps,
            })

            arr_min = arrival.hour * 60 + arrival.minute
            dep_min = departure.hour * 60 + departure.minute
            arrival_minutes.append(arr_min)
            departure_minutes.append(dep_min)

        # Compute summary
        summary = {
            "avg_arrival": "00:00:00",
            "avg_departure": "00:00:00",
            "avg_duration_hours": 0,
            "std_arrival_min": 0,
            "std_departure_min": 0,
            "total_hours_week": 0,
        }

        if sessions:
            avg_arr = sum(arrival_minutes) / len(arrival_minutes)
            avg_dep = sum(departure_minutes) / len(departure_minutes)
            total_hrs = sum(s["duration_hours"] for s in sessions)
            avg_dur = total_hrs / len(sessions)

            std_arr = math.sqrt(sum((m - avg_arr) ** 2 for m in arrival_minutes) / len(arrival_minutes)) if len(arrival_minutes) > 1 else 0
            std_dep = math.sqrt(sum((m - avg_dep) ** 2 for m in departure_minutes) / len(departure_minutes)) if len(departure_minutes) > 1 else 0

            def mins_to_time(m):
                h = int(m) // 60
                mi = int(m) % 60
                return f"{h:02d}:{mi:02d}:00"

            summary = {
                "avg_arrival": mins_to_time(avg_arr),
                "avg_departure": mins_to_time(avg_dep),
                "avg_duration_hours": round(avg_dur, 1),
                "std_arrival_min": round(std_arr),
                "std_departure_min": round(std_dep),
                "total_hours_week": round(total_hrs, 1),
            }

        return {"sessions": sessions, "summary": summary}

    def get_co2_ohlc(self, hours: int = 24, bucket_minutes: Optional[int] = None) -> Dict[str, Any]:
        """Get CO2 data aggregated into OHLC buckets."""
        if bucket_minutes is None:
            if hours <= 1:
                bucket_minutes = 5
            elif hours <= 6:
                bucket_minutes = 15
            elif hours <= 24:
                bucket_minutes = 60
            else:
                bucket_minutes = 240

        since = self._now() - timedelta(hours=hours)
        cutoff = self._format_timestamp(since)
        bucket_seconds = bucket_minutes * 60

        # Single query: get all readings with their bucket, then compute OHLC in Python
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT
                    datetime((strftime('%s', timestamp) / ?) * ?, 'unixepoch', 'localtime') as bucket,
                    co2_ppm,
                    timestamp
                FROM sensor_readings
                WHERE timestamp > ? AND co2_ppm IS NOT NULL
                ORDER BY timestamp ASC
            """, (bucket_seconds, bucket_seconds, cutoff)).fetchall()

        # Group by bucket and compute OHLC
        buckets: Dict[str, list] = {}
        for row in rows:
            buckets.setdefault(row["bucket"], []).append(row["co2_ppm"])

        candles = []
        for bucket_ts in sorted(buckets.keys()):
            values = buckets[bucket_ts]
            candles.append({
                "timestamp": bucket_ts,
                "open": values[0],
                "high": max(values),
                "low": min(values),
                "close": values[-1],
                "avg": round(sum(values) / len(values)),
                "readings": len(values),
            })

        return {"bucket_minutes": bucket_minutes, "candles": candles}

    def get_temperature_history(self, hours: int = 24, bucket_minutes: Optional[int] = None) -> Dict[str, Any]:
        """Get temperature data as time series for line chart."""
        if bucket_minutes is None:
            if hours <= 1:
                bucket_minutes = 5
            elif hours <= 6:
                bucket_minutes = 15
            elif hours <= 24:
                bucket_minutes = 30
            else:
                bucket_minutes = 120

        since = self._now() - timedelta(hours=hours)
        cutoff = self._format_timestamp(since)
        bucket_seconds = bucket_minutes * 60

        with self._connection() as conn:
            rows = conn.execute("""
                SELECT
                    datetime((strftime('%s', timestamp) / ?) * ?, 'unixepoch', 'localtime') as bucket,
                    AVG(temp_c) as avg_temp,
                    MIN(temp_c) as min_temp,
                    MAX(temp_c) as max_temp,
                    COUNT(*) as readings
                FROM sensor_readings
                WHERE timestamp > ? AND temp_c IS NOT NULL
                GROUP BY bucket
                ORDER BY bucket ASC
            """, (bucket_seconds, bucket_seconds, cutoff)).fetchall()

        points = []
        for row in rows:
            avg_c = row["avg_temp"]
            min_c = row["min_temp"]
            max_c = row["max_temp"]
            points.append({
                "timestamp": row["bucket"],
                "avg_f": round(avg_c * 9 / 5 + 32, 1),
                "min_f": round(min_c * 9 / 5 + 32, 1),
                "max_f": round(max_c * 9 / 5 + 32, 1),
                "readings": row["readings"],
            })

        return {"bucket_minutes": bucket_minutes, "points": points}

    def get_orchestration_activity(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get per-day orchestration counts and prompt timestamps."""
        now = self._now()
        day_labels = self._build_day_range(now, days)
        cutoff = self._format_timestamp(datetime.combine(
            now.date() - timedelta(days=max(days - 1, 0)),
            datetime.min.time(),
        ))

        with self._connection() as conn:
            rows = conn.execute("""
                SELECT timestamp, tool, session_id
                FROM orchestration_activity
                WHERE timestamp >= ?
                ORDER BY timestamp ASC
            """, (cutoff,)).fetchall()

        grouped: Dict[str, Dict[str, Any]] = {
            date_str: {
                "date": date_str,
                "messages": 0,
                "sessions": 0,
                "first_prompt": None,
                "last_prompt": None,
                "by_tool": {"claude": 0, "codex": 0},
                "timestamps": [],
            }
            for date_str in day_labels
        }
        sessions_by_date: Dict[str, set[str]] = {date_str: set() for date_str in day_labels}

        for row in rows:
            ts = self._parse_timestamp(row["timestamp"])
            date_str = ts.strftime("%Y-%m-%d")
            if date_str not in grouped:
                continue
            item = grouped[date_str]
            time_str = ts.strftime("%H:%M")
            item["messages"] += 1
            item["by_tool"][row["tool"]] = item["by_tool"].get(row["tool"], 0) + 1
            item["timestamps"].append({"time": time_str, "tool": row["tool"]})
            if item["first_prompt"] is None:
                item["first_prompt"] = time_str
            item["last_prompt"] = time_str
            sessions_by_date[date_str].add(row["session_id"])

        for date_str, session_ids in sessions_by_date.items():
            grouped[date_str]["sessions"] = len(session_ids)

        return [grouped[date_str] for date_str in day_labels]

    def get_project_focus(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get per-day project message distribution."""
        now = self._now()
        day_labels = self._build_day_range(now, days)
        cutoff = self._format_timestamp(datetime.combine(
            now.date() - timedelta(days=max(days - 1, 0)),
            datetime.min.time(),
        ))

        with self._connection() as conn:
            rows = conn.execute("""
                SELECT date(timestamp) AS date, project, COUNT(*) AS messages
                FROM orchestration_activity
                WHERE timestamp >= ?
                GROUP BY date(timestamp), project
                ORDER BY date(timestamp) ASC, messages DESC, project ASC
            """, (cutoff,)).fetchall()

        grouped: Dict[str, Dict[str, Any]] = {
            date_str: {
                "date": date_str,
                "total": 0,
                "projects": [],
            }
            for date_str in day_labels
        }

        for row in rows:
            date_str = row["date"]
            if date_str not in grouped:
                continue
            messages = int(row["messages"])
            grouped[date_str]["total"] += messages
            grouped[date_str]["projects"].append({
                "name": row["project"],
                "messages": messages,
            })

        return [grouped[date_str] for date_str in day_labels]

    def get_openings(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get per-day door and window open intervals."""
        now = self._now()
        start_dt = datetime.combine(
            now.date() - timedelta(days=max(days - 1, 0)),
            datetime.min.time(),
        )
        cutoff = self._format_timestamp(start_dt)
        day_labels = self._build_day_range(now, days)

        grouped: Dict[str, Dict[str, Any]] = {
            date_str: {
                "date": date_str,
                "door": [],
                "window": [],
            }
            for date_str in day_labels
        }

        with self._connection() as conn:
            for device_type in ("door", "window"):
                last_before_cutoff = conn.execute("""
                    SELECT timestamp, event
                    FROM device_events
                    WHERE timestamp < ? AND device_type = ?
                    ORDER BY timestamp DESC
                    LIMIT 1
                """, (cutoff, device_type)).fetchone()
                rows = conn.execute("""
                    SELECT timestamp, event
                    FROM device_events
                    WHERE timestamp >= ? AND device_type = ?
                    ORDER BY timestamp ASC
                """, (cutoff, device_type)).fetchall()

                open_start = start_dt if (
                    last_before_cutoff and last_before_cutoff["event"] == "open"
                ) else None

                for row in rows:
                    ts = self._parse_timestamp(row["timestamp"])
                    event = row["event"]
                    if event == "open":
                        if open_start is None:
                            open_start = ts
                    elif event == "closed" and open_start is not None:
                        for date_str, open_time, close_time in self._split_interval_by_date(open_start, ts):
                            if date_str in grouped:
                                grouped[date_str][device_type].append({
                                    "open": open_time,
                                    "close": close_time,
                                })
                        open_start = None

                if open_start is not None:
                    trailing_end = now
                    for date_str, open_time, close_time in self._split_interval_by_date(open_start, trailing_end):
                        if date_str in grouped:
                            grouped[date_str][device_type].append({
                                "open": open_time,
                                "close": close_time,
                            })

        return [grouped[date_str] for date_str in day_labels]

    def get_daily_stats(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get daily aggregate stats (door events, runtimes, presence hours)."""
        now = self._now()
        since = now - timedelta(days=days)
        cutoff = self._format_timestamp(since)

        with self._connection() as conn:
            # Door events per day
            door_rows = conn.execute("""
                SELECT date(timestamp) as date, COUNT(*) as count
                FROM device_events
                WHERE timestamp > ? AND device_type = 'door'
                GROUP BY date(timestamp)
            """, (cutoff,)).fetchall()
            door_by_date = {r["date"]: r["count"] for r in door_rows}

            # Presence hours per day from occupancy_log
            occ_rows = conn.execute("""
                SELECT timestamp, state FROM occupancy_log
                WHERE timestamp > ?
                ORDER BY timestamp ASC
            """, (cutoff,)).fetchall()
            last_occ_before_cutoff = conn.execute("""
                SELECT timestamp, state FROM occupancy_log
                WHERE timestamp <= ?
                ORDER BY timestamp DESC
                LIMIT 1
            """, (cutoff,)).fetchone()

            presence_by_date: Dict[str, float] = {}
            prev_ts = since if last_occ_before_cutoff and last_occ_before_cutoff["state"] == "present" else None
            prev_state = last_occ_before_cutoff["state"] if last_occ_before_cutoff else None
            for row in occ_rows:
                ts = self._parse_timestamp(row["timestamp"])
                if prev_ts and prev_state == "present":
                    self._accumulate_duration_by_date(
                        presence_by_date,
                        prev_ts,
                        ts,
                        seconds_per_unit=3600,
                    )
                prev_ts = ts
                prev_state = row["state"]

            if prev_ts and prev_state == "present":
                self._accumulate_duration_by_date(
                    presence_by_date,
                    prev_ts,
                    now,
                    seconds_per_unit=3600,
                )

            # ERV runtime per day
            erv_rows = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp > ? AND system = 'erv'
                ORDER BY timestamp ASC
            """, (cutoff,)).fetchall()
            last_erv_before_cutoff = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp <= ? AND system = 'erv'
                ORDER BY timestamp DESC
                LIMIT 1
            """, (cutoff,)).fetchone()

            erv_by_date: Dict[str, float] = {}
            erv_on_ts = (
                since
                if last_erv_before_cutoff and last_erv_before_cutoff["action"] in ("quiet", "medium", "turbo", "on")
                else None
            )
            for row in erv_rows:
                ts = self._parse_timestamp(row["timestamp"])
                action = row["action"]
                if action in ("quiet", "medium", "turbo", "on"):
                    if erv_on_ts is None:
                        erv_on_ts = ts
                elif action == "off" and erv_on_ts:
                    self._accumulate_duration_by_date(
                        erv_by_date,
                        erv_on_ts,
                        ts,
                        seconds_per_unit=60,
                    )
                    erv_on_ts = None

            if erv_on_ts:
                self._accumulate_duration_by_date(
                    erv_by_date,
                    erv_on_ts,
                    now,
                    seconds_per_unit=60,
                )

            # HVAC runtime per day
            hvac_rows = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp > ? AND system = 'hvac'
                ORDER BY timestamp ASC
            """, (cutoff,)).fetchall()
            last_hvac_before_cutoff = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp <= ? AND system = 'hvac'
                ORDER BY timestamp DESC
                LIMIT 1
            """, (cutoff,)).fetchone()

            hvac_by_date: Dict[str, float] = {}
            hvac_on_ts = (
                since
                if last_hvac_before_cutoff and last_hvac_before_cutoff["action"] in ("heat", "cool", "on")
                else None
            )
            for row in hvac_rows:
                ts = self._parse_timestamp(row["timestamp"])
                action = row["action"]
                if action in ("heat", "cool", "on"):
                    if hvac_on_ts is None:
                        hvac_on_ts = ts
                elif action == "off" and hvac_on_ts:
                    self._accumulate_duration_by_date(
                        hvac_by_date,
                        hvac_on_ts,
                        ts,
                        seconds_per_unit=60,
                    )
                    hvac_on_ts = None

            if hvac_on_ts:
                self._accumulate_duration_by_date(
                    hvac_by_date,
                    hvac_on_ts,
                    now,
                    seconds_per_unit=60,
                )

            # Collect all dates
            all_dates = set()
            all_dates.update(door_by_date.keys())
            all_dates.update(presence_by_date.keys())
            all_dates.update(erv_by_date.keys())
            all_dates.update(hvac_by_date.keys())

            stats = []
            for date_str in sorted(all_dates):
                stats.append({
                    "date": date_str,
                    "door_events": door_by_date.get(date_str, 0),
                    "erv_runtime_min": round(erv_by_date.get(date_str, 0)),
                    "hvac_runtime_min": round(hvac_by_date.get(date_str, 0)),
                    "presence_hours": round(presence_by_date.get(date_str, 0), 1),
                })

            return stats
