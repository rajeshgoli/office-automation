use std::{fs, path::Path};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::config::AppConfig;

pub fn migrate(config: &AppConfig) -> Result<()> {
    migrate_database(&config.runtime.database_path)
}

pub fn migrate_database(database_path: &Path) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;

    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sensor_readings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                co2_ppm INTEGER,
                temp_c REAL,
                humidity REAL,
                pm25 INTEGER,
                pm10 INTEGER,
                tvoc INTEGER,
                noise_db INTEGER,
                source TEXT DEFAULT 'qingping'
            );
            CREATE INDEX IF NOT EXISTS idx_sensor_timestamp ON sensor_readings(timestamp);

            CREATE TABLE IF NOT EXISTS occupancy_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                state TEXT NOT NULL,
                trigger TEXT,
                co2_ppm INTEGER,
                details TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_occupancy_timestamp ON occupancy_log(timestamp);

            CREATE TABLE IF NOT EXISTS device_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                device_type TEXT NOT NULL,
                device_name TEXT,
                event TEXT NOT NULL,
                details TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_device_timestamp ON device_events(timestamp);

            CREATE TABLE IF NOT EXISTS climate_actions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
                system TEXT NOT NULL,
                action TEXT NOT NULL,
                setpoint REAL,
                co2_ppm INTEGER,
                reason TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_climate_timestamp ON climate_actions(timestamp);

            CREATE TABLE IF NOT EXISTS orchestration_activity (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp DATETIME NOT NULL,
                tool TEXT NOT NULL CHECK(tool IN ('claude', 'codex')),
                project TEXT NOT NULL DEFAULT 'unknown',
                session_id TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_orch_timestamp ON orchestration_activity(timestamp);

            CREATE TABLE IF NOT EXISTS session_parser_state (
                source TEXT PRIMARY KEY,
                last_line INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS project_leverage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                date TEXT NOT NULL,
                project TEXT NOT NULL,
                metric TEXT NOT NULL,
                value REAL NOT NULL DEFAULT 0,
                UNIQUE(date, project, metric)
            );
            CREATE INDEX IF NOT EXISTS idx_proj_lev_date ON project_leverage(date);

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

            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );
            INSERT OR IGNORE INTO schema_migrations (version, name)
            VALUES (1, 'python_compat_foundation');
            PRAGMA user_version = 1;
            "#,
        )
        .context("failed to apply SQLite migrations")?;

    ensure_column(
        &connection,
        "sensor_readings",
        "noise_db",
        "ALTER TABLE sensor_readings ADD COLUMN noise_db INTEGER",
    )?;

    Ok(())
}

pub fn get_setting<T>(database_path: &Path, key: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to read app setting {key}"))?;

    value
        .map(|value| {
            serde_json::from_str(&value)
                .with_context(|| format!("invalid JSON in app setting {key}"))
        })
        .transpose()
}

pub fn set_setting<T>(database_path: &Path, key: &str, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let encoded = serde_json::to_string(value)
        .with_context(|| format!("failed to encode app setting {key}"))?;
    let updated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    connection
        .execute(
            r#"
            INSERT INTO app_settings (key, value, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at
            "#,
            params![key, encoded, updated_at],
        )
        .with_context(|| format!("failed to write app setting {key}"))?;

    Ok(())
}

pub fn log_occupancy_change(
    database_path: &Path,
    state: &str,
    trigger: Option<&str>,
    co2_ppm: Option<i64>,
    details: Option<&Value>,
) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let details = details
        .map(serde_json::to_string)
        .transpose()
        .context("failed to encode occupancy details")?;

    connection
        .execute(
            r#"
            INSERT INTO occupancy_log (timestamp, state, trigger, co2_ppm, details)
            VALUES (?, ?, ?, ?, ?)
            "#,
            params![timestamp, state, trigger, co2_ppm, details],
        )
        .context("failed to insert occupancy log")?;

    Ok(())
}

pub fn log_device_event(
    database_path: &Path,
    device_type: &str,
    event: &str,
    device_name: Option<&str>,
    details: Option<&Value>,
) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let details = details
        .map(serde_json::to_string)
        .transpose()
        .context("failed to encode device event details")?;

    connection
        .execute(
            r#"
            INSERT INTO device_events (timestamp, device_type, device_name, event, details)
            VALUES (?, ?, ?, ?, ?)
            "#,
            params![timestamp, device_type, device_name, event, details],
        )
        .context("failed to insert device event")?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRows {
    pub sensor_readings: Vec<Value>,
    pub occupancy_history: Vec<Value>,
    pub device_events: Vec<Value>,
    pub climate_actions: Vec<Value>,
}

