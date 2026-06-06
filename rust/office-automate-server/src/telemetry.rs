use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    config::AppConfig,
    db::{
        self, ProjectLeverageRow, SessionOutputRow, normalize_project_name, telemetry_database_path,
    },
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local, NaiveDateTime};
use chrono_tz::America::Los_Angeles;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryCollectStats {
    pub sessions: usize,
    pub rows_written: usize,
    pub synthetic_rows: usize,
    pub matched_commits: usize,
}

#[derive(Debug, Clone)]
struct GitCommand {
    timestamp: NaiveDateTime,
    repo: String,
    bash_command: String,
}

#[derive(Debug, Clone)]
struct SessionInfo {
    session_id: String,
    session_name: String,
    project_name: String,
    start_time: NaiveDateTime,
    end_time: NaiveDateTime,
    tool_counts: BTreeMap<String, i64>,
    git_commits: Vec<GitCommand>,
    git_pushes: Vec<GitCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitStats {
    repo: String,
    commit_hash: String,
    author_date: NaiveDateTime,
    files_changed: i64,
    insertions: i64,
    deletions: i64,
}

pub fn collect_telemetry(config: &AppConfig, dry_run: bool) -> Result<TelemetryCollectStats> {
    let days = if config.telemetry.days == 0 {
        2
    } else {
        config.telemetry.days
    };
    collect_session_telemetry(
        &config.runtime.session_tool_usage_db_path,
        &config.runtime.telemetry_db_path,
        &config.telemetry.repos,
        days,
        dry_run,
        Local::now().naive_local(),
    )
}

pub fn collect_session_telemetry(
    tool_usage_db_path: &Path,
    telemetry_db_path: &Path,
    repos: &[PathBuf],
    days: u64,
    dry_run: bool,
    now: NaiveDateTime,
) -> Result<TelemetryCollectStats> {
    let cutoff = now - Duration::days(days.max(1) as i64);
    let sessions = build_session_index(tool_usage_db_path, cutoff)?;
    let commits_by_repo = collect_git_stats(repos, start_of_day(cutoff))?;

    let mut matched_hashes = HashSet::new();
    let mut unmatched_commit_commands: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut rows = Vec::new();
    for session in sessions.values() {
        let mut matched_commits = Vec::new();
        for command in &session.git_commits {
            if let Some(commit) = match_commit(command, &commits_by_repo, &matched_hashes) {
                matched_hashes.insert(commit.commit_hash.clone());
                matched_commits.push(commit);
            } else {
                let date = command.timestamp.date().to_string();
                *unmatched_commit_commands
                    .entry((command.repo.clone(), date))
                    .or_insert(0) += 1;
            }
        }
        rows.push(session_row(session, &matched_commits)?);
    }

    rows.extend(synthetic_rows(
        &commits_by_repo,
        &matched_hashes,
        &unmatched_commit_commands,
    ));

    if !dry_run {
        db::upsert_collector_session_output_rows(telemetry_db_path, &rows)?;
    }

    let synthetic_rows = rows
        .iter()
        .filter(|row| row.session_id.starts_with("unattributed-"))
        .count();
    Ok(TelemetryCollectStats {
        sessions: sessions.len(),
        rows_written: rows.len(),
        synthetic_rows,
        matched_commits: matched_hashes.len(),
    })
}

pub fn collect_project_leverage(config: &AppConfig) -> Result<usize> {
    let rows = collect_project_leverage_rows(
        &config.runtime.database_path,
        &config.runtime.tool_usage_db_path,
        &config.runtime.engram_db_path,
        &config.runtime.engram_registry_path,
        Local::now().naive_local(),
    )?;
    db::upsert_project_leverage_rows(&config.runtime.database_path, &rows)
}

fn build_session_index(
    tool_usage_db_path: &Path,
    cutoff: NaiveDateTime,
) -> Result<BTreeMap<String, SessionInfo>> {
    if !tool_usage_db_path.exists() {
        tracing::warn!(
            "tool_usage DB not found at {}; telemetry collection skipped",
            tool_usage_db_path.display()
        );
        return Ok(BTreeMap::new());
    }

    let connection = Connection::open(tool_usage_db_path).with_context(|| {
        format!(
            "failed to open tool_usage SQLite database {}",
            tool_usage_db_path.display()
        )
    })?;
    if !table_exists(&connection, "tool_usage")? {
        tracing::warn!(
            "tool_usage table missing in {}; telemetry collection skipped",
            tool_usage_db_path.display()
        );
        return Ok(BTreeMap::new());
    }

    let mut statement = connection.prepare(
        "SELECT session_id, session_name, project_name, tool_name, target_file, bash_command, timestamp, cwd
         FROM tool_usage
         WHERE hook_type = 'PreToolUse' AND timestamp >= ?
         ORDER BY session_id, timestamp",
    )?;
    let cutoff = format_timestamp(cutoff);
    let rows = statement.query_map(params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;

    let mut sessions = BTreeMap::new();
    for row in rows {
        let (
            session_id,
            session_name,
            project_name,
            tool_name,
            _target_file,
            bash_command,
            timestamp,
            cwd,
        ) = row?;
        let timestamp = parse_datetime(&timestamp)?;
        let session_name = session_name.unwrap_or_else(|| session_id.clone());
        let project_name = normalize_project_name(
            project_name
                .as_deref()
                .or(cwd.as_deref())
                .unwrap_or("unknown"),
        );
        let session = sessions
            .entry(session_id.clone())
            .or_insert_with(|| SessionInfo {
                session_id: session_id.clone(),
                session_name,
                project_name: project_name.clone(),
                start_time: timestamp,
                end_time: timestamp,
                tool_counts: BTreeMap::new(),
                git_commits: Vec::new(),
                git_pushes: Vec::new(),
            });
        session.start_time = session.start_time.min(timestamp);
        session.end_time = session.end_time.max(timestamp);

        let tool_name = tool_name.unwrap_or_else(|| "unknown".to_string());
        *session.tool_counts.entry(tool_name.clone()).or_insert(0) += 1;
        let bash_command = bash_command.unwrap_or_default().trim().to_string();
        let repo = normalize_project_name(cwd.as_deref().unwrap_or(&session.project_name));
        if tool_name == "Bash" && bash_command.starts_with("git commit") {
            session.git_commits.push(GitCommand {
                timestamp,
                repo,
                bash_command,
            });
        } else if tool_name == "Bash" && bash_command.starts_with("git push") {
            session.git_pushes.push(GitCommand {
                timestamp,
                repo,
                bash_command,
            });
        }
    }

    Ok(sessions)
}

fn collect_git_stats(
    repos: &[PathBuf],
    cutoff: NaiveDateTime,
) -> Result<HashMap<String, Vec<CommitStats>>> {
    let mut commits_by_repo = HashMap::new();
    let cutoff_arg = format_timestamp(cutoff);

    for repo in repos {
        if !repo.exists() {
            tracing::warn!("skipping missing telemetry repo {}", repo.display());
            continue;
        }
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "log",
                "--all",
                "--no-merges",
                "--format=COMMIT:%H|%aI|%s",
                "--shortstat",
            ])
            .arg(format!("--after={cutoff_arg}"))
            .output()
            .with_context(|| format!("failed to run git log for {}", repo.display()))?;
        if !output.status.success() {
            bail!(
                "git log failed for {}: {}",
                repo.display(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let repo_name = normalize_project_name(
            repo.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown"),
        );
        let mut commits = Vec::new();
        let mut current: Option<CommitStats> = None;
        for raw_line in String::from_utf8_lossy(&output.stdout).lines() {
            let line = raw_line.trim_end();
            if let Some(rest) = line.strip_prefix("COMMIT:") {
                if let Some(commit) = current.take() {
                    commits.push(commit);
                }
                let mut pieces = rest.splitn(3, '|');
                let Some(commit_hash) = pieces.next() else {
                    continue;
                };
                let Some(author_date) = pieces.next() else {
                    continue;
                };
                current = Some(CommitStats {
                    repo: repo_name.clone(),
                    commit_hash: commit_hash.to_string(),
                    author_date: parse_datetime(author_date)?,
                    files_changed: 0,
                    insertions: 0,
                    deletions: 0,
                });
                continue;
            }

            if let Some(current) = &mut current {
                if let Some((files_changed, insertions, deletions)) = parse_shortstat(line) {
                    current.files_changed = files_changed;
                    current.insertions = insertions;
                    current.deletions = deletions;
                }
            }
        }
        if let Some(commit) = current {
            commits.push(commit);
        }
        commits_by_repo.insert(repo_name, commits);
    }

    Ok(commits_by_repo)
}

fn match_commit(
    command: &GitCommand,
    commits_by_repo: &HashMap<String, Vec<CommitStats>>,
    matched_hashes: &HashSet<String>,
) -> Option<CommitStats> {
    commits_by_repo
        .get(&command.repo)?
        .iter()
        .filter(|commit| !matched_hashes.contains(&commit.commit_hash))
        .filter_map(|commit| {
            let delta = (commit.author_date - command.timestamp)
                .num_seconds()
                .unsigned_abs();
            (delta < 60).then_some((delta, commit))
        })
        .min_by_key(|(delta, _)| *delta)
        .map(|(_, commit)| commit.clone())
}

fn session_row(session: &SessionInfo, matched_commits: &[CommitStats]) -> Result<SessionOutputRow> {
    let duration_minutes = ((session.end_time - session.start_time).num_seconds() / 60).max(0);
    let tool_counts = serde_json::to_string(&session.tool_counts)?;
    Ok(SessionOutputRow {
        session_id: session.session_id.clone(),
        project: session.project_name.clone(),
        start_time: format_timestamp(session.start_time),
        duration_minutes,
        lines_added: matched_commits.iter().map(|commit| commit.insertions).sum(),
        lines_removed: matched_commits.iter().map(|commit| commit.deletions).sum(),
        files_modified: matched_commits
            .iter()
            .map(|commit| commit.files_changed)
            .sum(),
        git_commits: session.git_commits.len() as i64,
        git_pushes: session
            .git_pushes
            .iter()
            .filter(|push| !push.bash_command.contains("--delete"))
            .count() as i64,
        user_message_count: 0,
        assistant_message_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        tool_counts: Some(tool_counts),
        languages: None,
        is_human_session: is_human_session(&session.session_name, &session.session_id),
    })
}

fn synthetic_rows(
    commits_by_repo: &HashMap<String, Vec<CommitStats>>,
    matched_hashes: &HashSet<String>,
    unmatched_commit_commands: &BTreeMap<(String, String), usize>,
) -> Vec<SessionOutputRow> {
    let mut grouped: BTreeMap<(String, String), Vec<CommitStats>> = BTreeMap::new();
    for (repo, commits) in commits_by_repo {
        for commit in commits {
            if matched_hashes.contains(&commit.commit_hash) {
                continue;
            }
            let date = commit.author_date.date().to_string();
            grouped
                .entry((repo.clone(), date))
                .or_default()
                .push(commit.clone());
        }
    }

    grouped
        .into_iter()
        .map(|((repo, date), commits)| {
            let earliest = commits
                .iter()
                .map(|commit| commit.author_date)
                .min()
                .expect("synthetic group has commits");
            let command_claims = unmatched_commit_commands
                .get(&(repo.clone(), date.clone()))
                .copied()
                .unwrap_or_default();
            let git_commits = commits.len().saturating_sub(command_claims) as i64;
            SessionOutputRow {
                session_id: format!("unattributed-{repo}-{date}"),
                project: repo,
                start_time: format_timestamp(earliest),
                duration_minutes: 0,
                lines_added: commits.iter().map(|commit| commit.insertions).sum(),
                lines_removed: commits.iter().map(|commit| commit.deletions).sum(),
                files_modified: commits.iter().map(|commit| commit.files_changed).sum(),
                git_commits,
                git_pushes: 0,
                user_message_count: 0,
                assistant_message_count: 0,
                input_tokens: 0,
                output_tokens: 0,
                tool_counts: None,
                languages: None,
                is_human_session: false,
            }
        })
        .collect()
}

pub fn collect_project_leverage_rows(
    database_path: &Path,
    tool_usage_db_path: &Path,
    engram_db_path: &Path,
    engram_registry_path: &Path,
    now: NaiveDateTime,
) -> Result<Vec<ProjectLeverageRow>> {
    let mut rows = Vec::new();
    rows.extend(collect_tool_usage_metrics(tool_usage_db_path)?);
    rows.extend(collect_engram_metrics(
        engram_db_path,
        engram_registry_path,
        now,
    )?);
    rows.extend(collect_office_automation_metrics(database_path)?);
    Ok(rows)
}

fn collect_tool_usage_metrics(tool_usage_db_path: &Path) -> Result<Vec<ProjectLeverageRow>> {
    if !tool_usage_db_path.exists() {
        tracing::info!(
            "skipping project leverage tool usage; DB not found at {}",
            tool_usage_db_path.display()
        );
        return Ok(Vec::new());
    }
    let connection = Connection::open(tool_usage_db_path).with_context(|| {
        format!(
            "failed to open tool_usage SQLite database {}",
            tool_usage_db_path.display()
        )
    })?;
    if !table_exists(&connection, "tool_usage")? {
        return Ok(Vec::new());
    }

    let mut rows = Vec::new();
    collect_sm_metrics(&connection, &mut rows)?;
    collect_persona_metrics(&connection, &mut rows)?;
    collect_telegram_metrics(&connection, &mut rows)?;
    Ok(rows)
}

fn collect_sm_metrics(connection: &Connection, rows: &mut Vec<ProjectLeverageRow>) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date,
                SUM(CASE WHEN bash_command LIKE 'sm send%' THEN 1 ELSE 0 END) AS sm_sends,
                SUM(CASE WHEN bash_command LIKE 'sm dispatch%' THEN 1 ELSE 0 END) AS sm_dispatches,
                SUM(CASE WHEN bash_command LIKE 'sm remind%' THEN 1 ELSE 0 END) AS sm_reminds
         FROM tool_usage
         WHERE tool_name = 'Bash'
           AND hook_type = 'PreToolUse'
           AND bash_command LIKE 'sm %'
         GROUP BY date(timestamp)",
    )?;
    let metric_rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or_default(),
            row.get::<_, Option<f64>>(2)?.unwrap_or_default(),
            row.get::<_, Option<f64>>(3)?.unwrap_or_default(),
        ))
    })?;
    for row in metric_rows {
        let (date, sends, dispatches, reminds) = row?;
        rows.push(project_row(&date, "session-manager", "sm_sends", sends));
        rows.push(project_row(
            &date,
            "session-manager",
            "sm_dispatches",
            dispatches,
        ));
        rows.push(project_row(&date, "session-manager", "sm_reminds", reminds));
    }

    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date, COUNT(DISTINCT session_id) AS active_sessions
         FROM tool_usage
         WHERE hook_type = 'PreToolUse'
         GROUP BY date(timestamp)",
    )?;
    let metric_rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or_default(),
        ))
    })?;
    for row in metric_rows {
        let (date, active_sessions) = row?;
        rows.push(project_row(
            &date,
            "session-manager",
            "sm_active_sessions",
            active_sessions,
        ));
    }

    Ok(())
}

