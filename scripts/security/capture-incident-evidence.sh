#!/usr/bin/env bash
set -euo pipefail

output_root="${OFFICE_AUTOMATE_INCIDENT_EVIDENCE_DIR:-tmp/incident-evidence}"
config_path="${OFFICE_AUTOMATE_CONFIG:-config.yaml}"
database_path="${OFFICE_AUTOMATE_DATABASE:-data/office_climate.db}"
server_bin="${OFFICE_AUTOMATE_SERVER_BIN:-target/release/office-automate-server}"
cloudflared_config="${CLOUDFLARED_CONFIG:-}"
log_lines="${OFFICE_AUTOMATE_INCIDENT_LOG_LINES:-2000}"
allow_unencrypted_local=false

usage() {
  cat <<'USAGE'
Usage: capture-incident-evidence.sh [options]

Captures local Office Automate incident evidence into a mode-0700 directory.
Raw output may contain secrets. Keep it local, encrypted, and sanitized before
sharing.

Options:
  --output-dir DIR             Evidence root. Defaults to tmp/incident-evidence
                               or OFFICE_AUTOMATE_INCIDENT_EVIDENCE_DIR.
  --config PATH                Controller config. Defaults to config.yaml or
                               OFFICE_AUTOMATE_CONFIG.
  --database PATH              Climate SQLite DB. Defaults to data/office_climate.db
                               or OFFICE_AUTOMATE_DATABASE.
  --server-bin PATH            Rust server binary to hash. Defaults to
                               target/release/office-automate-server.
  --cloudflared-config PATH    Cloudflared config to preserve and hash.
  --log-lines N                Tail line count per log. Defaults to 2000.
  --allow-unencrypted-local    Allow capture when FileVault state is unknown/off.
  -h, --help                   Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir)
      output_root="${2:-}"
      shift 2
      ;;
    --config)
      config_path="${2:-}"
      shift 2
      ;;
    --database)
      database_path="${2:-}"
      shift 2
      ;;
    --server-bin)
      server_bin="${2:-}"
      shift 2
      ;;
    --cloudflared-config)
      cloudflared_config="${2:-}"
      shift 2
      ;;
    --log-lines)
      log_lines="${2:-}"
      shift 2
      ;;
    --allow-unencrypted-local)
      allow_unencrypted_local=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$output_root" ]]; then
  echo "output directory must not be empty" >&2
  exit 2
fi
if [[ ! "$log_lines" =~ ^[0-9]+$ ]] || [[ "$log_lines" -lt 1 ]]; then
  echo "--log-lines must be a positive integer" >&2
  exit 2
fi

if [[ "$allow_unencrypted_local" != true ]]; then
  if ! command -v fdesetup >/dev/null 2>&1 || ! fdesetup status 2>/dev/null | grep -q "FileVault is On"; then
    cat >&2 <<'MSG'
FileVault is not reported as enabled.
Raw incident evidence may contain secrets. Use an encrypted evidence directory
or re-run with --allow-unencrypted-local only for a local, access-controlled
machine you accept as evidence storage.
MSG
    exit 1
  fi
fi

timestamp="$(date -u +"%Y%m%dT%H%M%SZ")"
evidence_dir="$output_root/$timestamp"
if [[ ! -d "$output_root" ]]; then
  mkdir -p "$output_root"
  chmod 700 "$output_root"
fi
mkdir -p "$evidence_dir"/{commands,hashes,logs,plists,redacted,sqlite}
chmod 700 "$evidence_dir"

capture() {
  local output="$1"
  shift
  {
    printf '$'
    printf ' %q' "$@"
    printf '\n\n'
    "$@"
  } >"$evidence_dir/$output" 2>&1 || true
  chmod 600 "$evidence_dir/$output"
}

capture_shell() {
  local output="$1"
  local command_text="$2"
  {
    printf '$ %s\n\n' "$command_text"
    bash -c "$command_text"
  } >"$evidence_dir/$output" 2>&1 || true
  chmod 600 "$evidence_dir/$output"
}

redact_file() {
  local source="$1"
  local dest="$2"
  if [[ ! -f "$source" ]]; then
    return
  fi
  sed -E \
    -e 's/([Aa]uthorization[[:space:]]*[:=][[:space:]]*)[^[:space:]]+([[:space:]][^[:space:]]+)?/\1<redacted>/g' \
    -e 's/(Bearer )[A-Za-z0-9._~+\/=-]+/\1<redacted>/Ig' \
    -e "s/((secret|token|password|key|credential|cookie|client_secret|jwt)[A-Za-z0-9_.-]*[[:space:]]*[:=][[:space:]]*)(\"[^\"]*\"|[^[:space:]]+)/\1<redacted>/Ig" \
    -e 's/(cf-access-[A-Za-z0-9_-]+:[[:space:]]*)[A-Za-z0-9._~+\/=-]+/\1<redacted>/Ig' \
    "$source" >"$dest" || true
  chmod 600 "$dest"
}

copy_if_file() {
  local source="$1"
  local dest_dir="$2"
  local dest_name="${3:-$(basename "$source")}"
  if [[ -f "$source" ]]; then
    cp "$source" "$dest_dir/$dest_name"
    chmod 600 "$dest_dir/$dest_name"
  fi
}

echo "Office Automate incident evidence"
echo "evidence_dir=$evidence_dir"
echo "raw evidence may contain secrets; do not paste it into tickets, chats, PRs, or issue comments"

