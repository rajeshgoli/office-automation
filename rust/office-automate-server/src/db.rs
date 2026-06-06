use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, Timelike};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{config::AppConfig, qingping::QingpingReading};

const SESSION_OUTPUT_SCHEMA: &str = r#"
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
"#;

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

pub fn insert_sensor_reading(database_path: &Path, reading: &QingpingReading) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    connection
        .execute(
            r#"
            INSERT INTO sensor_readings
                (timestamp, co2_ppm, temp_c, humidity, pm25, pm10, tvoc, noise_db, source)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            params![
                reading.database_timestamp(),
                reading.co2_ppm,
                reading.temp_c,
                reading.humidity,
                reading.pm25,
                reading.pm10,
                reading.tvoc,
                reading.noise_db,
                "qingping",
            ],
        )
        .context("failed to insert Qingping sensor reading")?;

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

pub fn log_climate_action(
    database_path: &Path,
    system: &str,
    action: &str,
    setpoint: Option<f64>,
    co2_ppm: Option<i64>,
    reason: Option<&str>,
) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }

    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    connection
        .execute(
            r#"
            INSERT INTO climate_actions (timestamp, system, action, setpoint, co2_ppm, reason)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
            params![timestamp, system, action, setpoint, co2_ppm, reason],
        )
        .with_context(|| format!("failed to log {system} climate action"))?;

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

pub fn read_office_sessions(database_path: &Path, days: i64) -> Result<Value> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let cutoff = format_timestamp(now - Duration::days(days));
    let rows = query_text_pairs(
        &connection,
        "SELECT timestamp, state FROM occupancy_log WHERE timestamp > ? ORDER BY timestamp ASC",
        &cutoff,
    )?;

    let mut by_date: BTreeMap<String, Vec<(NaiveDateTime, String)>> = BTreeMap::new();
    for (timestamp, state) in rows {
        if let Some(parsed) = parse_timestamp(&timestamp) {
            by_date
                .entry(parsed.date().to_string())
                .or_default()
                .push((parsed, state));
        }
    }

    let mut sessions = Vec::new();
    let mut arrival_minutes = Vec::new();
    let mut departure_minutes = Vec::new();

    for (date, transitions) in by_date {
        let Some(arrival) = transitions.iter().find_map(|(timestamp, state)| {
            (state == "present" && timestamp.hour() >= 5).then_some(*timestamp)
        }) else {
            continue;
        };

        let (mut departure, departure_state) = transitions
            .last()
            .map(|(timestamp, state)| (*timestamp, state.as_str()))
            .expect("non-empty transitions");
        if departure_state == "present" && arrival.date() == now.date() {
            departure = now;
        }

        let mut duration_hours = (departure - arrival).num_seconds() as f64 / 3600.0;
        let mut gaps = Vec::new();
        let mut gap_start = None;
        for (timestamp, state) in &transitions {
            if *timestamp <= arrival || *timestamp > departure {
                continue;
            }
            if state == "away" && gap_start.is_none() {
                gap_start = Some(*timestamp);
            } else if state == "present" {
                if let Some(start) = gap_start.take() {
                    let duration_min = (timestamp.signed_duration_since(start).num_seconds() as f64
                        / 60.0)
                        .round() as i64;
                    if duration_min >= 2 {
                        gaps.push(json!({
                            "left": start.format("%H:%M:%S").to_string(),
                            "returned": timestamp.format("%H:%M:%S").to_string(),
                            "duration_min": duration_min,
                        }));
                        duration_hours -= duration_min as f64 / 60.0;
                    }
                }
            }
        }

        arrival_minutes.push((arrival.hour() * 60 + arrival.minute()) as f64);
        departure_minutes.push((departure.hour() * 60 + departure.minute()) as f64);
        sessions.push(json!({
            "date": date,
            "arrival": arrival.format("%H:%M:%S").to_string(),
            "departure": departure.format("%H:%M:%S").to_string(),
            "duration_hours": round1(duration_hours),
            "gaps": gaps,
        }));
    }

    let summary = if sessions.is_empty() {
        json!({
            "avg_arrival": "00:00:00",
            "avg_departure": "00:00:00",
            "avg_duration_hours": 0,
            "std_arrival_min": 0,
            "std_departure_min": 0,
            "total_hours_week": 0,
        })
    } else {
        let avg_arrival = average(&arrival_minutes);
        let avg_departure = average(&departure_minutes);
        let durations = sessions
            .iter()
            .filter_map(|session| session.get("duration_hours").and_then(Value::as_f64))
            .collect::<Vec<_>>();
        let total_hours = durations.iter().sum::<f64>();
        json!({
            "avg_arrival": minutes_to_time(avg_arrival),
            "avg_departure": minutes_to_time(avg_departure),
            "avg_duration_hours": round1(total_hours / durations.len() as f64),
            "std_arrival_min": stddev(&arrival_minutes, avg_arrival).round() as i64,
            "std_departure_min": stddev(&departure_minutes, avg_departure).round() as i64,
            "total_hours_week": round1(total_hours),
        })
    };

    Ok(json!({"sessions": sessions, "summary": summary}))
}