fn collect_persona_metrics(
    connection: &Connection,
    rows: &mut Vec<ProjectLeverageRow>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date, COUNT(*) AS persona_reads
         FROM tool_usage
         WHERE tool_name = 'Read'
           AND target_file LIKE '%agent-os/personas/%'
         GROUP BY date(timestamp)",
    )?;
    let metric_rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or_default(),
        ))
    })?;
    for row in metric_rows {
        let (date, persona_reads) = row?;
        rows.push(project_row(
            &date,
            "agent-os",
            "persona_reads",
            persona_reads,
        ));
    }

    let mut projects_by_date: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date, COALESCE(NULLIF(project_name, ''), 'unknown') AS project
         FROM tool_usage
         WHERE tool_name = 'Read'
           AND target_file LIKE '%agent-os/personas/%'
         ORDER BY date(timestamp), project",
    )?;
    let metric_rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in metric_rows {
        let (date, project) = row?;
        projects_by_date
            .entry(date)
            .or_default()
            .insert(normalize_project_name(&project));
    }

    for (date, projects) in projects_by_date {
        rows.push(project_row(
            &date,
            "agent-os",
            "persona_projects",
            projects.len() as f64,
        ));
        for project in projects {
            rows.push(project_row(
                &date,
                "agent-os",
                &format!("persona_project::{project}"),
                1.0,
            ));
        }
    }

    Ok(())
}

