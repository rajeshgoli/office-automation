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
    # Push only github_prs table via dump+load (don't overwrite the full DB).
    # Dump to a temp file first so set -e halts before the remote DROP runs
    # if the local dump fails — piping straight to ssh would let the remote
    # destructive step proceed and clobber the Mac Mini's table.
    DUMP=$(mktemp -t github_prs_dump.XXXXXX)
    trap 'rm -f "$DUMP"' EXIT
    sqlite3 data/office_climate.db ".dump github_prs" > "$DUMP"
    [ -s "$DUMP" ] || { echo "ERROR: github_prs dump empty"; exit 1; }
    ssh "$MINI" "sqlite3 ~/office-automate/data/office_climate.db 'DROP TABLE IF EXISTS github_prs;' && sqlite3 ~/office-automate/data/office_climate.db" < "$DUMP"
    echo "Pushed to Mac Mini"
else
    echo "Mac Mini unreachable, skipping push"
fi

echo "Done"
