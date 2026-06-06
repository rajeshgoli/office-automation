#!/usr/bin/env bash
set -euo pipefail

execute=false
backup_root="${OFFICE_AUTOMATE_DECOMMISSION_BACKUP_DIR:-}"
plist_paths=()

usage() {
  cat <<'USAGE'
Usage: decommission_legacy_launchd.sh [--execute] --backup-dir <dir> <plist>...

Backs up and unloads legacy Office Automate launchd plists after the rollback
window has passed. Default mode is a dry run. Pass --execute to copy plists,
attempt launchctl bootout, and move the original plist aside.

Only pass legacy plists. Do not pass the primary-host Rust plists from
scripts/launchd/primary-host or the installed com.office-automate.server,
com.office-automate.telemetry, com.office-automate.project-leverage, and
com.office-automate.tunnel primary-host services.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --execute)
      execute=true
      shift
      ;;
    --backup-dir)
      backup_root="${2:-}"
      if [[ -z "$backup_root" ]]; then
        echo "--backup-dir requires a value" >&2
        exit 64
      fi
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      while [[ $# -gt 0 ]]; do
        plist_paths+=("$1")
        shift
      done
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage >&2
      exit 64
      ;;
    *)
      plist_paths+=("$1")
      shift
      ;;
  esac
done

if [[ -z "$backup_root" ]]; then
  echo "backup directory is required via --backup-dir or OFFICE_AUTOMATE_DECOMMISSION_BACKUP_DIR" >&2
  exit 64
fi

if [[ ${#plist_paths[@]} -eq 0 ]]; then
  echo "at least one legacy plist path is required" >&2
  exit 64
fi

timestamp="$(date +%Y%m%d-%H%M%S)"
backup_dir="$backup_root/legacy-launchd-$timestamp"
uid_domain="gui/$(id -u)"

plist_label() {
  local plist="$1"
  plutil -extract Label raw -o - "$plist" 2>/dev/null || basename "$plist" .plist
}

run_or_echo() {
  if [[ "$execute" == true ]]; then
    "$@"
  else
    printf '[dry-run] '
    printf '%q ' "$@"
    printf '\n'
  fi
}

echo "Backup directory: $backup_dir"
if [[ "$execute" == true ]]; then
  mkdir -p "$backup_dir/plists" "$backup_dir/logs"
fi

for plist in "${plist_paths[@]}"; do
  if [[ ! -f "$plist" ]]; then
    echo "legacy plist does not exist: $plist" >&2
    exit 66
  fi

  label="$(plist_label "$plist")"
  echo "Legacy service: $label"
  run_or_echo cp "$plist" "$backup_dir/plists/$(basename "$plist")"

  while IFS= read -r log_path; do
    [[ -z "$log_path" || ! -f "$log_path" ]] && continue
    run_or_echo cp "$log_path" "$backup_dir/logs/$(basename "$log_path")"
  done < <(plutil -extract StandardOutPath raw -o - "$plist" 2>/dev/null || true; plutil -extract StandardErrorPath raw -o - "$plist" 2>/dev/null || true)

  if launchctl print "$uid_domain/$label" >/dev/null 2>&1; then
    run_or_echo launchctl bootout "$uid_domain" "$plist"
  else
    echo "Service is not loaded in $uid_domain: $label"
  fi

  run_or_echo mv "$plist" "$plist.disabled-$timestamp"
done

if [[ "$execute" == true ]]; then
  echo "Legacy launchd decommission completed. Backup: $backup_dir"
else
  echo "Legacy launchd decommission planned. Backup: $backup_dir"
fi
