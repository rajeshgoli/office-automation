use std::{fs, path::Path};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

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
}