fn collect_telegram_metrics(
    connection: &Connection,
    rows: &mut Vec<ProjectLeverageRow>,
) -> Result<()> {
    if !table_exists(connection, "telegram_telemetry")? {
        return Ok(());
    }
    let mut statement = connection.prepare(
        "SELECT date(timestamp) AS date,
                SUM(CASE WHEN direction = 'in' THEN 1 ELSE 0 END) AS telegram_in,
                SUM(CASE WHEN direction = 'out' THEN 1 ELSE 0 END) AS telegram_out
         FROM telegram_telemetry
         GROUP BY date(timestamp)",
    )?;
    let metric_rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or_default(),
            row.get::<_, Option<f64>>(2)?.unwrap_or_default(),
        ))
    })?;
    for row in metric_rows {
        let (date, telegram_in, telegram_out) = row?;
        rows.push(project_row(
            &date,
            "session-manager",
            "sm_telegram_in",
            telegram_in,
        ));
        rows.push(project_row(
            &date,
            "session-manager",
            "sm_telegram_out",
            telegram_out,
        ));
    }
    Ok(())
}

fn collect_engram_metrics(
    engram_db_path: &Path,
    engram_registry_path: &Path,
    now: NaiveDateTime,
) -> Result<Vec<ProjectLeverageRow>> {
    if !engram_db_path.exists() {
        return Ok(Vec::new());
    }
    let connection = Connection::open(engram_db_path).with_context(|| {
        format!(
            "failed to open engram SQLite database {}",
            engram_db_path.display()
        )
    })?;
    if !table_exists(&connection, "dispatches")? {
        return Ok(Vec::new());
    }

    let mut statement = connection.prepare(
        "SELECT created_at FROM dispatches WHERE state = 'committed' ORDER BY created_at DESC",
    )?;
    let dispatch_rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut committed = Vec::new();
    for row in dispatch_rows {
        committed.push(parse_datetime(&row?)?);
    }

    let date = now.date().to_string();
    let mut rows = Vec::new();
    let folds_7d = committed
        .iter()
        .filter(|timestamp| **timestamp >= now - Duration::days(7))
        .count() as f64;
    rows.push(project_row(&date, "engram", "engram_folds_7d", folds_7d));
    rows.push(project_row(
        &date,
        "engram",
        "engram_active_concepts",
        count_active_concepts(engram_registry_path)? as f64,
    ));
    if let Some(last_fold) = committed.into_iter().max() {
        rows.push(project_row(
            &date,
            "engram",
            "engram_last_fold_age_hours",
            (now - last_fold).num_seconds() as f64 / 3600.0,
        ));
    }
    Ok(rows)
}

