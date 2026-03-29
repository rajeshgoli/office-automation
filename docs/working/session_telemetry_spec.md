# Session Telemetry Collector

**Replaces:** Claude Code session-meta files (stopped writing after 2026-03-19)
**Feeds:** `GET /history/leverage` endpoint
**Storage:** Isolated `data/telemetry.db` — separate from `office_climate.db` so it can be nuked without affecting live data
**Runs on:** Work Mac (launchd, every 30 min) → rsynced to Mac Mini

## Problem

The leverage endpoint depends on the `session_output` table for lines_added, lines_removed, files_modified, git_commits, and git_pushes. This data came from Claude Code's `~/.claude/usage-data/session-meta/*.json` files, rsynced to the Mac Mini and parsed by `session_stats_parser.py::import_session_meta()`.

As of March 19, 2026, Claude Code stopped writing session-meta files. Codex never produced them. The `session_output` table is now empty — leverage ratios show zeros.

## Design Principles

1. **Git log is the source of truth for code output.** Commits are immutable, attributable, and contain exact line counts. No proxy needed.
2. **tool_usage.db is the source of truth for session activity.** It already tracks every tool call with session_id, project_name, and timestamps — 1M+ records and counting.
3. **Derive, don't duplicate.** Rather than building a parallel telemetry emitter, derive metrics from data that's already being collected.
4. **Periodic script, not a service.** A single Python script that runs periodically via launchd, queries two existing databases, and writes to an isolated telemetry DB. No daemons, no message queues.
5. **Isolated storage.** Telemetry lives in its own `data/telemetry.db`, not in `office_climate.db`. If it gets nuked or corrupted, live climate/occupancy data is unaffected. The leverage endpoint ATTACHes it at query time.

## Data Sources

### 1. tool_usage.db (session-manager hooks)

**Location:** `~/.local/share/claude-sessions/tool_usage.db` (work Mac only — not synced to Mac Mini, not needed there since the collector runs locally on the work Mac).

**Already captures per session:**

| Metric | How to derive |
|--------|--------------|
| `session_id` | `session_id` column (sm session ID) |
| `project_name` | `project_name` column (basename of cwd) |
| `start_time` | `MIN(timestamp) WHERE session_id = ?` |
| `duration_minutes` | `(MAX(timestamp) - MIN(timestamp))` in minutes |
| `user_message_count` | Not directly available — approximate from prompt-bearing tool sequences |
| `files_modified` | `COUNT(DISTINCT target_file) WHERE tool_name IN ('Write', 'Edit')` |
| `tool_counts` | `GROUP BY tool_name` counts |
| `is_human_session` | Check if `session_name` starts with `claude-` (human) vs named sessions dispatched by `sm dispatch` |

**What it cannot provide:** `lines_added`, `lines_removed`, `git_commits`, `git_pushes`, `input_tokens`, `output_tokens`, `languages`. These come from git log.

### 2. Git log on watched repos

**Repos to scan:** A configured list of local repo paths on the work Mac (same repos that exist on disk today):

```yaml
# In config.yaml under a new `telemetry` section
telemetry:
  repos:
    - ~/Desktop/automation/office-automate
    - ~/Desktop/automation/session-manager
    - ~/Desktop/automation/taskbar
    - ~/Desktop/fractal-market-simulator
    - ~/Desktop/automation/social_media
    - ~/Desktop/automation/backup-manager
  # Add more as needed
```

For each repo, run:
```
git -C <repo> log --all --no-merges --format="COMMIT:%H|%aI|%s" --shortstat --after=<cutoff>
```

Parse output to extract per-commit:
- `commit_hash`, `author_date` (convert to PST), `subject`
- `files_changed`, `insertions`, `deletions` (from `--shortstat` line)

**Why `--no-merges`:** Merge commits double-count the diff. The feature branch commit has the real stats; the merge commit replays them. Using `--no-merges` prevents inflation.

**Why `--all`:** Captures commits on feature branches that haven't been merged to main yet. Without it, a day of heavy branch work would show zero output.

**Cutoff:** Only scan commits newer than the oldest un-synced date. On first run, go back 30 days. On subsequent runs, go back 2 days (handles timezone edge cases).

### 3. Attributing commits to sessions (via tool_usage.db)

