#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: sync_rollback_snapshot.sh <snapshot-dir> <backup-root>

Copies one completed Office Automate rollback snapshot to backup storage.
This is backup/snapshot sync only; it does not sync operational telemetry,
tool-usage, project-leverage, or live controller state between hosts.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -ne 2 ]]; then
  usage >&2
  exit 64
fi

snapshot_dir="${1%/}"
backup_root="${2%/}"

if [[ ! -d "$snapshot_dir" ]]; then
  echo "snapshot directory does not exist: $snapshot_dir" >&2
  exit 66
fi

if [[ ! -f "$snapshot_dir/manifest.json" ]]; then
  echo "snapshot manifest is missing: $snapshot_dir/manifest.json" >&2
  exit 66
fi

mkdir -p "$backup_root"
destination="$backup_root/$(basename "$snapshot_dir")"

rsync -a --delete --chmod=go-rwx "$snapshot_dir/" "$destination/"
echo "Synced rollback snapshot to $destination"