pub fn read_history(database_path: &Path, hours: i64, limit: i64) -> Result<HistoryRows> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let since = (chrono::Local::now() - chrono::Duration::hours(hours))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    Ok(HistoryRows {
        sensor_readings: recent_rows(&connection, "sensor_readings", &since, limit)?,
        occupancy_history: recent_rows(&connection, "occupancy_log", &since, limit)?,
        device_events: recent_rows(&connection, "device_events", &since, limit)?,
        climate_actions: recent_rows(&connection, "climate_actions", &since, limit)?,
    })
}

fn recent_rows(
    connection: &Connection,
    table_name: &'static str,
    since: &str,
    limit: i64,
) -> Result<Vec<Value>> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT * FROM {table_name} WHERE timestamp > ? ORDER BY timestamp DESC LIMIT ?"
        ))
        .with_context(|| format!("failed to prepare history query for {table_name}"))?;
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let rows = statement
        .query_map(params![since, limit], |row| {
            let mut object = serde_json::Map::new();
            for (index, column) in columns.iter().enumerate() {
                object.insert(column.clone(), sqlite_value_to_json(row.get_ref(index)?));
            }
            Ok(Value::Object(object))
        })
        .with_context(|| format!("failed to query history rows for {table_name}"))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("failed to read history rows for {table_name}"))
}

fn sqlite_value_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => Value::from(value),
        ValueRef::Text(value) => Value::from(String::from_utf8_lossy(value).to_string()),
        ValueRef::Blob(value) => Value::from(String::from_utf8_lossy(value).to_string()),
    }
}

fn ensure_column(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
    alter_sql: &str,
) -> Result<()> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .with_context(|| format!("failed to inspect table {table_name}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("failed to read columns for table {table_name}"))?;

    for column in columns {
        if column? == column_name {
            return Ok(());
        }
    }

    connection
        .execute(alter_sql, [])
        .with_context(|| format!("failed to add column {column_name} to table {table_name}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");

        migrate_database(&db_path).expect("first migration");
        migrate_database(&db_path).expect("second migration");

        let connection = Connection::open(&db_path).expect("open migrated database");
        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sensor_readings'",
                [],
                |row| row.get(0),
            )
            .expect("query sensor table");
        let migration_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("query migrations");

        assert_eq!(table_count, 1);
        assert_eq!(migration_count, 1);
    }

    #[test]
    fn migration_adds_noise_db_to_legacy_sensor_table() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");

        {
            let connection = Connection::open(&db_path).expect("open database");
            connection
                .execute_batch(
                    r#"
                    CREATE TABLE sensor_readings (
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
                    "#,
                )
                .expect("create legacy sensor table");
        }

        migrate_database(&db_path).expect("migration");

        let connection = Connection::open(&db_path).expect("open migrated database");
        connection
            .execute(
                r#"
                INSERT INTO sensor_readings
                    (co2_ppm, temp_c, humidity, pm25, pm10, tvoc, noise_db, source)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                (450, 21.0, 45.0, 1, 2, 3, 36, "qingping"),
            )
            .expect("insert sensor reading with noise_db");
    }

    #[test]
    fn app_settings_round_trip_json_values() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");

        let value = serde_json::json!({
            "heat_on_temp_f": 70,
            "heat_off_temp_f": 74,
            "cool_off_temp_f": 78,
            "cool_on_temp_f": 82,
        });
        set_setting(&db_path, "hvac_temperature_bands", &value).expect("write setting");

        let stored: serde_json::Value = get_setting(&db_path, "hvac_temperature_bands")
            .expect("read setting")
            .expect("value");
        assert_eq!(stored, value);
    }

    #[test]
    fn logs_presence_and_reads_history_rows() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");

        log_device_event(
            &db_path,
            "presence",
            "manual_present",
            Some("Dashboard"),
            Some(&serde_json::json!({"state": "present"})),
        )
        .expect("log device");
        log_occupancy_change(&db_path, "present", Some("manual"), Some(500), None)
            .expect("log occupancy");

        let history = read_history(&db_path, 1, 100).expect("history");

        assert_eq!(history.device_events.len(), 1);
        assert_eq!(history.device_events[0]["device_type"], "presence");
        assert_eq!(history.device_events[0]["event"], "manual_present");
        assert_eq!(history.occupancy_history.len(), 1);
        assert_eq!(history.occupancy_history[0]["state"], "present");
        assert_eq!(history.occupancy_history[0]["trigger"], "manual");
        assert_eq!(history.occupancy_history[0]["co2_ppm"], 500);
    }
}
