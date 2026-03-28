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
            """)
            logger.info(f"Database initialized at {self.db_path}")

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
        since = datetime.now() - timedelta(days=days)
        with self._connection() as conn:
            rows = conn.execute("""
                SELECT timestamp, state FROM occupancy_log
                WHERE timestamp > ?
                ORDER BY timestamp ASC
            """, (since.isoformat(),)).fetchall()

        # Group transitions by date
        by_date: Dict[str, List] = {}
        for row in rows:
            ts = datetime.strptime(row["timestamp"], "%Y-%m-%d %H:%M:%S")
            date_str = ts.strftime("%Y-%m-%d")
            by_date.setdefault(date_str, []).append((ts, row["state"]))

        sessions = []
        arrival_minutes = []
        departure_minutes = []

        for date_str, transitions in sorted(by_date.items()):
            # Find first PRESENT after 5am (ignore overnight sensor noise)
            first_present = None
            last_away = None
            for ts, state in transitions:
                if state == "present" and first_present is None and ts.hour >= 5:
                    first_present = ts
                if state == "away" and first_present is not None:
                    last_away = ts

            if first_present is None:
                continue

            arrival = first_present
            departure = last_away or transitions[-1][0]
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

        since = datetime.now() - timedelta(hours=hours)
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
            """, (bucket_seconds, bucket_seconds, since.isoformat())).fetchall()

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

        since = datetime.now() - timedelta(hours=hours)
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
            """, (bucket_seconds, bucket_seconds, since.isoformat())).fetchall()

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

    def get_daily_stats(self, days: int = 7) -> List[Dict[str, Any]]:
        """Get daily aggregate stats (door events, runtimes, presence hours)."""
        since = datetime.now() - timedelta(days=days)

        with self._connection() as conn:
            # Door events per day
            door_rows = conn.execute("""
                SELECT date(timestamp) as date, COUNT(*) as count
                FROM device_events
                WHERE timestamp > ? AND device_type = 'door'
                GROUP BY date(timestamp)
            """, (since.isoformat(),)).fetchall()
            door_by_date = {r["date"]: r["count"] for r in door_rows}

            # Presence hours per day from occupancy_log
            occ_rows = conn.execute("""
                SELECT timestamp, state FROM occupancy_log
                WHERE timestamp > ?
                ORDER BY timestamp ASC
            """, (since.isoformat(),)).fetchall()

            presence_by_date: Dict[str, float] = {}
            prev_ts = None
            prev_state = None
            for row in occ_rows:
                ts = datetime.strptime(row["timestamp"], "%Y-%m-%d %H:%M:%S")
                if prev_ts and prev_state == "present":
                    date_str = prev_ts.strftime("%Y-%m-%d")
                    dur_hrs = (ts - prev_ts).total_seconds() / 3600
                    # Cap at same-day boundary
                    if ts.date() == prev_ts.date():
                        presence_by_date[date_str] = presence_by_date.get(date_str, 0) + dur_hrs
                prev_ts = ts
                prev_state = row["state"]

            # ERV runtime per day
            erv_rows = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp > ? AND system = 'erv'
                ORDER BY timestamp ASC
            """, (since.isoformat(),)).fetchall()

            erv_by_date: Dict[str, float] = {}
            erv_on_ts = None
            for row in erv_rows:
                ts = datetime.strptime(row["timestamp"], "%Y-%m-%d %H:%M:%S")
                action = row["action"]
                if action in ("quiet", "medium", "turbo", "on"):
                    if erv_on_ts is None:
                        erv_on_ts = ts
                elif action == "off" and erv_on_ts:
                    date_str = erv_on_ts.strftime("%Y-%m-%d")
                    dur_min = (ts - erv_on_ts).total_seconds() / 60
                    erv_by_date[date_str] = erv_by_date.get(date_str, 0) + dur_min
                    erv_on_ts = None

            # HVAC runtime per day
            hvac_rows = conn.execute("""
                SELECT timestamp, action FROM climate_actions
                WHERE timestamp > ? AND system = 'hvac'
                ORDER BY timestamp ASC
            """, (since.isoformat(),)).fetchall()

            hvac_by_date: Dict[str, float] = {}
            hvac_on_ts = None
            for row in hvac_rows:
                ts = datetime.strptime(row["timestamp"], "%Y-%m-%d %H:%M:%S")
                action = row["action"]
                if action in ("heat", "cool", "on"):
                    if hvac_on_ts is None:
                        hvac_on_ts = ts
                elif action == "off" and hvac_on_ts:
                    date_str = hvac_on_ts.strftime("%Y-%m-%d")
                    dur_min = (ts - hvac_on_ts).total_seconds() / 60
                    hvac_by_date[date_str] = hvac_by_date.get(date_str, 0) + dur_min
                    hvac_on_ts = None

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
