# Productivity Phase 2: Leverage & Deployment

**Classification: Epic** — 5 sub-tickets (A: session-meta pipeline, B: GitHub PR pipeline, C: leverage endpoints, D: Android UI, E: artifact server + domain rename). A and B are independent; C depends on both; D depends on C; E is independent.

## Context

Phase 1 (PR #22, merged) captured the *input* side of the leverage equation — every human prompt to Claude Code and Codex, timestamped, stored, and visualized in the Productivity tab. The tab shows office sessions, orchestration activity, project focus, and daily/weekly summary cards.

Phase 2 adds the *output* side — what did those prompts produce? — and computes the ratio. The core data sources are Claude's session-meta files (lines added, commits, files modified per session) and GitHub PRs (merged work units). Together with the existing orchestration_activity table, these yield leverage metrics like lines-per-prompt and commits-per-day that answer the question: "am I getting better at orchestrating agents?"

This spec also addresses two operational gaps: a generic artifact server for deploying APKs (not just office-climate, but any app like session-manager), and renaming the domain from `climate.rajeshgo.li` to `office.rajeshgo.li` to reflect that this system is no longer just about climate control.

## Tenets

1. **Measure leverage, not volume.** Ratios always; never celebrate raw line counts. A day with 3 prompts that produced 200 commits has higher leverage than 50 prompts that went in circles.

2. **Prefer session-meta over git log.** Session-meta attributes output to sessions directly — it covers zero-commit work (research, debugging, exploration) and avoids the timestamp-matching complexity of `git log` parsing.

3. **Pipeline is idempotent and append-only.** Same principle as Phase 1. Re-running the parser on the same data must not create duplicates. Session-meta uses `session_id` as a natural primary key; PRs use `(repo, pr_number)`.

4. **Ship the ratio card before the trend chart.** Summary cards first, sparklines and trend lines later. Get the numbers visible, iterate on visualization.

5. **Per-project leverage is the north star.** Each project Rajesh builds is a lever for his own work. The ultimate dashboard shows not just aggregate productivity but per-tool usage signals — is session-manager actually being used? Is engram's knowledge staying fresh?

---

## Part A: Session-Meta Pipeline

### Source Files

Claude Code writes per-session analytics to `~/.claude/usage-data/session-meta/*.json` on the work Mac. There are ~800+ files, one per session. Each file is a JSON object with these fields (from a real sample):

```json
{
  "session_id": "001e8fcc-67e6-4745-bbdb-4892a88065f4",
  "project_path": "/Users/rajesh/Desktop/fractal-market-simulator",
  "start_time": "2026-03-03T02:49:15.095Z",
  "duration_minutes": 0,
  "user_message_count": 3,
  "assistant_message_count": 0,
  "tool_counts": {},
  "languages": {},
  "git_commits": 0,
  "git_pushes": 0,
  "input_tokens": 0,
  "output_tokens": 0,
  "first_prompt": "No prompt",
  "user_interruptions": 0,
  "user_response_times": [],
  "tool_errors": 0,
  "tool_error_categories": {},
  "uses_task_agent": false,
  "uses_mcp": false,
  "uses_web_search": false,
  "uses_web_fetch": false,
  "lines_added": 0,
  "lines_removed": 0,
  "files_modified": 0,
  "message_hours": [18, 18, 18],
  "user_message_timestamps": ["2026-03-03T02:49:15.285Z", ...]
}
```

Key fields for leverage: `session_id`, `project_path`, `start_time` (UTC — must convert to PST), `duration_minutes`, `lines_added`, `lines_removed`, `files_modified`, `git_commits`, `git_pushes`, `user_message_count`, `assistant_message_count`, `input_tokens`, `output_tokens`, `tool_counts` (JSON dict), `languages` (JSON dict), `first_prompt`.

### Sync Mechanism

Extend the existing rsync cron job on the Mac Mini to also sync the session-meta directory:

```
rsync -az rajesh@<work-mac-ip>:~/.claude/usage-data/session-meta/ ~/office-automate/data/session-meta/
```

This syncs the entire directory. New files appear as new JSON files; existing files may be updated (session-meta files are written incrementally as a session progresses). The parser must handle both new and updated files.

### Filtering

Sessions with `duration_minutes == 0 AND user_message_count == 0` are process artifacts (Claude Code starting and immediately exiting). Skip these.

The `first_prompt` field can identify machine-dispatched sessions. Use the existing `is_machine_generated()` function from `session_stats_parser.py` to check `first_prompt`. Flag the result as `is_human_session` — agent-dispatched sessions still produce output that counts toward leverage, but only human-initiated prompts go in the denominator for per-prompt ratios.

### Database Schema

Add to `src/database.py` in `_init_schema()`:

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
CREATE INDEX IF NOT EXISTS idx_session_output_start ON session_output(start_time);
CREATE INDEX IF NOT EXISTS idx_session_output_project ON session_output(project);
```

No expression indexes — Mac Mini runs SQLite on High Sierra which doesn't support them.

`tool_counts` and `languages` are stored as JSON strings (matching the source format). `is_human_session` is 0 or 1.

### Parser Extension

Add a new function to `session_stats_parser.py`:

**Contract:**

1. Scan `data/session-meta/` for all `.json` files.
2. For each file, parse JSON. If malformed, skip and log a warning.
3. Extract `session_id`. Use `INSERT OR REPLACE` (unconditional upsert) keyed on `session_id`. Session-meta files mutate many fields over time — not just `duration_minutes` and `lines_added`, but also `files_modified`, `git_commits`, `git_pushes`, `assistant_message_count`, `input_tokens`, `output_tokens`, `tool_counts`, and `languages`. Checking individual fields for staleness is fragile; unconditional replace is correct and cheap (one row per session, ~800 total).
4. Filter: skip if `duration_minutes == 0 AND user_message_count == 0`.
5. Convert `start_time` from UTC ISO 8601 to local Pacific time: `datetime.fromisoformat(start_time.replace('Z', '+00:00')).astimezone().strftime(...)`.
6. Extract project name: `os.path.basename(project_path)` → apply `_normalize_project()`.
7. Set `is_human_session = 0` if `is_machine_generated(first_prompt)`, else `1`.
8. Upsert the row.

This is not append-only like history.jsonl — session-meta files are mutable (updated during a session). The parser must handle upserts, not just inserts.

---

## Part B: GitHub PR Pipeline

### Data Source

All repositories under the `rajeshgoli` GitHub account. The collector enumerates repos dynamically via `gh repo list rajeshgoli --json name --limit 100` rather than a hardcoded list. This picks up new repos automatically.

For each repo, fetch PRs with pagination: `gh pr list --repo rajeshgoli/{repo} --state all --json number,title,state,additions,deletions,changedFiles,createdAt,mergedAt --limit 500`. session-manager and fractal-market-simulator already exceed 200 PRs each. If a repo returns exactly 500 results, log a warning that pagination may be needed — but 500 is sufficient headroom for the foreseeable future.

### Sync Mechanism

A script (or extension to `session_stats_parser.py`) runs on the Mac Mini after each rsync cron cycle. It requires `gh` CLI authenticated on the Mac Mini.

**Rate limiting:** GitHub API allows 5,000 requests/hour for authenticated users. With ~12 repos, a full sync is ~12 API calls (one per repo). Running every 30 minutes = 24 calls/hour. Well within limits.

### Database Schema

```sql
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
```

Timestamps from the GitHub API are UTC ISO 8601. Convert to local Pacific time on insert, consistent with every other table in this database.

### Parser Contract

1. Run `gh repo list rajeshgoli --json name --limit 100`. Parse repo names.
2. For each repo, run `gh pr list` as above.
3. For each PR, upsert by `(repo, pr_number)`. PRs change state over time (open → merged, additions change during review).
4. Convert `createdAt` and `mergedAt` from UTC to PST.
5. If `gh` CLI is not available or not authenticated, log a warning and skip. The pipeline must not fail if GitHub is unreachable.

---

## Part C: Leverage Endpoints

### `GET /history/leverage?days=7`

Joins `orchestration_activity`, `session_output`, and `github_prs` to produce per-day leverage metrics.

**Response:**
```json
{
  "ok": true,
  "days": [
    {
      "date": "2026-03-27",
      "prompts": 42,
      "sessions": 5,
      "lines_added": 1250,
      "lines_removed": 340,
      "lines_changed": 1590,
      "files_modified": 28,
      "commits": 12,
      "prs_merged": 3,
      "prs_opened": 5,
      "avg_pr_cycle_hours": 2.3,
      "lines_per_prompt": 37.9,
      "commits_per_prompt": 0.29,
      "lines_per_session_minute": 4.2
    }
  ],
  "week": {
    "prompts": 180,
    "sessions": 22,
    "lines_added": 5400,
    "lines_removed": 1200,
    "lines_changed": 6600,
    "files_modified": 95,
    "commits": 48,
    "prs_merged": 14,
    "prs_opened": 18,
    "avg_pr_cycle_hours": 3.1,
    "lines_per_prompt": 36.7,
    "commits_per_prompt": 0.27,
    "lines_per_session_minute": 3.8,
    "active_days": 5
  }
}
```

**Computation rules:**

- `prompts` = count from `orchestration_activity` for that day (human prompts only, already filtered by Phase 1 parser).
- `sessions` = distinct `session_id` from `orchestration_activity`.
- `lines_added`, `lines_removed`, `files_modified`, `commits` = SUM from `session_output` where `date(start_time) = date`. Include ALL sessions (both human and machine-dispatched). Agent-dispatched sessions produce real output that the human's orchestration prompt caused — excluding them would undercount leverage. The `is_human_session` flag exists for analysis (e.g., "what fraction of output came from agent swarms?"), not for filtering.
- `lines_changed = lines_added + lines_removed`.
- `prs_merged` = COUNT from `github_prs` where `date(merged_at) = date`.
- `prs_opened` = COUNT from `github_prs` where `date(created_at) = date`.
- `avg_pr_cycle_hours` = AVG of `(merged_at - created_at)` in hours for PRs merged that day. Null if no PRs merged.
- `lines_per_prompt = lines_changed / prompts`. Null when `prompts = 0`.
- `commits_per_prompt = commits / prompts`. Null when `prompts = 0`.
- `lines_per_session_minute = lines_changed / SUM(duration_minutes)`. Null when total duration is 0.

The `week` object aggregates all days in the response. `active_days` = count of days with `prompts > 0`.

**Null handling:** When a denominator is zero, the ratio must be `null` in JSON (not 0, not Infinity). The Android client renders null ratios as "--".

### Endpoint Registration

Register at `self._app.router.add_get("/history/leverage", self._handle_history_leverage_get)` alongside the existing `/history/*` routes at line ~2048 of `src/orchestrator.py`. Follows the same `try/except` pattern with `days` query parameter clamped to `[1, 30]`.

---

## Part D: Android UI

### Leverage Cards

Add two new sections to `ProductivityScreen.kt`, below the existing daily/weekly summary cards:

**Today Leverage (2×2 FlowRow):**

| Card | Value | Format | Accent |
|------|-------|--------|--------|
| LINES/PROMPT | `lines_per_prompt` | "38.2" or "--" | Emerald |
| COMMITS | `commits` | "12" | Amber |
| PRs MERGED | `prs_merged` | "3" | Blue |
| LINES CHANGED | `lines_changed` | "1,590" | Cyan |

**This Week Leverage (2×2 FlowRow):**

| Card | Value | Format | Accent |
|------|-------|--------|--------|
| WEEK LINES | `week.lines_changed` | "6,600" | Emerald |
| WEEK COMMITS | `week.commits` | "48" | Amber |
| WEEK PRs | `week.prs_merged` | "14" | Blue |
| AVG L/PROMPT | `week.lines_per_prompt` | "36.7" or "--" | Cyan |

### Data Models

Add to `HistoryModels.kt`:

```kotlin
@Serializable
data class LeverageResponse(
    val ok: Boolean,
    val days: List<LeverageDay>,
    val week: LeverageWeek
)

@Serializable
data class LeverageDay(
    val date: String,
    val prompts: Int,
    val sessions: Int,
    @SerialName("lines_added") val linesAdded: Int,
    @SerialName("lines_removed") val linesRemoved: Int,
    @SerialName("lines_changed") val linesChanged: Int,
    @SerialName("files_modified") val filesModified: Int,
    val commits: Int,
    @SerialName("prs_merged") val prsMerged: Int,
    @SerialName("prs_opened") val prsOpened: Int,
    @SerialName("avg_pr_cycle_hours") val avgPrCycleHours: Double?,
    @SerialName("lines_per_prompt") val linesPerPrompt: Double?,
    @SerialName("commits_per_prompt") val commitsPerPrompt: Double?,
    @SerialName("lines_per_session_minute") val linesPerSessionMinute: Double?
)

@Serializable
data class LeverageWeek(
    val prompts: Int,
    val sessions: Int,
    @SerialName("lines_added") val linesAdded: Int,
    @SerialName("lines_removed") val linesRemoved: Int,
    @SerialName("lines_changed") val linesChanged: Int,
    @SerialName("files_modified") val filesModified: Int,
    val commits: Int,
    @SerialName("prs_merged") val prsMerged: Int,
    @SerialName("prs_opened") val prsOpened: Int,
    @SerialName("avg_pr_cycle_hours") val avgPrCycleHours: Double?,
    @SerialName("lines_per_prompt") val linesPerPrompt: Double?,
    @SerialName("commits_per_prompt") val commitsPerPrompt: Double?,
    @SerialName("lines_per_session_minute") val linesPerSessionMinute: Double?,
    @SerialName("active_days") val activeDays: Int
)
```

### API Service / Repository

Add to `ApiService.kt`:
```kotlin
@GET("history/leverage")
suspend fun getLeverage(@Query("days") days: Int = 7): LeverageResponse
```

Add corresponding wrapper in `ClimateRepository.kt` with `runCatching {}`. Fetch in `ProductivityViewModel` in parallel with existing orchestration/project-focus calls.

---

## Part E: Artifact Server & Domain Rename

### Generic Artifact Server

Replace the current single-file `/apk` endpoint with a multi-app artifact server.

**`POST /deploy/{app}`** — Upload an artifact.

- OAuth-protected (NOT in `skip_paths`).
- **App name validation:** `{app}` must match `^[a-z0-9][a-z0-9-]*$` (lowercase alphanumeric + hyphens, no leading hyphen). Return 400 on mismatch. This prevents path traversal and unexpected filesystem writes.
- Multipart form upload, field name `file`.
- Writes atomically to `data/apps/{app}/latest.apk` using temp file + `os.replace()`.
- Creates `data/apps/{app}/` directory on first upload.
- 100 MB size limit. Return 413 if exceeded.
- Stores upload metadata in `data/apps/{app}/meta.json`: `{"uploaded_at": "...", "size_bytes": N, "uploaded_by": "email"}`.

Response:
```json
{"ok": true, "app": "office-climate", "size_bytes": 12345678, "download_url": "/apps/office-climate/latest.apk"}
```

**`GET /apps/{app}/latest.apk`** — Download the latest artifact.

- No authentication required (same as current `/apk`).
- Add `/apps/` prefix to `skip_paths` in `_oauth_middleware()`.
- Returns 404 if no artifact exists for that app name.
- `Content-Disposition: attachment; filename={app}.apk`.

**Migration:** The existing `GET /apk` endpoint and `data/app-debug.apk` file continue to work during the transition. Once the Android app is updated to use `/apps/office-climate/latest.apk`, deprecate and remove the old endpoint.

**App naming convention:** Lowercase, hyphenated. Examples: `office-climate`, `session-manager`, `engram`.

### Domain Rename

Rename `climate.rajeshgo.li` to `office.rajeshgo.li` in the Cloudflare tunnel configuration.

**Changes required:**
1. Cloudflare dashboard: Update tunnel public hostname from `climate.rajeshgo.li` to `office.rajeshgo.li`.
2. Android app: Update default server URL in `android/.../util/Constants.kt:10` (the actual `BASE_URL` constant). Also update the placeholder URL shown in `android/.../ui/settings/SettingsScreen.kt:89`.
3. `occupancy_detector.py`: Default URL is `http://localhost:8080` (line 308-309), not the public domain. No change needed — the production Launch Agent overrides with `--url http://192.168.5.140:8080`. But update `CLAUDE.md` examples that reference the public URL.
4. `CLAUDE.md`: Update all references from `climate.loca.lt` / `climate.rajeshgo.li` to `office.rajeshgo.li`.
5. OAuth redirect URIs in Google Cloud Console: Add `https://office.rajeshgo.li/auth/callback`.
6. Keep `climate.rajeshgo.li` as a redirect to `office.rajeshgo.li` for a transition period (Cloudflare Page Rule or second tunnel route).

**Artifact subdomain (optional, deferred):** `apks.rajeshgo.li` could point to the same tunnel with a path prefix, but it's simpler to serve artifacts from `office.rajeshgo.li/apps/` for now. Revisit if the artifact server grows.

---

## Tier 3: Per-Project Leverage Metrics (Design Only)

This section captures the vision for domain-specific leverage signals per project. These are not implemented in Phase 2 — they require instrumentation in each project. The purpose is to identify what's measurable so that future phases can add collection.

Each project Rajesh builds is a tool that multiplies his own output. The leverage question per project is: "is this tool actually being used, and how much is it amplifying my work?"

### session-manager

The highest-leverage tool. Enables parallel agent orchestration and mobile productivity.

| Signal | Source | Why it matters |
|--------|--------|---------------|
| sm-managed sessions | session-manager logs/DB | How many agents are running under sm vs. bare Claude |
| sm dispatches | `[sm dispatch]` messages in history | Parallel work units launched |
| sm sends | `[sm send]` messages | Inter-agent coordination events |
| sm reminds | `[sm remind]` messages | Active monitoring of agent work |
| Telegram messages | Telegram bot logs | Mobile orchestration — highest leverage signal. Every Telegram message means Rajesh is productive while away from his desk |
| SSH sessions | sm ssh connection logs | Remote terminal access to agents |

**Mechanically derivable now?** Not from `orchestration_activity` — Phase 1's parser (`session_stats_parser.py:25-27`) filters out `[sm` and `[Input from:` messages *before* inserting, so they never reach the database. To count sm usage, the parser would need a schema change (e.g., a separate `sm_activity` table) or a direct scan of the raw `claude_history.jsonl` file counting excluded lines. Telegram and SSH require instrumentation in session-manager.

### engram

Knowledge maintenance for codebases.

| Signal | Source | Why it matters |
|--------|--------|---------------|
| Fold recency | engram's fold timestamps | Is the knowledge up to date? Stale folds = no leverage |
| Concept registry hits | Agent session logs (grep for registry terms) | Are agents actually using the maintained knowledge? |
| Briefing generations | engram API/logs | How often are compressed briefings requested? |

**Mechanically derivable now?** No — requires engram to expose an API or write telemetry.

### office-automate

Climate/productivity automation for the shed office.

| Signal | Source | Why it matters |
|--------|--------|---------------|
| Automation events | `climate_actions` table | ERV/HVAC actions that would have been manual |
| State transitions | `occupancy_log` table | System correctly detecting presence without manual input |
| Dashboard views | HTTP access logs (if logged) | Is the dashboard actually consulted? |

**Mechanically derivable now?** Yes — the data is already in the database. A simple daily count of `climate_actions` and `occupancy_log` entries shows automation activity.

### taskbar

Native macOS taskbar replacement.

| Signal | Source | Why it matters |
|--------|--------|---------------|
| Active usage | Process uptime / launch frequency | Is it running? Is it the primary window manager? |
| Window switches | AppKit event counts (needs instrumentation) | How many context switches does it handle per day? |

**Mechanically derivable now?** No — requires taskbar to log telemetry.

### agent-os

Workflow conventions and personas for AI agents.

| Signal | Source | Why it matters |
|--------|--------|---------------|
| Persona activations | Grep for "As engineer", "As reviewer" etc. in orchestration history | Which personas are actually used? |
| Persona adoption across repos | Cross-reference project field with persona mentions | Is agent-os used beyond office-automate? |

**Mechanically derivable now?** Partially — persona invocations appear in Claude history prompts. A regex scan of orchestration_activity or raw history.jsonl could count these.

### Implementation path for Tier 3

Phase 2 ships Tier 2 (session-meta + GitHub PRs). Tier 3 collection follows in Phase 3:

1. **Quick wins (derivable from existing data):** sm message counts from raw `claude_history.jsonl` (scan for `[sm` / `[Input from:` prefixes — these are excluded by the parser, so must be counted from the source file or via a new parser pass), office-automate automation event counts from `climate_actions` table, persona activation grep from raw history.
2. **Needs instrumentation:** Telegram message counts (session-manager), fold timestamps (engram), window switch counts (taskbar).
3. **Needs API:** engram concept registry queries, session-manager SSH session counts.

---

## Out of Scope

- **Tier 3 implementation.** This phase designs the Tier 3 vision but only implements Tier 2 (session-meta + GitHub PRs).
- **Git log parsing.** Session-meta already provides lines_added/removed/commits per session. Git log adds commit-level granularity but complex timestamp matching. Deferred to Phase 2.5.
- **Codex session-meta.** Codex CLI doesn't produce session-meta files. Only Claude Code output metrics are available.
- **Trend charts / sparklines.** Summary cards only. Trend visualization is a follow-up.
- **Real-time updates.** Stays 30-min cron. No WebSocket push for new session-meta.
- **Commit-level attribution.** Which prompt produced which commit — requires git log timestamp matching. Deferred.
- **Backend deployment via /deploy.** The artifact server handles APK uploads. Deploying Python files + restarting the orchestrator is a different workflow (SSH-based) and stays manual.

---

## Test Plan

### Part A: Session-Meta Parser

1. **Basic import.** Create a temp directory with 3 session-meta JSON files: one with 5 commits and 100 lines_added, one with 0 commits but 20 lines (research session), one with `duration_minutes=0 AND user_message_count=0` (process artifact). Assert parser imports exactly 2 rows. Assert the artifact session is skipped.

2. **Idempotency.** Run parser twice on the same 3 files. Assert row count is still 2 (INSERT OR IGNORE on session_id PK).

3. **Upsert on update.** Import a session-meta file with `lines_added=0, git_commits=0`. Replace the file with an updated version where `lines_added=50, git_commits=3`. Re-run parser. Assert the row now shows `lines_added=50` AND `git_commits=3` (unconditional replace, not field-by-field check).

4. **UTC to Pacific time conversion.** Session with `start_time: "2026-03-03T02:49:15.095Z"` must parse to `2026-03-02 18:49:15` Pacific (PST, UTC-8 in winter). Verify stored timestamp.

5. **Project normalization.** Session with `project_path: "/Users/rajesh/worktrees/fractal-1808-em"` must store project as `"fractal"`. Session with `project_path: "/Users/rajesh/Desktop/automation/session-manager"` must store as `"session-manager"`.

6. **Machine-generated detection.** Session with `first_prompt: "[Input from: spec-reviewer (d7436972) via sm send] Review this spec"` must set `is_human_session=0`. Session with `first_prompt: "Fix the login bug"` must set `is_human_session=1`.

7. **Malformed JSON.** Place a truncated file in the session-meta directory. Assert parser skips it, logs a warning, and imports all valid files.

### Part B: GitHub PR Pipeline

8. **PR import.** Mock `gh` CLI output with 5 PRs across 2 repos: 3 merged, 1 open, 1 closed-not-merged. Assert 5 rows inserted with correct state values.

9. **PR upsert.** Import a PR with state "open". Re-import with state "merged" and a `merged_at` timestamp. Assert the row is updated, not duplicated.

10. **UTC to Pacific time.** PR with `createdAt: "2026-03-15T20:30:00Z"` must store as `2026-03-15 13:30:00` Pacific (PDT, UTC-7 in March after DST). Verify.

11. **gh CLI unavailable.** Mock `gh` as not found. Assert the pipeline logs a warning and returns gracefully without crashing.

### Part C: Leverage Endpoint

12. **Basic computation.** Insert: 10 orchestration_activity rows for day X, 2 session_output rows totaling 200 lines_added + 50 lines_removed + 8 commits, 1 github_prs row merged on day X. Call `GET /history/leverage?days=1`. Assert: `prompts=10`, `lines_changed=250`, `commits=8`, `prs_merged=1`, `lines_per_prompt=25.0`, `commits_per_prompt=0.8`.

13. **Zero-prompt safety.** Day with 0 orchestration_activity rows but session_output and PR data. Assert `lines_per_prompt=null`, `commits_per_prompt=null` (not division by zero).

14. **PR cycle time.** Insert 2 PRs merged on same day: one with 2-hour cycle, one with 4-hour cycle. Assert `avg_pr_cycle_hours=3.0`.

15. **Week aggregation.** Insert data across 5 days. Call `GET /history/leverage?days=7`. Assert `week.active_days=5`, totals sum correctly, ratios computed from totals.

### Part D: Android

16. **Leverage card rendering.** With mock LeverageResponse containing today's `lines_per_prompt=38.2` and `commits=12`, verify cards display "38.2" and "12".

17. **Null handling.** With mock LeverageResponse where `lines_per_prompt=null`, verify card displays "--" not "null" or "0".

18. **Parallel fetch.** Verify ProductivityViewModel fetches orchestration, project-focus, and leverage endpoints concurrently (not sequentially).

### Part E: Artifact Server

19. **Upload success.** POST multipart to `/deploy/office-climate` with a test file. Assert 200 response, file exists at `data/apps/office-climate/latest.apk`, `meta.json` written.

20. **Download.** GET `/apps/office-climate/latest.apk` after upload. Assert file content matches upload.

21. **Auth required for upload.** POST to `/deploy/office-climate` without Bearer token. Assert 401.

22. **No auth for download.** GET `/apps/office-climate/latest.apk` without Bearer token. Assert 200.

23. **Missing app 404.** GET `/apps/nonexistent/latest.apk`. Assert 404.

24. **Size limit.** POST with body exceeding 100 MB. Assert 413.
