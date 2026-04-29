#!/bin/bash
# Run telemetry + PR collectors locally (needs git repos + gh CLI),
# then push telemetry.db to Mac Mini.
set -e

cd "$(dirname "$0")"
LOG="/tmp/office-automate-collect.log"
MINI="rajesh@bakasura4.local"
VENV="./venv/bin/python"

exec >> "$LOG" 2>&1
echo "--- $(date) ---"

source venv/bin/activate

# Build worktree map (needs local git repos for branch matching)
$VENV -c "
from src.project_names import build_worktree_map, WORKTREE_MAP_PATH
import json
mapping = build_worktree_map()
with open(WORKTREE_MAP_PATH, 'w') as f:
    json.dump(mapping, f, indent=2, sort_keys=True)
print(f'Worktree map: {len(mapping)} entries')
" 2>&1

# Collect session telemetry (needs tool_usage.db + local git repos)
$VENV collect_session_telemetry.py 2>&1

# Collect GitHub PRs (needs gh CLI)
$VENV session_stats_parser.py --mode github-prs 2>&1

# Push to Mac Mini
if ssh -o ConnectTimeout=5 "$MINI" true 2>/dev/null; then
    rsync -az data/telemetry.db "$MINI:~/office-automate/data/telemetry.db"
    rsync -az data/worktree_map.json "$MINI:~/office-automate/data/worktree_map.json"
    # Replace the remote github_prs table atomically inside one transaction:
    # DROP + restore + index DDL all stream into a single remote sqlite3
    # invocation, so a mid-stream SSH failure rolls back instead of leaving
    # the Mac Mini with a missing or partially rebuilt table. Indexes are
    # pulled from sqlite_master (.dump TABLE doesn't include them) so any
    # index added in database.py propagates without script edits.
    DUMP=$(mktemp -t github_prs_dump.XXXXXX)
    INDEXES=$(mktemp -t github_prs_idx.XXXXXX)
    trap 'rm -f "$DUMP" "$INDEXES"' EXIT
    sqlite3 data/office_climate.db ".dump github_prs" > "$DUMP"
    # .dump TABLE emits PRAGMA + BEGIN/COMMIT boilerplate even when the table
    # doesn't exist locally (e.g. fresh checkout, gh unavailable on first run).
    # Without this guard the transaction would be DROP-only — wiping the
    # remote table with nothing to put back.
    grep -q '^CREATE TABLE github_prs' "$DUMP" || {
        echo "ERROR: local github_prs table missing; refusing to push"
        exit 1
    }
    sqlite3 data/office_climate.db \
        "SELECT sql || ';' FROM sqlite_master WHERE tbl_name='github_prs' AND type='index' AND sql IS NOT NULL" \
        > "$INDEXES"
    {
        echo "BEGIN;"
        echo "DROP TABLE IF EXISTS github_prs;"
        grep -vE '^(PRAGMA |BEGIN TRANSACTION;|COMMIT;)' "$DUMP"
        cat "$INDEXES"
        echo "COMMIT;"
    } | ssh "$MINI" "sqlite3 ~/office-automate/data/office_climate.db"
    echo "Pushed to Mac Mini"
else
    echo "Mac Mini unreachable, skipping push"
fi

echo "Done"