pub fn read_co2_ohlc(database_path: &Path, hours: i64, bucket_minutes: i64) -> Result<Value> {
    let rows = query_sensor_points(database_path, "co2_ppm", hours)?;
    let mut buckets: BTreeMap<NaiveDateTime, Vec<i64>> = BTreeMap::new();
    for (timestamp, value) in rows {
        if let (Some(timestamp), Some(value)) = (parse_timestamp(&timestamp), value.as_i64()) {
            buckets
                .entry(bucket_start(timestamp, bucket_minutes))
                .or_default()
                .push(value);
        }
    }

    let candles = buckets
        .into_iter()
        .map(|(bucket, values)| {
            let sum: i64 = values.iter().sum();
            json!({
                "timestamp": format_timestamp(bucket),
                "open": values[0],
                "high": values.iter().max().copied().unwrap_or_default(),
                "low": values.iter().min().copied().unwrap_or_default(),
                "close": values[values.len() - 1],
                "avg": (sum as f64 / values.len() as f64).round() as i64,
                "readings": values.len(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({"bucket_minutes": bucket_minutes, "candles": candles}))
}

pub fn read_temperature_history(
    database_path: &Path,
    hours: i64,
    bucket_minutes: i64,
) -> Result<Value> {
    let rows = query_sensor_points(database_path, "temp_c", hours)?;
    let mut buckets: BTreeMap<NaiveDateTime, Vec<f64>> = BTreeMap::new();
    for (timestamp, value) in rows {
        if let (Some(timestamp), Some(value)) = (parse_timestamp(&timestamp), value.as_f64()) {
            buckets
                .entry(bucket_start(timestamp, bucket_minutes))
                .or_default()
                .push(value);
        }
    }

    let points = buckets
        .into_iter()
        .map(|(bucket, values)| {
            let avg_c = values.iter().sum::<f64>() / values.len() as f64;
            let min_c = values.iter().copied().fold(f64::INFINITY, f64::min);
            let max_c = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            json!({
                "timestamp": format_timestamp(bucket),
                "avg_f": round1(c_to_f(avg_c)),
                "min_f": round1(c_to_f(min_c)),
                "max_f": round1(c_to_f(max_c)),
                "readings": values.len(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({"bucket_minutes": bucket_minutes, "points": points}))
}

pub fn read_daily_stats(database_path: &Path, days: i64) -> Result<Vec<Value>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let start = day_start(now.date() - Duration::days(days - 1));
    let cutoff = format_timestamp(start);
    let labels = day_labels(now.date(), days);

    let mut door_counts = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date, COUNT(*) AS count FROM device_events WHERE timestamp >= ? AND device_type = 'door' GROUP BY date(timestamp)",
    )?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (date, count) = row?;
        door_counts.insert(date, count);
    }

    let presence = durations_from_state_table(
        &connection,
        "occupancy_log",
        "state",
        "present",
        &cutoff,
        start,
        now,
        3600.0,
        None,
    )?;
    let erv = durations_from_state_table(
        &connection,
        "climate_actions",
        "action",
        "off",
        &cutoff,
        start,
        now,
        60.0,
        Some("erv"),
    )?;
    let hvac = durations_from_state_table(
        &connection,
        "climate_actions",
        "action",
        "off",
        &cutoff,
        start,
        now,
        60.0,
        Some("hvac"),
    )?;

    Ok(labels
        .into_iter()
        .map(|date| {
            json!({
                "date": date,
                "door_events": door_counts.get(&date).copied().unwrap_or_default(),
                "erv_runtime_min": erv.get(&date).copied().unwrap_or_default().round() as i64,
                "hvac_runtime_min": hvac.get(&date).copied().unwrap_or_default().round() as i64,
                "presence_hours": round1(presence.get(&date).copied().unwrap_or_default()),
            })
        })
        .collect())
}

pub fn read_orchestration_activity(database_path: &Path, days: i64) -> Result<Vec<Value>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let labels = day_labels(now.date(), days);
    let cutoff = format_timestamp(day_start(now.date() - Duration::days(days - 1)));
    let mut grouped = labels
        .iter()
        .map(|date| {
            (
                date.clone(),
                json!({
                    "date": date,
                    "messages": 0,
                    "sessions": 0,
                    "first_prompt": Value::Null,
                    "last_prompt": Value::Null,
                    "by_tool": {"claude": 0, "codex": 0},
                    "timestamps": [],
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut sessions: HashMap<String, HashSet<String>> = HashMap::new();

    let mut statement = connection.prepare(
        "SELECT timestamp, tool, session_id FROM orchestration_activity WHERE timestamp >= ? ORDER BY timestamp ASC",
    )?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (timestamp, tool, session_id) = row?;
        let Some(parsed) = parse_timestamp(&timestamp) else {
            continue;
        };
        let date = parsed.date().to_string();
        let Some(item) = grouped.get_mut(&date) else {
            continue;
        };
        let time = parsed.format("%H:%M").to_string();
        item["messages"] = json!(item["messages"].as_i64().unwrap_or_default() + 1);
        item["by_tool"][&tool] = json!(item["by_tool"][&tool].as_i64().unwrap_or_default() + 1);
        item["timestamps"]
            .as_array_mut()
            .expect("timestamps")
            .push(json!({"time": time, "tool": tool}));
        if item["first_prompt"].is_null() {
            item["first_prompt"] = json!(time);
        }
        item["last_prompt"] = json!(time);
        sessions.entry(date).or_default().insert(session_id);
    }

    for (date, session_ids) in sessions {
        if let Some(item) = grouped.get_mut(&date) {
            item["sessions"] = json!(session_ids.len());
        }
    }

    Ok(labels
        .into_iter()
        .filter_map(|date| grouped.remove(&date))
        .collect())
}

pub fn read_project_focus(database_path: &Path, days: i64) -> Result<Vec<Value>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let labels = day_labels(now.date(), days);
    let cutoff = format_timestamp(day_start(now.date() - Duration::days(days - 1)));
    let mut grouped: BTreeMap<String, HashMap<String, ProjectFocusItem>> = BTreeMap::new();

    let mut statement = connection.prepare(
        "SELECT timestamp, project FROM orchestration_activity WHERE timestamp >= ? ORDER BY timestamp ASC",
    )?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (timestamp, project) = row?;
        let Some(parsed) = parse_timestamp(&timestamp) else {
            continue;
        };
        let date = parsed.date().to_string();
        if !labels.contains(&date) {
            continue;
        }
        let name = normalize_project_name(&project);
        let time = parsed.format("%H:%M").to_string();
        let item = grouped
            .entry(date)
            .or_default()
            .entry(name.clone())
            .or_insert(ProjectFocusItem {
                name,
                messages: 0,
                first_prompt: time.clone(),
                last_prompt: time.clone(),
            });
        item.messages += 1;
        if time < item.first_prompt {
            item.first_prompt = time.clone();
        }
        if time > item.last_prompt {
            item.last_prompt = time;
        }
    }

    Ok(labels
        .into_iter()
        .map(|date| {
            let mut projects = grouped
                .remove(&date)
                .unwrap_or_default()
                .into_values()
                .collect::<Vec<_>>();
            projects.sort_by(|a, b| b.messages.cmp(&a.messages).then_with(|| a.name.cmp(&b.name)));
            let total: i64 = projects.iter().map(|project| project.messages).sum();
            json!({
                "date": date,
                "total": total,
                "projects": projects.into_iter().map(ProjectFocusItem::into_value).collect::<Vec<_>>(),
            })
        })
        .collect())
}

pub fn read_openings(database_path: &Path, days: i64) -> Result<Vec<Value>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let start = day_start(now.date() - Duration::days(days - 1));
    let cutoff = format_timestamp(start);
    let labels = day_labels(now.date(), days);
    let mut grouped = labels
        .iter()
        .map(|date| {
            (
                date.clone(),
                json!({"date": date, "door": [], "window": []}),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for device_type in ["door", "window"] {
        let last_before: Option<String> = connection
            .query_row(
                "SELECT event FROM device_events WHERE timestamp < ? AND device_type = ? ORDER BY timestamp DESC LIMIT 1",
                params![cutoff, device_type],
                |row| row.get(0),
            )
            .optional()?;
        let mut open_start = (last_before.as_deref() == Some("open")).then_some(start);

        let mut statement = connection.prepare(
            "SELECT timestamp, event FROM device_events WHERE timestamp >= ? AND device_type = ? ORDER BY timestamp ASC",
        )?;
        let rows = statement.query_map(params![cutoff, device_type], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (timestamp, event) = row?;
            let Some(parsed) = parse_timestamp(&timestamp) else {
                continue;
            };
            if event == "open" && open_start.is_none() {
                open_start = Some(parsed);
            } else if event == "closed" {
                if let Some(started) = open_start.take() {
                    push_open_intervals(&mut grouped, device_type, started, parsed);
                }
            }
        }
        if let Some(started) = open_start {
            push_open_intervals(&mut grouped, device_type, started, now);
        }
    }

    Ok(labels
        .into_iter()
        .filter_map(|date| grouped.remove(&date))
        .collect())
}

pub fn read_leverage_history(database_path: &Path, days: i64) -> Result<Value> {
    let telemetry_path = telemetry_database_path(database_path);
    read_leverage_history_with_telemetry(database_path, &telemetry_path, days)
}

pub fn read_leverage_history_with_telemetry(
    database_path: &Path,
    telemetry_database_path: &Path,
    days: i64,
) -> Result<Value> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let labels = day_labels(now.date(), days);
    let cutoff = format_timestamp(day_start(now.date() - Duration::days(days - 1)));
    let orchestration_datetime = sqlite_local_datetime_expr("timestamp");
    let orchestration_date = format!("date({orchestration_datetime})");
    let pr_created_datetime = sqlite_local_datetime_expr("created_at");
    let pr_created_date = format!("date({pr_created_datetime})");
    let pr_merged_datetime = sqlite_local_datetime_expr("merged_at");
    let pr_merged_date = format!("date({pr_merged_datetime})");
    let mut grouped = labels
        .iter()
        .map(|date| (date.clone(), LeverageDay::new(date)))
        .collect::<BTreeMap<_, _>>();

    let mut statement = connection.prepare(&format!(
        "SELECT {orchestration_date} AS date, COUNT(*) AS prompts, COUNT(DISTINCT session_id) AS sessions \
         FROM orchestration_activity WHERE {orchestration_datetime} >= ? GROUP BY {orchestration_date}"
    ))?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (date, prompts, sessions) = row?;
        if let Some(day) = grouped.get_mut(&date) {
            day.prompts = prompts;
            day.sessions = sessions;
        }
    }

    for row in read_leverage_session_rows(&connection, telemetry_database_path, &cutoff)? {
        if let Some(day) = grouped.get_mut(&row.date) {
            day.lines_added = row.lines_added;
            day.lines_removed = row.lines_removed;
            day.files_modified = row.files_modified;
            day.commits = row.commits;
            day.duration_minutes = row.duration_minutes;
        }
    }

    let mut statement = connection.prepare(&format!(
        "SELECT {pr_created_date} AS date, COUNT(*) AS count \
         FROM github_prs WHERE {pr_created_datetime} >= ? GROUP BY {pr_created_date}"
    ))?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (date, count) = row?;
        if let Some(day) = grouped.get_mut(&date) {
            day.prs_opened = count;
        }
    }

    let mut statement = connection.prepare(&format!(
        "SELECT {pr_merged_date} AS date, COUNT(*) AS count, SUM((julianday(merged_at) - julianday(created_at)) * 24.0) AS hours \
         FROM github_prs WHERE merged_at IS NOT NULL AND {pr_merged_datetime} >= ? GROUP BY {pr_merged_date}"
    ))?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<f64>>(2)?.unwrap_or_default(),
        ))
    })?;
    for row in rows {
        let (date, count, hours) = row?;
        if let Some(day) = grouped.get_mut(&date) {
            day.prs_merged = count;
            day.pr_cycle_hours_total = hours;
        }
    }

    let mut week = LeverageWeek::default();
    let days_payload = labels
        .into_iter()
        .filter_map(|date| grouped.remove(&date))
        .map(|day| {
            week.add(&day);
            day.into_value()
        })
        .collect::<Vec<_>>();

    Ok(json!({"days": days_payload, "week": week.into_value()}))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeverageSessionRow {
    date: String,
    lines_added: i64,
    lines_removed: i64,
    files_modified: i64,
    commits: i64,
    duration_minutes: i64,
}

fn read_leverage_session_rows(
    connection: &Connection,
    telemetry_database_path: &Path,
    cutoff: &str,
) -> Result<Vec<LeverageSessionRow>> {
    ensure_telemetry_database(telemetry_database_path)?;
    let telemetry_path = telemetry_database_path.to_string_lossy().to_string();
    connection
        .execute("ATTACH DATABASE ? AS telemetry", params![telemetry_path])
        .context("failed to attach telemetry database")?;

    let result = query_attached_leverage_session_rows(connection, cutoff);
    let detach_result = connection
        .execute_batch("DETACH DATABASE telemetry")
        .context("failed to detach telemetry database");

    match (result, detach_result) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn query_attached_leverage_session_rows(
    connection: &Connection,
    cutoff: &str,
) -> Result<Vec<LeverageSessionRow>> {
    let session_datetime = sqlite_local_datetime_expr("start_time");
    let session_date = format!("date({session_datetime})");
    let mut statement = connection.prepare(&format!(
        "SELECT {session_date} AS date, \
                SUM(lines_added) AS lines_added, \
                SUM(lines_removed) AS lines_removed, \
                SUM(files_modified) AS files_modified, \
                SUM(git_commits) AS commits, \
                SUM(duration_minutes) AS duration_minutes \
         FROM telemetry.session_output \
         WHERE {session_datetime} >= ? \
         GROUP BY {session_date}"
    ))?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok(LeverageSessionRow {
            date: row.get(0)?,
            lines_added: row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
            lines_removed: row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            files_modified: row.get::<_, Option<i64>>(3)?.unwrap_or_default(),
            commits: row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            duration_minutes: row.get::<_, Option<i64>>(5)?.unwrap_or_default(),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read leverage session rows")
}

pub fn telemetry_database_path(database_path: &Path) -> PathBuf {
    database_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("telemetry.db")
}

pub fn ensure_telemetry_database(database_path: &Path) -> Result<()> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create telemetry data directory {}",
                parent.display()
            )
        })?;
    }

    let connection = Connection::open(database_path).with_context(|| {
        format!(
            "failed to open telemetry SQLite database {}",
            database_path.display()
        )
    })?;
    connection
        .execute_batch(SESSION_OUTPUT_SCHEMA)
        .context("failed to apply telemetry SQLite schema")
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionOutputRow {
    pub session_id: String,
    pub project: String,
    pub start_time: String,
    pub duration_minutes: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub files_modified: i64,
    pub git_commits: i64,
    pub git_pushes: i64,
    pub user_message_count: i64,
    pub assistant_message_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub tool_counts: Option<String>,
    pub languages: Option<String>,
    pub is_human_session: bool,
}

pub fn upsert_collector_session_output_rows(
    telemetry_database_path: &Path,
    rows: &[SessionOutputRow],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    ensure_telemetry_database(telemetry_database_path)?;
    let mut connection = Connection::open(telemetry_database_path).with_context(|| {
        format!(
            "failed to open telemetry SQLite database {}",
            telemetry_database_path.display()
        )
    })?;
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(
            "INSERT INTO session_output (
                session_id, project, start_time, duration_minutes, lines_added, lines_removed,
                files_modified, git_commits, git_pushes, user_message_count,
                assistant_message_count, input_tokens, output_tokens, tool_counts, languages,
                is_human_session
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(session_id) DO UPDATE SET
                project = excluded.project,
                start_time = excluded.start_time,
                duration_minutes = excluded.duration_minutes,
                lines_added = excluded.lines_added,
                lines_removed = excluded.lines_removed,
                files_modified = excluded.files_modified,
                git_commits = excluded.git_commits,
                git_pushes = excluded.git_pushes,
                tool_counts = excluded.tool_counts,
                is_human_session = excluded.is_human_session
            WHERE session_output.user_message_count = 0
              AND session_output.assistant_message_count = 0
              AND session_output.input_tokens = 0
              AND session_output.output_tokens = 0",
        )?;
        for row in rows {
            statement.execute(params![
                row.session_id,
                row.project,
                row.start_time,
                row.duration_minutes,
                row.lines_added,
                row.lines_removed,
                row.files_modified,
                row.git_commits,
                row.git_pushes,
                row.user_message_count,
                row.assistant_message_count,
                row.input_tokens,
                row.output_tokens,
                row.tool_counts,
                row.languages,
                if row.is_human_session { 1 } else { 0 },
            ])?;
        }
    }
    transaction.commit()?;
    Ok(rows.len())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectLeverageRow {
    pub date: String,
    pub project: String,
    pub metric: String,
    pub value: f64,
}

pub fn upsert_project_leverage_rows(
    database_path: &Path,
    rows: &[ProjectLeverageRow],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    migrate_database(database_path)?;
    let mut connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(
            "INSERT INTO project_leverage (date, project, metric, value)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(date, project, metric) DO UPDATE SET value = excluded.value",
        )?;
        for row in rows {
            statement.execute(params![row.date, row.project, row.metric, row.value])?;
        }
    }
    transaction.commit()?;
    Ok(rows.len())
}