tool_usage.db records every `git commit` and `git push` Bash command with its `session_id` and `cwd`. This is the bridge:

```sql
SELECT session_id, session_name, cwd, bash_command, timestamp
FROM tool_usage
WHERE hook_type = 'PreToolUse'
  AND tool_name = 'Bash'
  AND (bash_command LIKE 'git commit%' OR bash_command LIKE 'git push%')
```

For each git commit command in tool_usage.db:
- The `cwd` tells us which repo
- The `timestamp` correlates to the commit's author date (within seconds)
- The `session_id` gives us session attribution

This lets us attribute `lines_added`, `lines_removed`, `files_modified`, `git_commits`, and `git_pushes` to specific sessions — which session-meta used to do.

**Fallback for commits not in tool_usage.db:** Some commits happen outside session-manager (manual git, CI, etc.). These go into a synthetic session per repo per day: `session_id = "unattributed-{repo}-{date}"`. They still count toward daily totals.

## Schema

**Isolated DB:** `data/telemetry.db` on both work Mac (source of truth) and Mac Mini (rsynced copy). Same `session_output` schema as before, but in its own file:

```sql
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
```

Fields we populate from this collector:
- `session_id` — sm session ID (e.g., `a544f0b2`)
- `project` — normalized project name
- `start_time` — earliest tool call timestamp (PST)
- `duration_minutes` — last minus first tool call
- `lines_added` — sum of insertions from attributed git commits
- `lines_removed` — sum of deletions from attributed git commits
- `files_modified` — sum of `files_changed` from git `--shortstat` across matched commits. **Known overcount:** if 3 commits in one session all touch the same file, it's counted 3 times. This is acceptable — the metric is directionally correct for leverage ratios and deduplicating per-file paths across commits adds complexity for marginal accuracy gain.
- `git_commits` — count of `git commit` commands in tool_usage.db for this session
- `git_pushes` — count of `git push` commands (excluding `--delete` branch cleanup)
- `tool_counts` — JSON dict of tool_name → count from tool_usage.db
- `is_human_session` — 1 if session_name matches `claude-[hex]` pattern (user-started), 0 if named (agent-dispatched)

Fields we leave at defaults (not derivable without Claude internals):
- `user_message_count` — 0 (not available from hooks)
- `assistant_message_count` — 0
- `input_tokens` — 0
- `output_tokens` — 0
- `languages` — NULL (could be derived from file extensions of edited files, but low value — skip for v1)

## Implementation

### File: `collect_session_telemetry.py` (new, project root)

Single script, three phases:

#### Phase 1: Build session index from tool_usage.db

```python
def build_session_index(tool_db: Path, cutoff_date: str) -> dict:
    """
    Returns {session_id: SessionInfo} where SessionInfo has:
      - session_name, project_name
      - start_time, end_time (min/max timestamp)
      - tool_counts: {tool_name: count}
      - files_modified: set of target_files from Write/Edit
      - git_commits: [(timestamp, cwd, bash_command), ...]
      - git_pushes: [(timestamp, cwd, bash_command), ...]
    """
```

Query tool_usage.db once:
```sql
SELECT session_id, session_name, project_name, tool_name,
       target_file, bash_command, timestamp, cwd
FROM tool_usage
WHERE hook_type = 'PreToolUse'
  AND timestamp >= ?
ORDER BY session_id, timestamp
```

Group in Python. This is ~3K rows/day, well within memory.

#### Phase 2: Collect git stats from repos

```python
def collect_git_stats(repos: list[Path], cutoff_date: str) -> dict:
    """
    Returns {(repo_basename, commit_hash): CommitStats} where CommitStats has:
      - author_date, subject
      - files_changed, insertions, deletions
    """
```

For each repo, run `git log` and parse the output. Regex for `--shortstat`:
```python
SHORTSTAT_RE = re.compile(
    r'\s*(\d+) files? changed'
    r'(?:,\s*(\d+) insertions?\(\+\))?'
    r'(?:,\s*(\d+) deletions?\(-\))?'
)
```

#### Phase 3: Attribute and upsert