capture commands/date.txt date
capture commands/host.txt hostname
capture commands/uname.txt uname -a
capture commands/git-head.txt git rev-parse HEAD
capture commands/git-status.txt git status --short --branch
capture commands/git-log.txt git log --oneline -20

capture commands/listeners.txt lsof -nP -iTCP -sTCP:LISTEN
capture commands/cloudflared-version.txt cloudflared --version
capture_shell hashes/cloudflared.sha256 'command -v cloudflared >/dev/null 2>&1 && shasum -a 256 "$(command -v cloudflared)"'

capture commands/launchctl-server.txt launchctl print "gui/$(id -u)/com.office-automate.server"
capture commands/launchctl-telemetry.txt launchctl print "gui/$(id -u)/com.office-automate.telemetry"
capture commands/launchctl-project-leverage.txt launchctl print "gui/$(id -u)/com.office-automate.project-leverage"
capture commands/launchctl-edge.txt sudo -n launchctl print system/com.office-automate.edge
capture commands/launchctl-tunnel.txt sudo -n launchctl print system/com.office-automate.tunnel
capture commands/launchctl-gui-tunnel.txt launchctl print "gui/$(id -u)/com.office-automate.tunnel"

capture_shell commands/ps-eww.raw.secret.txt 'pids="$(pgrep -f "office-automate-server|cloudflared" | tr "\n" " ")"; if [[ -n "$pids" ]]; then for pid in $pids; do ps eww -p "$pid"; done; fi'
redact_file "$evidence_dir/commands/ps-eww.raw.secret.txt" "$evidence_dir/redacted/ps-eww.redacted.txt"

capture_shell commands/file-permissions.txt 'stat -f "%Sp %Su %Sg %N" config.yaml data data/*.db data/apps logs logs/* certs certs/* /var/lib/office-automate /var/lib/office-automate/* /var/log/office-automate /var/log/office-automate/* 2>/dev/null'
capture hashes/server-binary.sha256 shasum -a 256 "$server_bin"
capture_shell hashes/artifacts.sha256 'if [[ -d data/apps ]]; then find data/apps -maxdepth 3 -type f -print0 | xargs -0 shasum -a 256; fi'

if [[ -f "$database_path" ]]; then
  capture sqlite/office-climate-quick-check.txt sqlite3 "$database_path" "PRAGMA quick_check;"
  capture sqlite/recent-device-events.txt sqlite3 "$database_path" "SELECT timestamp, device_type, event, device_name FROM device_events ORDER BY timestamp DESC LIMIT 100;"
  capture sqlite/recent-climate-actions.txt sqlite3 "$database_path" "SELECT timestamp, action, reason FROM climate_actions ORDER BY timestamp DESC LIMIT 100;"
  capture sqlite/recent-device-registration-audit.txt sqlite3 "$database_path" "SELECT timestamp, event, device_id, details FROM device_registration_audit ORDER BY timestamp DESC LIMIT 100;"
fi

copy_if_file "$config_path" "$evidence_dir/plists"
copy_if_file "$HOME/Library/LaunchAgents/com.office-automate.server.plist" "$evidence_dir/plists"
copy_if_file "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist" "$evidence_dir/plists"
copy_if_file "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist" "$evidence_dir/plists"
copy_if_file "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist" "$evidence_dir/plists" "user-com.office-automate.tunnel.plist"
copy_if_file "/Library/LaunchDaemons/com.office-automate.edge.plist" "$evidence_dir/plists" "system-com.office-automate.edge.plist"
copy_if_file "/Library/LaunchDaemons/com.office-automate.tunnel.plist" "$evidence_dir/plists" "system-com.office-automate.tunnel.plist"
if [[ -n "$cloudflared_config" ]]; then
  copy_if_file "$cloudflared_config" "$evidence_dir/plists"
  capture hashes/cloudflared-config.sha256 shasum -a 256 "$cloudflared_config"
fi

capture_shell logs/recent-office-logs.raw.secret.txt "for log in logs/*.log /var/log/office-automate/*/*.log; do [[ -f \"\$log\" ]] && { echo \"===== \$log\"; tail -n '$log_lines' \"\$log\"; }; done"
redact_file "$evidence_dir/logs/recent-office-logs.raw.secret.txt" "$evidence_dir/redacted/recent-office-logs.redacted.txt"

capture_shell logs/security-signals.raw.secret.txt "for log in logs/*.log /var/log/office-automate/*/*.log; do [[ -f \"\$log\" ]] && grep -iE 'auth|access|cloudflare|deploy|artifact|mqtt|qingping|yolink|token|secret|error|fail|restart' \"\$log\" | tail -n '$log_lines'; done"
redact_file "$evidence_dir/logs/security-signals.raw.secret.txt" "$evidence_dir/redacted/security-signals.redacted.txt"

cat >"$evidence_dir/README.txt" <<'README'
Office Automate incident evidence bundle.

Handling rules:
- Raw files may contain secrets, bearer tokens, cookies, device keys, or private
  configuration. Keep this directory local and encrypted.
- Do not paste raw output into tickets, chats, PRs, issue comments, or email.
- Share only files from redacted/ unless an explicit incident commander approves
  a narrower raw excerpt.
- Preserve this directory mode as 0700 and individual files as 0600.
README
chmod 600 "$evidence_dir/README.txt"

echo "captured=$evidence_dir"
echo "redacted_summaries=$evidence_dir/redacted"