pub fn read_project_leverage(database_path: &Path, days: i64) -> Result<Value> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let now = Local::now().naive_local();
    let labels = day_labels(now.date(), days);
    let since = (now.date() - Duration::days(days - 1)).to_string();
    let mut by_project: HashMap<String, HashMap<String, HashMap<String, f64>>> = HashMap::new();
    let mut persona_projects = HashSet::new();

    let mut statement = connection.prepare(
        "SELECT date, project, metric, value FROM project_leverage WHERE date >= ? ORDER BY date ASC, project ASC, metric ASC",
    )?;
    let rows = statement.query_map(params![since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
        ))
    })?;
    for row in rows {
        let (date, project, metric, value) = row?;
        if project == "agent-os" && metric.starts_with("persona_project::") {
            persona_projects.insert(metric.trim_start_matches("persona_project::").to_string());
        }
        by_project
            .entry(project)
            .or_default()
            .entry(date)
            .or_default()
            .insert(metric, value);
    }

    let session_metrics = [
        "sm_dispatches",
        "sm_sends",
        "sm_reminds",
        "sm_active_sessions",
        "sm_telegram_in",
        "sm_telegram_out",
    ];
    let engram_metrics = [
        "engram_last_fold_age_hours",
        "engram_folds_7d",
        "engram_active_concepts",
    ];
    let agent_metrics = ["persona_reads", "persona_projects"];
    let office_metrics = ["automation_events", "state_transitions"];

    let session_days =
        project_leverage_days(by_project.get("session-manager"), &session_metrics, &labels);
    let session_week = sum_project_days(&session_days, &session_metrics);
    let engram_days = project_leverage_days(by_project.get("engram"), &engram_metrics, &labels);
    let latest_engram = by_project.get("engram").and_then(|days| {
        days.iter()
            .max_by_key(|(date, _)| *date)
            .map(|(_, metrics)| metrics)
    });
    let engram_current = json!({
        "last_fold_age_hours": metric_value(latest_engram.and_then(|metrics| metrics.get("engram_last_fold_age_hours")).copied()),
        "folds_7d": metric_value(Some(latest_engram.and_then(|metrics| metrics.get("engram_folds_7d")).copied().unwrap_or_default())),
        "active_concepts": metric_value(Some(latest_engram.and_then(|metrics| metrics.get("engram_active_concepts")).copied().unwrap_or_default())),
    });
    let agent_days = project_leverage_days(by_project.get("agent-os"), &agent_metrics, &labels);
    let persona_project_count = if persona_projects.is_empty() {
        sum_metric(&agent_days, "persona_projects")
    } else {
        persona_projects.len() as f64
    };
    let agent_week = json!({
        "persona_reads": metric_value(Some(sum_metric(&agent_days, "persona_reads"))),
        "persona_projects": metric_value(Some(persona_project_count)),
    });
    let office_days =
        project_leverage_days(by_project.get("office-automate"), &office_metrics, &labels);
    let office_week = sum_project_days(&office_days, &office_metrics);

    Ok(json!({
        "projects": {
            "session-manager": {
                "summary": summarize_session_manager(&session_week, days),
                "days": session_days,
                "week": session_week,
            },
            "engram": {
                "summary": summarize_engram(&engram_current),
                "days": engram_days,
                "current": engram_current,
            },
            "agent-os": {
                "summary": summarize_agent_os(&agent_week, days),
                "days": agent_days,
                "week": agent_week,
            },
            "office-automate": {
                "summary": summarize_office_automate(&office_week, days),
                "days": office_days,
                "week": office_week,
            },
        }
    }))
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