For each session from Phase 1:
1. Match git commits: for each `git commit` command in the session, find the closest commit in Phase 2 by repo + timestamp (within 60s window).
2. Sum `insertions` → `lines_added`, `deletions` → `lines_removed`, `files_changed` → `files_modified` across matched commits.
3. Count `git_commits` and `git_pushes` (excluding `--delete`).
4. Build `tool_counts` JSON.
5. Determine `is_human_session` from session_name pattern.
6. `INSERT OR REPLACE` into `session_output`.

For unattributed commits (git commits with no matching tool_usage.db entry):
- Group by (repo, date).
- Create synthetic session `unattributed-{repo}-{date}` with the aggregated stats.
- This ensures daily totals in the leverage endpoint are complete even for manual or CI commits.

### Matching git commits to sessions

The key correlation: when Claude runs `git commit`, tool_usage.db records the timestamp and cwd. Git log records the commit's author date. These are within seconds of each other.

```python
def match_commit(session_commit_ts: datetime, repo: str,
                 git_commits: dict) -> Optional[CommitStats]:
    """Find a git commit within 60s of the tool_usage timestamp in the same repo."""
    for (r, h), stats in git_commits.items():
        if r == repo and abs((stats.author_date - session_commit_ts).total_seconds()) < 60:
            return stats
    return None
```

Why 60s tolerance: git commit runs near-instantly, but there can be slight clock differences between the hook timestamp and git's author date. 60s is generous enough to never miss a match but tight enough to avoid false positives (commits are rare enough that two won't land in the same 60s window in the same repo).

**Performance note:** The linear scan is O(sessions × commits), which is fine at current scale (~100 sessions/day, ~50 commits/day). If this becomes a bottleneck, sort commits by timestamp and use bisect for O(sessions × log(commits)) — but don't optimize prematurely.

### Data flow: Work Mac → Mac Mini

The collector runs on the **work Mac** where both `tool_usage.db` and the git repos live. It writes directly to a local `data/telemetry.db`. The Mac Mini's existing sync script (`scripts/sync_session_history.sh`) rsyncs this file — no import or merge step needed on the Mac Mini side. The leverage endpoint ATTACHes `telemetry.db` at query time.

### Launch Agent setup

**Work Mac** (where repos and tool_usage.db live):

Use launchd, not cron — the work Mac is a laptop that sleeps. cron jobs don't fire while sleeping and don't catch up on wake. `launchd` with `StartCalendarInterval` runs missed jobs on wake.

```xml
<!-- ~/Library/LaunchAgents/com.office-automate.telemetry.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.office-automate.telemetry</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/rajesh/Desktop/automation/office-automate/venv/bin/python</string>
    <string>/Users/rajesh/Desktop/automation/office-automate/collect_session_telemetry.py</string>
    <string>--output</string>
    <string>/Users/rajesh/Desktop/automation/office-automate/data/telemetry.db</string>
  </array>
  <key>WorkingDirectory</key>
  <string>/Users/rajesh/Desktop/automation/office-automate</string>
  <key>StartInterval</key>
  <integer>1800</integer>
  <key>StandardErrorPath</key>
  <string>/tmp/session-telemetry.log</string>
</dict>
</plist>
```

**Mac Mini** (add to `scripts/sync_session_history.sh` data sync section):
```bash
rsync -az "${WORK_MAC_USER}@${WORK_MAC_HOST}:~/Desktop/automation/office-automate/data/telemetry.db" "${DATA_DIR}/telemetry.db"
```

### Leverage endpoint change

The leverage endpoint ATTACHes `telemetry.db` at query time instead of reading from `session_output` in `office_climate.db`:

```python
# In the leverage query handler:
ATTACH 'data/telemetry.db' AS telemetry;
SELECT ... FROM telemetry.session_output WHERE date(start_time) = ?;
DETACH telemetry;
```

This means `telemetry.db` can be deleted, rebuilt, or corrupted without affecting climate/occupancy data.

### CLI interface

```
usage: collect_session_telemetry.py [-h] [--tool-db PATH] [--output PATH] [--days N] [--dry-run]

Collect session output metrics from tool_usage.db and git repos.

options:
  --tool-db PATH    Path to tool_usage.db (default: ~/.local/share/claude-sessions/tool_usage.db)
  --output PATH     Path to telemetry DB (default: data/telemetry.db)
  --days N          How many days back to scan (default: 2, first run: 30)
  --dry-run         Print what would be written, don't write
```

### Migration plan

