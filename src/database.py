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