pub fn get_latest_device_state(database_path: &Path, device_type: &str) -> Result<Option<String>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    connection
        .query_row(
            r#"
            SELECT event FROM device_events
            WHERE device_type = ?
            ORDER BY timestamp DESC, id DESC
            LIMIT 1
            "#,
            params![device_type],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to read latest {device_type} device state"))
}

fn parse_timestamp(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .or_else(|| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").ok())
        .or_else(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|timestamp| timestamp.with_timezone(&Local).naive_local())
        })
}

fn sqlite_local_datetime_expr(column: &str) -> String {
    let has_explicit_timezone =
        format!("({column} LIKE '%Z' OR {column} LIKE '%+__:__' OR {column} LIKE '%-__:__')");
    format!(
        "CASE WHEN {has_explicit_timezone} THEN datetime({column}, 'localtime') ELSE datetime({column}) END"
    )
}

fn format_timestamp(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn day_start(date: NaiveDate) -> NaiveDateTime {
    date.and_hms_opt(0, 0, 0).expect("valid start of day")
}

fn day_labels(today: NaiveDate, days: i64) -> Vec<String> {
    (0..days)
        .rev()
        .map(|offset| (today - Duration::days(offset)).to_string())
        .collect()
}

fn bucket_start(timestamp: NaiveDateTime, bucket_minutes: i64) -> NaiveDateTime {
    let bucket_minutes = bucket_minutes.max(1) as u32;
    let minutes = timestamp.hour() * 60 + timestamp.minute();
    let bucket = (minutes / bucket_minutes) * bucket_minutes;
    timestamp
        .date()
        .and_hms_opt(bucket / 60, bucket % 60, 0)
        .expect("valid bucket")
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn c_to_f(value: f64) -> f64 {
    value * 9.0 / 5.0 + 32.0
}

fn average(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn stddev(values: &[f64], average: f64) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    (values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt()
}

fn minutes_to_time(minutes: f64) -> String {
    let minutes = minutes as i64;
    format!("{:02}:{:02}:00", minutes / 60, minutes % 60)
}

fn query_text_pairs(
    connection: &Connection,
    sql: &str,
    parameter: &str,
) -> Result<Vec<(String, String)>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![parameter], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read text-pair rows")
}

fn query_sensor_points(
    database_path: &Path,
    column: &'static str,
    hours: i64,
) -> Result<Vec<(String, Value)>> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let cutoff = format_timestamp(Local::now().naive_local() - Duration::hours(hours));
    let mut statement = connection.prepare(&format!(
        "SELECT timestamp, {column} FROM sensor_readings WHERE timestamp > ? AND {column} IS NOT NULL ORDER BY timestamp ASC"
    ))?;
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            sqlite_value_to_json(row.get_ref(1)?),
        ))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read sensor points")
}