fn collect_office_automation_metrics(database_path: &Path) -> Result<Vec<ProjectLeverageRow>> {
    if !database_path.exists() {
        return Ok(Vec::new());
    }
    let connection = Connection::open(database_path)
        .with_context(|| format!("failed to open SQLite database {}", database_path.display()))?;
    let mut rows = Vec::new();

    for (table, metric) in [
        ("climate_actions", "automation_events"),
        ("occupancy_log", "state_transitions"),
    ] {
        if !table_exists(&connection, table)? {
            continue;
        }
        let mut statement = connection.prepare(&format!(
            "SELECT date(timestamp) AS date, COUNT(*) AS count FROM {table} GROUP BY date(timestamp)"
        ))?;
        let metric_rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<f64>>(1)?.unwrap_or_default(),
            ))
        })?;
        for row in metric_rows {
            let (date, count) = row?;
            rows.push(project_row(&date, "office-automate", metric, count));
        }
    }
    Ok(rows)
}

fn count_active_concepts(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read engram registry {}", path.display()))?;
    Ok(contents
        .lines()
        .filter(|line| {
            let line = line.trim();
            line.starts_with("## C") && line.to_ascii_uppercase().contains("(ACTIVE")
        })
        .count())
}

fn project_row(date: &str, project: &str, metric: &str, value: f64) -> ProjectLeverageRow {
    ProjectLeverageRow {
        date: date.to_string(),
        project: project.to_string(),
        metric: metric.to_string(),
        value,
    }
}