`telemetry.db` becomes the single source of truth for all `session_output` data. This requires a one-time migration and a change to where `import_session_meta()` writes.

**Step 1: Migrate existing data from office_climate.db → telemetry.db**

```sql
-- Run once on Mac Mini during rollout
ATTACH 'data/telemetry.db' AS telemetry;
INSERT OR IGNORE INTO telemetry.session_output SELECT * FROM session_output;
DETACH telemetry;
```

This copies all pre-March-19 session-meta rows into `telemetry.db`. Using `INSERT OR IGNORE` means if the telemetry collector already backfilled a row, the existing (collector-derived) data is kept.

**Step 2: Redirect import_session_meta() to write to telemetry.db**

Change `session_stats_parser.py` so that `import_session_meta()` writes to `data/telemetry.db` instead of `office_climate.db`. The function currently calls `db.replace_session_output(rows)` via `database.py` — change it to open `telemetry.db` directly with the same `INSERT OR REPLACE` logic.

Session-meta data is richer (has token counts, message counts), so when both sources have the same session_id, session-meta wins. Import order: telemetry collector first, then session-meta importer (overwrites with richer data where available).

**Step 3: Drop the old table from office_climate.db**

After verifying the migration is complete (row counts match, leverage endpoint returns correct data):

```sql
-- Run once on Mac Mini after verification
DROP TABLE IF EXISTS session_output;
```

Remove the `CREATE TABLE session_output` and `replace_session_output()` from `database.py`. The leverage endpoint now reads exclusively from `telemetry.db` via ATTACH.

**Step 4: Clean up database.py**

Remove `replace_session_output()` (database.py:475-520) and the `session_output` table creation from the schema. The leverage query (database.py:910-921) changes to ATTACH `telemetry.db` as described in the Leverage endpoint change section.

### Backfill

On first run, use `--days 30` to backfill from when session-meta stopped. tool_usage.db has data going back to January 2026. Git log has full history. This populates the gap from March 19 to present.

For dates before March 19 where session-meta files exist: the migration (Step 1 above) carries those rows into `telemetry.db`. The session-meta importer continues to run afterward and overwrites with richer data where available (token counts, message counts).

## What this doesn't capture (and why that's OK)

| Metric | Status | Notes |
|--------|--------|-------|
| `input_tokens` | 0 | Only available from Claude internals. Not visible from hooks. |
| `output_tokens` | 0 | Same. |
| `user_message_count` | 0 | Hooks fire on tool calls, not conversational turns. Could approximate by counting distinct tool_use_id sequences, but that's fragile. |
| `assistant_message_count` | 0 | Same. |
| `languages` | NULL | Could derive from file extensions, but low leverage for the leverage dashboard. |

These fields were nice-to-have in session-meta but are **not used by the leverage endpoint**. The endpoint computes:
- `lines_per_prompt` — uses `lines_added + lines_removed` (from this collector) / `prompts` (from `orchestration_activity`, already working)
- `commits_per_prompt` — uses `git_commits` (from this collector) / `prompts`
- `lines_per_session_minute` — uses lines / `duration_minutes` (from this collector)

All three leverage ratios are fully covered.

## Testing

1. **Unit test:** Mock tool_usage.db and git log output, verify session index building and commit attribution.
2. **Integration test:** Run against real tool_usage.db and a real repo, verify upserts into staging DB.
3. **Backfill validation:** After first run with `--days 30`, compare daily totals against `git log --shortstat` for a known day. They should match within 5% (some commits may be on branches that were deleted).

## Rollout

1. Implement `collect_session_telemetry.py` on work Mac
2. Run `--days 30 --dry-run` to validate output
3. Run `--days 30` to backfill telemetry.db
4. Migrate existing session_output from office_climate.db → telemetry.db (Migration Step 1)
5. Redirect `import_session_meta()` to write to telemetry.db (Migration Step 2)
6. Update leverage endpoint to ATTACH telemetry.db
7. Verify leverage endpoint returns non-zero data (both pre- and post-March-19 dates)
8. Drop `session_output` table from office_climate.db (Migration Step 3)
9. Clean up `database.py` (Migration Step 4)
10. Install Launch Agent on work Mac
11. Add telemetry.db rsync to `scripts/sync_session_history.sh`
12. Verify Android leverage cards show real data