fn durations_from_state_table(
    connection: &Connection,
    table: &'static str,
    state_column: &'static str,
    active_value: &'static str,
    cutoff: &str,
    start: NaiveDateTime,
    now: NaiveDateTime,
    seconds_per_unit: f64,
    system: Option<&'static str>,
) -> Result<HashMap<String, f64>> {
    let last_state: Option<String> = if let Some(system) = system {
        connection
            .query_row(
                &format!(
                    "SELECT {state_column} FROM {table} WHERE timestamp <= ? AND system = ? ORDER BY timestamp DESC LIMIT 1"
                ),
                params![cutoff, system],
                |row| row.get(0),
            )
            .optional()?
    } else {
        connection
            .query_row(
                &format!(
                    "SELECT {state_column} FROM {table} WHERE timestamp <= ? ORDER BY timestamp DESC LIMIT 1"
                ),
                params![cutoff],
                |row| row.get(0),
            )
            .optional()?
    };

    let mut statement = if system.is_some() {
        connection.prepare(&format!(
            "SELECT timestamp, {state_column} FROM {table} WHERE timestamp > ? AND system = ? ORDER BY timestamp ASC"
        ))?
    } else {
        connection.prepare(&format!(
            "SELECT timestamp, {state_column} FROM {table} WHERE timestamp > ? ORDER BY timestamp ASC"
        ))?
    };
    let rows = if let Some(system) = system {
        statement
            .query_map(params![cutoff, system], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        statement
            .query_map(params![cutoff], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut totals = HashMap::new();
    let mut previous_timestamp = last_state.as_ref().map(|_| start);
    let mut previous_active = last_state.as_deref().is_some_and(|state| {
        if system.is_some() {
            state != active_value
        } else {
            state == active_value
        }
    });

    for (timestamp, state) in rows {
        let Some(parsed) = parse_timestamp(&timestamp) else {
            continue;
        };
        if previous_active {
            if let Some(started) = previous_timestamp {
                accumulate_duration(&mut totals, started, parsed, seconds_per_unit);
            }
        }
        previous_timestamp = Some(parsed);
        previous_active = if system.is_some() {
            state != active_value
        } else {
            state == active_value
        };
    }

    if previous_active {
        if let Some(started) = previous_timestamp {
            accumulate_duration(&mut totals, started, now, seconds_per_unit);
        }
    }

    Ok(totals)
}

fn accumulate_duration(
    totals: &mut HashMap<String, f64>,
    mut start: NaiveDateTime,
    end: NaiveDateTime,
    seconds_per_unit: f64,
) {
    while start < end {
        let next_midnight = day_start(start.date() + Duration::days(1));
        let segment_end = end.min(next_midnight);
        let amount = (segment_end - start).num_seconds() as f64 / seconds_per_unit;
        *totals.entry(start.date().to_string()).or_default() += amount;
        start = segment_end;
    }
}

fn push_open_intervals(
    grouped: &mut BTreeMap<String, Value>,
    device_type: &str,
    mut start: NaiveDateTime,
    end: NaiveDateTime,
) {
    while start < end {
        let next_midnight = day_start(start.date() + Duration::days(1));
        let segment_end = end.min(next_midnight);
        if let Some(day) = grouped.get_mut(&start.date().to_string()) {
            day[device_type]
                .as_array_mut()
                .expect("opening array")
                .push(json!({
                    "open": start.format("%H:%M:%S").to_string(),
                    "close": segment_end.format("%H:%M:%S").to_string(),
                }));
        }
        start = segment_end;
    }
}

#[derive(Debug, Clone)]
struct ProjectFocusItem {
    name: String,
    messages: i64,
    first_prompt: String,
    last_prompt: String,
}

impl ProjectFocusItem {
    fn into_value(self) -> Value {
        json!({
            "name": self.name,
            "messages": self.messages,
            "first_prompt": self.first_prompt,
            "last_prompt": self.last_prompt,
        })
    }
}

pub(crate) fn normalize_project_name(project: &str) -> String {
    let basename = project
        .trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(project)
        .to_ascii_lowercase();
    match basename.as_str() {
        "" => "unknown".to_string(),
        "office-automation" | "claude-automate" => "office-automate".to_string(),
        "taskbar" => "deskbar".to_string(),
        "financial-analysis" | "market generator" | "fms-branch" => "fractal".to_string(),
        "claude-session-manager" => "session-manager".to_string(),
        value if value == "fractal" || value.starts_with("fractal-") => "fractal".to_string(),
        value => value.to_string(),
    }
}

#[derive(Debug, Clone)]
struct LeverageDay {
    date: String,
    prompts: i64,
    sessions: i64,
    lines_added: i64,
    lines_removed: i64,
    files_modified: i64,
    commits: i64,
    prs_merged: i64,
    prs_opened: i64,
    duration_minutes: i64,
    pr_cycle_hours_total: f64,
}

impl LeverageDay {
    fn new(date: &str) -> Self {
        Self {
            date: date.to_string(),
            prompts: 0,
            sessions: 0,
            lines_added: 0,
            lines_removed: 0,
            files_modified: 0,
            commits: 0,
            prs_merged: 0,
            prs_opened: 0,
            duration_minutes: 0,
            pr_cycle_hours_total: 0.0,
        }
    }

    fn lines_changed(&self) -> i64 {
        self.lines_added + self.lines_removed
    }

    fn into_value(self) -> Value {
        json!({
            "date": self.date,
            "prompts": self.prompts,
            "sessions": self.sessions,
            "lines_added": self.lines_added,
            "lines_removed": self.lines_removed,
            "lines_changed": self.lines_changed(),
            "files_modified": self.files_modified,
            "commits": self.commits,
            "prs_merged": self.prs_merged,
            "prs_opened": self.prs_opened,
            "avg_pr_cycle_hours": safe_ratio(self.pr_cycle_hours_total, self.prs_merged),
            "lines_per_prompt": safe_ratio(self.lines_changed() as f64, self.prompts),
            "commits_per_prompt": safe_ratio(self.commits as f64, self.prompts),
            "lines_per_session_minute": safe_ratio(self.lines_changed() as f64, self.duration_minutes),
        })
    }
}

#[derive(Debug, Default)]
struct LeverageWeek {
    prompts: i64,
    sessions: i64,
    lines_added: i64,
    lines_removed: i64,
    files_modified: i64,
    commits: i64,
    prs_merged: i64,
    prs_opened: i64,
    active_days: i64,
    duration_minutes: i64,
    pr_cycle_hours_total: f64,
}

impl LeverageWeek {
    fn add(&mut self, day: &LeverageDay) {
        self.prompts += day.prompts;
        self.sessions += day.sessions;
        self.lines_added += day.lines_added;
        self.lines_removed += day.lines_removed;
        self.files_modified += day.files_modified;
        self.commits += day.commits;
        self.prs_merged += day.prs_merged;
        self.prs_opened += day.prs_opened;
        self.duration_minutes += day.duration_minutes;
        self.pr_cycle_hours_total += day.pr_cycle_hours_total;
        if day.prompts > 0 {
            self.active_days += 1;
        }
    }

    fn lines_changed(&self) -> i64 {
        self.lines_added + self.lines_removed
    }

    fn into_value(self) -> Value {
        json!({
            "prompts": self.prompts,
            "sessions": self.sessions,
            "lines_added": self.lines_added,
            "lines_removed": self.lines_removed,
            "lines_changed": self.lines_changed(),
            "files_modified": self.files_modified,
            "commits": self.commits,
            "prs_merged": self.prs_merged,
            "prs_opened": self.prs_opened,
            "avg_pr_cycle_hours": safe_ratio(self.pr_cycle_hours_total, self.prs_merged),
            "lines_per_prompt": safe_ratio(self.lines_changed() as f64, self.prompts),
            "commits_per_prompt": safe_ratio(self.commits as f64, self.prompts),
            "lines_per_session_minute": safe_ratio(self.lines_changed() as f64, self.duration_minutes),
            "active_days": self.active_days,
        })
    }
}

fn safe_ratio(numerator: f64, denominator: i64) -> Value {
    if denominator == 0 {
        Value::Null
    } else {
        json!(round2(numerator / denominator as f64))
    }
}

fn metric_value(value: Option<f64>) -> Value {
    match value {
        None => Value::Null,
        Some(value) if value.fract() == 0.0 => json!(value as i64),
        Some(value) => json!(round2(value)),
    }
}

fn project_leverage_days(
    rows: Option<&HashMap<String, HashMap<String, f64>>>,
    metrics: &[&str],
    labels: &[String],
) -> Vec<Value> {
    labels
        .iter()
        .map(|date| {
            let mut day = serde_json::Map::new();
            day.insert("date".to_string(), json!(date));
            for metric in metrics {
                day.insert(
                    (*metric).to_string(),
                    metric_value(Some(
                        rows.and_then(|rows| rows.get(date))
                            .and_then(|row| row.get(*metric))
                            .copied()
                            .unwrap_or_default(),
                    )),
                );
            }
            Value::Object(day)
        })
        .collect()
}

fn sum_metric(days: &[Value], metric: &str) -> f64 {
    days.iter()
        .filter_map(|day| {
            day.get(metric).and_then(Value::as_f64).or_else(|| {
                day.get(metric)
                    .and_then(Value::as_i64)
                    .map(|value| value as f64)
            })
        })
        .sum()
}

fn sum_project_days(days: &[Value], metrics: &[&str]) -> Value {
    let mut object = serde_json::Map::new();
    for metric in metrics {
        object.insert(
            (*metric).to_string(),
            metric_value(Some(sum_metric(days, metric))),
        );
    }
    Value::Object(object)
}

fn window_phrase(days: i64) -> String {
    match days {
        1 => "today".to_string(),
        7 => "this week".to_string(),
        value => format!("in the last {value} days"),
    }
}

fn summarize_session_manager(week: &Value, days: i64) -> String {
    let dispatches = week["sm_dispatches"].as_i64().unwrap_or_default();
    let sends = week["sm_sends"].as_i64().unwrap_or_default();
    let telegram = week["sm_telegram_in"].as_i64().unwrap_or_default()
        + week["sm_telegram_out"].as_i64().unwrap_or_default();
    if telegram > 0 {
        format!(
            "{dispatches} dispatches, {telegram} Telegram messages {}",
            window_phrase(days)
        )
    } else {
        format!(
            "{dispatches} dispatches, {sends} sends {}",
            window_phrase(days)
        )
    }
}

fn summarize_engram(current: &Value) -> String {
    let active = current["active_concepts"].as_i64().unwrap_or_default();
    if let Some(age) = current["last_fold_age_hours"].as_f64() {
        format!("Last fold {age:.1}h ago, {active} active concepts")
    } else {
        format!("{active} active concepts, no committed fold data yet")
    }
}

fn summarize_agent_os(week: &Value, days: i64) -> String {
    format!(
        "{} persona reads across {} projects {}",
        week["persona_reads"].as_i64().unwrap_or_default(),
        week["persona_projects"].as_i64().unwrap_or_default(),
        window_phrase(days)
    )
}

fn summarize_office_automate(week: &Value, days: i64) -> String {
    format!(
        "{} automation events, {} state transitions {}",
        week["automation_events"].as_i64().unwrap_or_default(),
        week["state_transitions"].as_i64().unwrap_or_default(),
        window_phrase(days)
    )
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
    fn normalizes_legacy_and_remote_project_names() {
        assert_eq!(
            normalize_project_name("office-automation"),
            "office-automate"
        );
        assert_eq!(
            normalize_project_name("/Users/rajesh/projects/taskbar"),
            "deskbar"
        );
        assert_eq!(normalize_project_name("fractal-quant-algo"), "fractal");
        assert_eq!(normalize_project_name("fractal-algo-rust"), "fractal");
    }

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
    fn leverage_history_aggregates_session_output_metrics() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");

        let today = Local::now().naive_local().date();
        let timestamp = format_timestamp(day_start(today) + Duration::hours(10));
        {
            let connection = Connection::open(&db_path).expect("open database");
            connection
                .execute(
                    "INSERT INTO orchestration_activity (timestamp, tool, project, session_id) VALUES (?, ?, ?, ?), (?, ?, ?, ?)",
                    (
                        &timestamp,
                        "claude",
                        "office-automate",
                        "session-1",
                        &timestamp,
                        "codex",
                        "office-automate",
                        "session-2",
                    ),
                )
                .expect("insert orchestration rows");
        }

        let telemetry_path = telemetry_database_path(&db_path);
        ensure_telemetry_database(&telemetry_path).expect("telemetry schema");
        {
            let connection = Connection::open(&telemetry_path).expect("open telemetry database");
            connection
                .execute(
                    "INSERT INTO session_output \
                     (session_id, project, start_time, duration_minutes, lines_added, lines_removed, files_modified, git_commits) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?), (?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        "session-1",
                        "office-automate",
                        &timestamp,
                        30,
                        150,
                        20,
                        4,
                        5,
                        "session-2",
                        "office-automate",
                        &timestamp,
                        20,
                        50,
                        30,
                        2,
                        3,
                    ),
                )
                .expect("insert session output rows");
        }

        let payload = read_leverage_history(&db_path, 1).expect("leverage history");

        assert_eq!(payload["days"][0]["date"], today.to_string());
        assert_eq!(payload["days"][0]["prompts"], 2);
        assert_eq!(payload["days"][0]["sessions"], 2);
        assert_eq!(payload["days"][0]["lines_added"], 200);
        assert_eq!(payload["days"][0]["lines_removed"], 50);
        assert_eq!(payload["days"][0]["lines_changed"], 250);
        assert_eq!(payload["days"][0]["files_modified"], 6);
        assert_eq!(payload["days"][0]["commits"], 8);
        assert_eq!(payload["days"][0]["lines_per_prompt"], 125.0);
        assert_eq!(payload["days"][0]["commits_per_prompt"], 4.0);
        assert_eq!(payload["days"][0]["lines_per_session_minute"], 5.0);
        assert_eq!(payload["week"]["lines_added"], 200);
        assert_eq!(payload["week"]["lines_removed"], 50);
        assert_eq!(payload["week"]["files_modified"], 6);
        assert_eq!(payload["week"]["commits"], 8);
        assert_eq!(payload["week"]["lines_per_session_minute"], 5.0);
    }

    #[test]
    fn leverage_history_reads_configured_telemetry_database() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        let telemetry_path = temp_dir.path().join("custom").join("telemetry.sqlite");
        migrate_database(&db_path).expect("migration");
        ensure_telemetry_database(&telemetry_path).expect("telemetry schema");

        let today = Local::now().naive_local().date();
        let timestamp = format_timestamp(day_start(today) + Duration::hours(10));
        {
            let connection = Connection::open(&telemetry_path).expect("open telemetry database");
            connection
                .execute(
                    "INSERT INTO session_output \
                     (session_id, project, start_time, duration_minutes, lines_added, lines_removed, files_modified, git_commits) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        "session-1",
                        "office-automate",
                        &timestamp,
                        10,
                        40,
                        2,
                        1,
                        3,
                    ),
                )
                .expect("insert session output row");
        }

        let payload = read_leverage_history_with_telemetry(&db_path, &telemetry_path, 1)
            .expect("leverage history");

        assert_eq!(payload["days"][0]["lines_added"], 40);
        assert_eq!(payload["days"][0]["lines_removed"], 2);
        assert_eq!(payload["week"]["commits"], 3);
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

    #[test]
    fn inserts_qingping_sensor_readings() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");

        let reading = QingpingReading {
            device_name: "Qingping Air Monitor".to_string(),
            mac_hint: "AABBCCDDEEFF".to_string(),
            temp_c: Some(22.5),
            humidity: Some(45.0),
            co2_ppm: Some(620),
            pm25: Some(3),
            pm10: Some(4),
            tvoc: Some(25),
            noise_db: Some(37),
            timestamp: "2026-06-05T12:05:00".to_string(),
            raw_data: "{}".to_string(),
        };

        insert_sensor_reading(&db_path, &reading).expect("insert reading");

        let connection = Connection::open(&db_path).expect("open database");
        let row = connection
            .query_row(
                r#"
                SELECT timestamp, co2_ppm, temp_c, humidity, pm25, pm10, tvoc, noise_db, source
                FROM sensor_readings
                "#,
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                    ))
                },
            )
            .expect("read inserted row");

        assert_eq!(
            row,
            (
                "2026-06-05 12:05:00".to_string(),
                620,
                22.5,
                45.0,
                3,
                4,
                25,
                37,
                "qingping".to_string()
            )
        );
    }

    #[test]
    fn logs_and_reads_latest_device_state() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let db_path = temp_dir.path().join("office_climate.db");
        migrate_database(&db_path).expect("migration");

        log_device_event(
            &db_path,
            "door",
            "open",
            Some("Office Door"),
            Some(&serde_json::json!({"state": "open"})),
        )
        .expect("log open");
        log_device_event(&db_path, "door", "closed", Some("Office Door"), None)
            .expect("log closed");

        assert_eq!(
            get_latest_device_state(&db_path, "door").expect("latest"),
            Some("closed".to_string())
        );
        assert_eq!(
            get_latest_device_state(&db_path, "window").expect("missing"),
            None
        );

        let connection = Connection::open(&db_path).expect("open database");
        let details: Option<String> = connection
            .query_row(
                "SELECT details FROM device_events WHERE event = 'open'",
                [],
                |row| row.get(0),
            )
            .expect("read details");
        assert_eq!(details, Some(r#"{"state":"open"}"#.to_string()));
    }
}