fn table_exists(connection: &Connection, table_name: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
            params![table_name],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .context("failed to inspect SQLite schema")
}

fn parse_shortstat(line: &str) -> Option<(i64, i64, i64)> {
    let mut files = None;
    let mut insertions = 0;
    let mut deletions = 0;
    for segment in line.trim().split(',') {
        let segment = segment.trim();
        if segment.contains("file") && segment.contains("changed") {
            files = first_i64(segment);
        } else if segment.contains("insertion") {
            insertions = first_i64(segment).unwrap_or_default();
        } else if segment.contains("deletion") {
            deletions = first_i64(segment).unwrap_or_default();
        }
    }
    files.map(|files| (files, insertions, deletions))
}

fn first_i64(value: &str) -> Option<i64> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse().ok())
}

fn parse_datetime(value: &str) -> Result<NaiveDateTime> {
    let normalized = value.trim();
    let rfc3339 = if let Some(prefix) = normalized.strip_suffix('Z') {
        format!("{prefix}+00:00")
    } else {
        normalized.to_string()
    };
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(&rfc3339) {
        return Ok(timestamp.with_timezone(&Los_Angeles).naive_local());
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(timestamp) = NaiveDateTime::parse_from_str(normalized, format) {
            return Ok(timestamp);
        }
    }
    bail!("unsupported timestamp {value:?}")
}

