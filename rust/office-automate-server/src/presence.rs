use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::Local;
use tokio::{process::Command, time::timeout};

use crate::config::{AppConfig, PresenceConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceSnapshot {
    pub last_active_timestamp: f64,
    pub external_monitor: bool,
    pub idle_seconds: f64,
    pub display_count: usize,
    pub external_displays: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MacOsPresenceReader {
    command_timeout: Duration,
}

impl MacOsPresenceReader {
    pub fn from_config(config: &PresenceConfig) -> Self {
        Self {
            command_timeout: Duration::from_secs(config.command_timeout_seconds.max(1)),
        }
    }

    pub async fn read_snapshot(&self) -> Result<PresenceSnapshot> {
        let idle_output =
            run_command("ioreg", &["-c", "IOHIDSystem"], self.command_timeout).await?;
        let idle_sample_timestamp = unix_timestamp_now();
        let idle_seconds = parse_hid_idle_seconds(&idle_output)
            .context("ioreg output did not include HIDIdleTime")?;

        let display_output = run_command(
            "system_profiler",
            &["SPDisplaysDataType"],
            self.command_timeout,
        )
        .await?;

        build_presence_snapshot(idle_seconds, idle_sample_timestamp, &display_output)
    }
}

pub async fn smoke_presence(config: &AppConfig) -> Result<PresenceSnapshot> {
    MacOsPresenceReader::from_config(&config.presence)
        .read_snapshot()
        .await
}

async fn run_command(program: &str, args: &[&str], command_timeout: Duration) -> Result<String> {
    let output = timeout(
        command_timeout,
        Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .with_context(|| format!("{program} timed out after {command_timeout:?}"))?
    .with_context(|| format!("failed to run {program}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{program} failed with status {}: {stderr}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn parse_hid_idle_seconds(output: &str) -> Option<f64> {
    output
        .lines()
        .find(|line| line.contains("HIDIdleTime"))
        .and_then(|line| {
            line.split(|character: char| !character.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .next_back()
        })
        .and_then(|value| value.parse::<f64>().ok())
        .map(|nanoseconds| nanoseconds / 1_000_000_000.0)
}

pub fn parse_display_info(output: &str) -> (usize, Vec<String>) {
    let lines = output.lines().collect::<Vec<_>>();
    let mut all_displays = Vec::new();
    let mut external_displays = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let stripped = line.trim();
        if !is_display_heading_candidate(stripped) {
            continue;
        }

        let display_name = stripped.trim_end_matches(':').to_string();
        let mut is_display = false;
        let mut is_internal = false;

        for lookahead in lines.iter().skip(index + 1).take(15) {
            let lookahead = lookahead.trim();
            if is_display_heading_candidate(lookahead) {
                break;
            }
            if lookahead.contains("Resolution") || lookahead.contains("Display Type") {
                is_display = true;
            }
            if lookahead.contains("Built-in") || lookahead.contains("Connection Type: Internal") {
                is_internal = true;
            }
        }

        if is_display {
            all_displays.push(display_name.clone());
            if !is_internal {
                external_displays.push(display_name);
            }
        }
    }

    (all_displays.len(), external_displays)
}

fn build_presence_snapshot(
    idle_seconds: f64,
    idle_sample_timestamp: f64,
    display_output: &str,
) -> Result<PresenceSnapshot> {
    let (display_count, external_displays) = parse_display_info(display_output);

    Ok(PresenceSnapshot {
        last_active_timestamp: idle_sample_timestamp - idle_seconds,
        external_monitor: !external_displays.is_empty(),
        idle_seconds,
        display_count,
        external_displays,
    })
}

fn is_display_heading_candidate(stripped: &str) -> bool {
    const SKIP_PREFIXES: &[&str] = &[
        "Chipset",
        "Type:",
        "Bus:",
        "Vendor:",
        "Metal",
        "Total",
        "Graphics/Displays:",
        "Apple M",
        "Resolution",
        "Display Type",
        "Main Display",
        "Mirror",
        "Online",
        "Rotation",
        "Connection",
        "Automatically",
        "UI Looks",
    ];

    stripped.ends_with(':')
        && !SKIP_PREFIXES
            .iter()
            .any(|prefix| stripped.starts_with(prefix))
}

fn unix_timestamp_now() -> f64 {
    Local::now().timestamp_millis() as f64 / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hid_idle_time_nanoseconds() {
        let output = r#"
        | |   "HIDIdleTime" = 12500000000
        | |   "OtherProperty" = 1
        "#;

        assert_eq!(parse_hid_idle_seconds(output), Some(12.5));
    }

    #[test]
    fn parses_internal_and_external_displays() {
        let output = r#"
Graphics/Displays:

    Apple M2:

      Chipset Model: Apple M2

      Color LCD:
        Resolution: 3456 x 2234 Retina
        Main Display: Yes
        Connection Type: Internal

      DELL U2720Q:
        Resolution: 3840 x 2160
        Display Type: External
        Main Display: No

      LG HDR 4K:
        Resolution: 3840 x 2160
        Display Type: External
        Main Display: No
"#;

        let (display_count, external_displays) = parse_display_info(output);

        assert_eq!(display_count, 3);
        assert_eq!(external_displays, vec!["DELL U2720Q", "LG HDR 4K"]);
    }

    #[test]
    fn treats_builtin_only_display_as_no_external_monitor() {
        let output = r#"
Graphics/Displays:
    Apple M2:
      Color LCD:
        Resolution: 3456 x 2234 Retina
        Display Type: Built-in Retina LCD
"#;

        let (display_count, external_displays) = parse_display_info(output);

        assert_eq!(display_count, 1);
        assert!(external_displays.is_empty());
    }

    #[test]
    fn last_active_timestamp_uses_idle_sample_time() {
        let display_output = r#"
Graphics/Displays:
    Apple M2:
      DELL U2720Q:
        Resolution: 3840 x 2160
        Display Type: External
"#;

        let snapshot =
            build_presence_snapshot(1.0, 1000.0, display_output).expect("presence snapshot");

        assert_eq!(snapshot.last_active_timestamp, 999.0);
        assert!(snapshot.external_monitor);
    }
}