fn format_timestamp(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn start_of_day(value: NaiveDateTime) -> NaiveDateTime {
    value
        .date()
        .and_hms_opt(0, 0, 0)
        .expect("midnight should be a valid local naive time")
}

fn is_human_session(session_name: &str, session_id: &str) -> bool {
    if session_name.starts_with("claude-")
        && session_name
            .trim_start_matches("claude-")
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return true;
    }
    session_name.is_empty() || session_name == session_id
}

#[allow(dead_code)]
pub fn default_telemetry_database_path(config: &AppConfig) -> PathBuf {
    telemetry_database_path(&config.runtime.database_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_tool_usage_db(path: &Path) {
        let connection = Connection::open(path).expect("open tool DB");
        connection
            .execute_batch(
                "CREATE TABLE tool_usage (
                    session_id TEXT,
                    session_name TEXT,
                    project_name TEXT,
                    tool_name TEXT,
                    target_file TEXT,
                    bash_command TEXT,
                    timestamp TEXT,
                    cwd TEXT,
                    hook_type TEXT
                );",
            )
            .expect("schema");
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_tool_usage(
        path: &Path,
        session_id: &str,
        session_name: &str,
        project_name: &str,
        tool_name: &str,
        target_file: Option<&str>,
        bash_command: Option<&str>,
        timestamp: &str,
        cwd: &Path,
        hook_type: &str,
    ) {
        let connection = Connection::open(path).expect("open tool DB");
        connection
            .execute(
                "INSERT INTO tool_usage (
                    session_id, session_name, project_name, tool_name, target_file,
                    bash_command, timestamp, cwd, hook_type
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    session_id,
                    session_name,
                    project_name,
                    tool_name,
                    target_file,
                    bash_command,
                    timestamp,
                    cwd.display().to_string(),
                    hook_type,
                ],
            )
            .expect("insert tool usage");
    }

    fn run_git(repo: &Path, args: &[&str], env_date: Option<&str>) {
        let mut command = Command::new("git");
        command.arg("-C").arg(repo).args(args);
        if let Some(date) = env_date {
            command.env("GIT_AUTHOR_DATE", date);
            command.env("GIT_COMMITTER_DATE", date);
        }
        let output = command.output().expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let repo = temp_dir.path().join("office-automate");
        fs::create_dir(&repo).expect("repo dir");
        run_git(&repo, &["init"], None);
        run_git(&repo, &["config", "user.name", "Telemetry Test"], None);
        run_git(
            &repo,
            &["config", "user.email", "telemetry@example.com"],
            None,
        );
        (temp_dir, repo)
    }

    #[test]
    fn collect_telemetry_attributes_git_stats_and_synthetic_rows() {
        let (_temp_dir, repo) = init_repo();
        let tracked = repo.join("tracked.py");
        fs::write(&tracked, "print('v1')\n").expect("write tracked");
        run_git(&repo, &["add", "tracked.py"], None);
        run_git(
            &repo,
            &["commit", "-m", "tracked"],
            Some("2026-03-27T09:00:10-07:00"),
        );
        fs::write(&tracked, "print('v1')\nprint('v2')\n").expect("write tracked");
        run_git(&repo, &["add", "tracked.py"], None);
        run_git(
            &repo,
            &["commit", "-m", "manual"],
            Some("2026-03-27T11:15:00-07:00"),
        );

        let tool_db = repo.parent().expect("parent").join("tool_usage.db");
        let telemetry_db = repo.parent().expect("parent").join("telemetry.db");
        create_tool_usage_db(&tool_db);
        insert_tool_usage(
            &tool_db,
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Read",
            None,
            None,
            "2026-03-27 08:55:00",
            &repo,
            "PreToolUse",
        );
        insert_tool_usage(
            &tool_db,
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            Some("git commit -m tracked"),
            "2026-03-27 09:00:00",
            &repo,
            "PreToolUse",
        );
        insert_tool_usage(
            &tool_db,
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            Some("git push origin feature/test"),
            "2026-03-27 09:01:00",
            &repo,
            "PreToolUse",
        );
        insert_tool_usage(
            &tool_db,
            "session-1",
            "claude-a1b2c3d4",
            "office-automate",
            "Bash",
            None,
            Some("git commit -m manual"),
            "2026-03-27 11:00:00",
            &repo,
            "PreToolUse",
        );

        let stats = collect_session_telemetry(
            &tool_db,
            &telemetry_db,
            std::slice::from_ref(&repo),
            30,
            false,
            parse_datetime("2026-03-28 12:00:00").expect("now"),
        )
        .expect("collect");

        assert_eq!(
            stats,
            TelemetryCollectStats {
                sessions: 1,
                rows_written: 2,
                synthetic_rows: 1,
                matched_commits: 1,
            }
        );

        let connection = Connection::open(&telemetry_db).expect("open telemetry");
        let mut statement = connection
            .prepare(
                "SELECT session_id, lines_added, files_modified, git_commits, git_pushes, tool_counts, is_human_session
                 FROM session_output ORDER BY session_id",
            )
            .expect("query");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .expect("rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect rows");

        assert_eq!(rows[0].0, "session-1");
        assert_eq!(rows[0].1, 1);
        assert_eq!(rows[0].2, 1);
        assert_eq!(rows[0].3, 2);
        assert_eq!(rows[0].4, 1);
        assert_eq!(rows[0].5.as_deref(), Some(r#"{"Bash":3,"Read":1}"#));
        assert_eq!(rows[0].6, 1);
        assert!(rows[1].0.starts_with("unattributed-office-automate-"));
        assert_eq!(rows[1].1, 1);
        assert_eq!(rows[1].3, 0);
        assert_eq!(rows[1].6, 0);
    }

    #[test]
    fn collect_telemetry_dry_run_does_not_create_output_database() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let tool_db = temp_dir.path().join("tool_usage.db");
        let telemetry_db = temp_dir.path().join("missing").join("telemetry.db");
        create_tool_usage_db(&tool_db);

        let stats = collect_session_telemetry(
            &tool_db,
            &telemetry_db,
            &[],
            30,
            true,
            parse_datetime("2026-03-28 12:00:00").expect("now"),
        )
        .expect("collect");

        assert_eq!(
            stats,
            TelemetryCollectStats {
                sessions: 0,
                rows_written: 0,
                synthetic_rows: 0,
                matched_commits: 0,
            }
        );
        assert!(!telemetry_db.exists());
        assert!(!telemetry_db.parent().expect("parent").exists());
    }

    #[test]
    fn synthetic_rows_keep_full_cutoff_day_across_repeated_runs() {
        let (_temp_dir, repo) = init_repo();
        let tracked = repo.join("tracked.py");
        fs::write(&tracked, "print('morning')\n").expect("write tracked");
        run_git(&repo, &["add", "tracked.py"], None);
        run_git(
            &repo,
            &["commit", "-m", "morning"],
            Some("2026-03-27T09:00:00-07:00"),
        );
        fs::write(&tracked, "print('morning')\nprint('evening')\n").expect("write tracked");
        run_git(&repo, &["add", "tracked.py"], None);
        run_git(
            &repo,
            &["commit", "-m", "evening"],
            Some("2026-03-27T20:00:00-07:00"),
        );

        let tool_db = repo.parent().expect("parent").join("tool_usage.db");
        let telemetry_db = repo.parent().expect("parent").join("telemetry.db");
        create_tool_usage_db(&tool_db);

        collect_session_telemetry(
            &tool_db,
            &telemetry_db,
            std::slice::from_ref(&repo),
            1,
            false,
            parse_datetime("2026-03-28 00:05:00").expect("now"),
        )
        .expect("initial collect");
        collect_session_telemetry(
            &tool_db,
            &telemetry_db,
            std::slice::from_ref(&repo),
            1,
            false,
            parse_datetime("2026-03-28 18:00:00").expect("now"),
        )
        .expect("second collect");

        let connection = Connection::open(&telemetry_db).expect("open telemetry");
        let (git_commits, lines_added): (i64, i64) = connection
            .query_row(
                "SELECT git_commits, lines_added
                 FROM session_output
                 WHERE session_id = 'unattributed-office-automate-2026-03-27'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("synthetic row");

        assert_eq!(git_commits, 2);
        assert_eq!(lines_added, 2);
    }

    #[test]
    fn collect_project_leverage_rows_from_fake_databases() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let office_db = temp_dir.path().join("office.db");
        let tool_db = temp_dir.path().join("tool_usage.db");
        let engram_db = temp_dir.path().join("engram_state.db");
        let registry = temp_dir.path().join("engram_concept_registry.md");
        let today = Local::now().naive_local().date().to_string();
        let sm_timestamp = format!("{today} 09:00:00");
        let persona_timestamp = format!("{today} 10:00:00");
        let climate_timestamp = format!("{today} 11:00:00");
        let occupancy_timestamp = format!("{today} 12:00:00");
        db::migrate_database(&office_db).expect("office schema");
        create_tool_usage_db(&tool_db);

        for index in 0..5 {
            insert_tool_usage(
                &tool_db,
                &format!("s{index}"),
                &format!("s{index}"),
                "office-automate",
                "Bash",
                None,
                Some(&format!("sm send agent-{index} hello")),
                &sm_timestamp,
                temp_dir.path(),
                "PreToolUse",
            );
        }
        for project in [
            "fractal-market-simulator",
            "fractal-1808-em",
            "office-automate",
        ] {
            insert_tool_usage(
                &tool_db,
                project,
                project,
                project,
                "Read",
                Some("/Users/rajesh/.agent-os/personas/engineer.md"),
                None,
                &persona_timestamp,
                temp_dir.path(),
                "PreToolUse",
            );
        }

        {
            let connection = Connection::open(&engram_db).expect("engram DB");
            connection
                .execute_batch("CREATE TABLE dispatches (created_at TEXT, state TEXT);")
                .expect("engram rows");
            connection
                .execute(
                    "INSERT INTO dispatches (created_at, state) VALUES (?, 'committed')",
                    params![persona_timestamp],
                )
                .expect("engram dispatch");
        }
        fs::write(
            &registry,
            "## C001: One (ACTIVE)\n## C002: Two (DEAD)\n## C003: Three (ACTIVE)\n",
        )
        .expect("registry");
        {
            let connection = Connection::open(&office_db).expect("office DB");
            connection
                .execute(
                    "INSERT INTO climate_actions (timestamp, system, action) VALUES (?, ?, ?)",
                    params![climate_timestamp, "erv", "turbo"],
                )
                .expect("climate");
            connection
                .execute(
                    "INSERT INTO occupancy_log (timestamp, state) VALUES (?, ?)",
                    params![occupancy_timestamp, "present"],
                )
                .expect("occupancy");
        }

        let rows = collect_project_leverage_rows(
            &office_db,
            &tool_db,
            &engram_db,
            &registry,
            parse_datetime(&occupancy_timestamp).expect("now"),
        )
        .expect("collect rows");
        db::upsert_project_leverage_rows(&office_db, &rows).expect("upsert");

        let payload = db::read_project_leverage(&office_db, 1).expect("payload");
        assert_eq!(
            payload["projects"]["session-manager"]["week"]["sm_sends"],
            5.0
        );
        assert_eq!(
            payload["projects"]["agent-os"]["week"]["persona_projects"],
            2.0
        );
        assert_eq!(
            payload["projects"]["engram"]["current"]["active_concepts"],
            2.0
        );
        assert_eq!(
            payload["projects"]["office-automate"]["week"]["automation_events"],
            1.0
        );
    }

    #[test]
    fn parses_git_shortstat_variants() {
        assert_eq!(
            parse_shortstat(" 2 files changed, 12 insertions(+), 3 deletions(-)"),
            Some((2, 12, 3))
        );
        assert_eq!(
            parse_shortstat(" 1 file changed, 1 insertion(+)"),
            Some((1, 1, 0))
        );
    }

    #[test]
    fn parses_offset_timestamps_as_pacific_database_time() {
        assert_eq!(
            parse_datetime("2026-03-27T16:00:10Z").expect("timestamp"),
            parse_datetime("2026-03-27 09:00:10").expect("pacific timestamp")
        );
        assert_eq!(
            parse_datetime("2026-01-15T18:30:00Z").expect("timestamp"),
            parse_datetime("2026-01-15 10:30:00").expect("pacific timestamp")
        );
    }
}
