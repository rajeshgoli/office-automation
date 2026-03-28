# Productivity Phase 2: Leverage & Deployment

**Classification: Cross-Repo Epic** — 8 sub-tickets across 3 repos. A–E in office-automate, F in session-manager, G in engram, H spans office-automate + Android.

| Ticket | Repo | Dependencies | Scope |
|--------|------|-------------|-------|
| **A: Session-meta pipeline** | office-automate | None | Parser, DB schema, rsync |
| **B: GitHub PR pipeline** | office-automate | None | gh collector, DB schema |
| **C: Leverage endpoints** | office-automate | A, B | `/history/leverage` endpoint |
| **D: Android leverage UI** | office-automate | C | Leverage cards in Productivity tab |
| **E: Artifact server + domain** | office-automate | None | `/deploy/{app}`, `/apps/{app}`, domain rename |
| **F: sm Telegram telemetry** | session-manager | None | Telegram message counters only (sm commands already in tool_usage.db) |
| **G: engram fold telemetry** | engram | None | Fold stats CLI/export |
| **H: Project leverage pipeline + UI** | office-automate | None hard; F optional (Telegram), G optional (convenience) | Collection from tool_usage.db + rsynced engram DB, endpoint, Android cards |

A, B, E, F, G, H can all run in parallel. C depends on A+B. D depends on C. H has no hard dependencies — it reads rsynced engram DB directly (G just adds `--json` convenience). F is optional for H (adds Telegram metrics only — H shows "--" for Telegram without it).

The EM managing this epic must spawn agents in session-manager and engram repos for tickets F and G respectively.

## Context

Phase 1 (PR #22, merged) captured the *input* side of the leverage equation — every human prompt to Claude Code and Codex, timestamped, stored, and visualized in the Productivity tab. The tab shows office sessions, orchestration activity, project focus, and daily/weekly summary cards.

Phase 2 adds the *output* side — what did those prompts produce? — and computes the ratio. The core data sources are Claude's session-meta files (lines added, commits, files modified per session) and GitHub PRs (merged work units). Together with the existing orchestration_activity table, these yield leverage metrics like lines-per-prompt and commits-per-day that answer the question: "am I getting better at orchestrating agents?"

This spec also addresses two operational gaps: a generic artifact server for deploying APKs (not just office-climate, but any app like session-manager), and renaming the domain from `climate.rajeshgo.li` to `office.rajeshgo.li` to reflect that this system is no longer just about climate control.

## Tenets

1. **Measure leverage, not volume.** Ratios always; never celebrate raw line counts. A day with 3 prompts that produced 200 commits has higher leverage than 50 prompts that went in circles.

2. **Prefer session-meta over git log.** Session-meta attributes output to sessions directly — it covers zero-commit work (research, debugging, exploration) and avoids the timestamp-matching complexity of `git log` parsing.

3. **Pipeline is idempotent and upsert-safe.** Re-running the parser on the same data must not create duplicates or lose updates. Session-meta uses `INSERT OR REPLACE` keyed on `session_id`; PRs upsert on `(repo, pr_number)`. Phase 1's orchestration_activity is append-only; Phase 2's sources are mutable.

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

Key fields for leverage: `session_id`, `project_path`, `start_time` (UTC — must convert to local Pacific time), `duration_minutes`, `lines_added`, `lines_removed`, `files_modified`, `git_commits`, `git_pushes`, `user_message_count`, `assistant_message_count`, `input_tokens`, `output_tokens`, `tool_counts` (JSON dict), `languages` (JSON dict), `first_prompt`.

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
4. Convert `createdAt` and `mergedAt` from UTC to local Pacific time (use `astimezone()`, same as session-meta).
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

## Part D: Android UI — Leverage Cards + Projects Tab

Phase 2 adds leverage cards to the Productivity tab and a new **Projects** tab (4th bottom nav) for per-project metrics.

### Navigation change

Add a 4th tab to `AppNavigation.kt`. Current tabs: Dashboard, History, Productivity. New:

```
Dashboard  |  History  |  Productivity  |  Projects
   🏠          📊          📈               🧩
```

- Route: `Routes.PROJECTS = "projects"`
- Icon: `Icons.Filled.Apps` / `Icons.Outlined.Apps` (or `GridView`)
- New screen: `ProjectsScreen.kt` in `ui/projects/`
- New ViewModel: `ProjectsViewModel.kt`

### Productivity tab additions

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

### Projects tab — screen layout

The Projects tab is a vertically scrollable screen with one **project card** per active project. Each card is a self-contained section showing that project's health and usage metrics.

```
┌──────────────────────────────────────┐
│  PROJECTS                            │
│                                      │
│  ┌──────────────────────────────────┐│
│  │ 🟢 SESSION-MANAGER              ││
│  │ Highest leverage tool            ││
│  │                                  ││
│  │  52        180       95          ││
│  │  dispatches sends    telegram    ││
│  │                                  ││
│  │  ▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪▪  7d spark││
│  │  599 spawns · 145 reminds        ││
│  └──────────────────────────────────┘│
│                                      │
│  ┌──────────────────────────────────┐│
│  │ 🟣 ENGRAM                       ││
│  │ Knowledge maintenance            ││
│  │                                  ││
│  │  3.5h          42       12       ││
│  │  since fold    concepts folds/7d ││
│  │                                  ││
│  │  ● Fold status: FRESH            ││
│  └──────────────────────────────────┘│
│                                      │
│  ┌──────────────────────────────────┐│
│  │ 🔵 AGENT-OS                     ││
│  │ Workflow system                   ││
│  │                                  ││
│  │  28            4                 ││
│  │  persona reads projects          ││
│  │                                  ││
│  │  Top: engineer (12) reviewer (8) ││
│  └──────────────────────────────────┘│
│                                      │
│  ┌──────────────────────────────────┐│
│  │ 🟢 OFFICE-AUTOMATE              ││
│  │ Climate & productivity            ││
│  │                                  ││
│  │  45            12                ││
│  │  automations   transitions       ││
│  └──────────────────────────────────┘│
│                                      │
│  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ │
│  Dashboard | History | Prod | Projects│
└──────────────────────────────────────┘
```

### Project card anatomy

Each card follows a consistent structure:

```
┌──────────────────────────────────────┐
│ [color dot] PROJECT-NAME             │  ← Header: project color + name
│ One-line description                 │  ← Subtitle: from server `summary` field
│                                      │
│  VALUE1     VALUE2     VALUE3        │  ← Metric row: 2-3 hero numbers
│  label1     label2     label3        │     in large font with small labels
│                                      │
│  Secondary detail line               │  ← Footer: additional context
└──────────────────────────────────────┘
```

- **Background**: `Surface` color (0xFF1A1A1E), same as existing `StatTile`
- **Project color dot**: Uses `KnownProjectColors` map from `ProductivityScreen.kt`
- **Hero numbers**: Large (20sp), bold, accent-colored
- **Labels**: Small (10sp), `TextSecondary`
- **Footer**: Small (11sp), `TextSecondary`, optional

### Per-project card content

**session-manager:**
- Hero metrics: dispatches (week), sends (week), Telegram in (week)
- Footer: "{spawns} spawns · {reminds} reminds this week"
- If Telegram data unavailable (Part F not yet shipped), show "--" for Telegram and omit from footer

**engram:**
- Hero metrics: hours since fold, active concepts, folds in last 7 days
- Footer: fold freshness indicator — "FRESH" (< 12h), "STALE" (12-48h), "OUTDATED" (> 48h)
- Color-code the freshness: Emerald / Amber / Red

**agent-os:**
- Hero metrics: persona reads (week), distinct projects using personas
- Footer: "Top: {persona1} ({count}), {persona2} ({count})" — most-read personas

**office-automate:**
- Hero metrics: automation events (week), state transitions (week)
- Footer: none needed (this is the host app)

### Project card ordering

Cards are ordered by activity — most active project first. Activity = total metric values for the week. Projects with zero activity in the last 7 days are hidden (not shown as empty cards).

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
2. Android app: Update default server URL in `android/.../util/Constants.kt:10` (`SERVER_URL` constant). Also update the placeholder URL shown in `android/.../ui/settings/SettingsScreen.kt:89`.
3. `occupancy_detector.py`: Default URL is `http://localhost:8080` (line 308-309), not the public domain. No change needed — the production Launch Agent overrides with `--url http://192.168.5.140:8080`. But update `CLAUDE.md` examples that reference the public URL.
4. `CLAUDE.md`: Update all domain references — both `climate.rajeshgo.li` and `climate.loca.lt` — to `office.rajeshgo.li`. LocalTunnel is dead and no longer in use; remove LocalTunnel setup instructions and references entirely.
5. OAuth redirect URIs in Google Cloud Console: Add `https://office.rajeshgo.li/auth/callback`.
6. Keep `climate.rajeshgo.li` as a redirect to `office.rajeshgo.li` for a transition period (Cloudflare Page Rule or second tunnel route).

**Artifact subdomain (optional, deferred):** `apks.rajeshgo.li` could point to the same tunnel with a path prefix, but it's simpler to serve artifacts from `office.rajeshgo.li/apps/` for now. Revisit if the artifact server grows.

---

## Part F: session-manager Command Telemetry (session-manager repo — minimal)

The highest-leverage tool. Enables parallel agent orchestration and mobile productivity.

### What already exists — tool_usage.db

Every `sm` command executed by an agent via Bash is **already captured** in `tool_usage.db` (`~/.local/share/claude-sessions/tool_usage.db`). The `bash_command` column contains the full command text. Current totals:

| Command | Count (illustrative, as of 2026-03-28) | How to query |
|---------|-------|-------------|
| `sm send` | ~4,600 | `bash_command LIKE 'sm send%'` |
| `sm dispatch` | ~1,600 | `bash_command LIKE 'sm dispatch%'` |
| `sm spawn` | ~600 | `bash_command LIKE 'sm spawn%'` |
| `sm wait` | ~1,100 | `bash_command LIKE 'sm wait%'` |
| `sm remind` | ~150 | `bash_command LIKE 'sm remind%'` |
| `sm name` | ~700 | `bash_command LIKE 'sm name%'` |
| Other sm | ~8,200 | status, what, clear, output, inbox, etc. |

These counts grow continuously. Daily breakdown shows clear patterns (e.g., 2026-03-25: 182 sends, 116 spawns, 628 total).

**No new instrumentation needed for agent-side sm metrics.** The collection script (Part H) queries `tool_usage.db` directly.

**Additional dimensions already in tool_usage.db:**

- **Sender → receiver**: `session_name` contains the sender's role (e.g., "em-epics", "spec-owner-taskbar", "2361-engineer"). The target is the first argument of `sm send {target}` in `bash_command`. **Caveat:** targets are not always session IDs — session-manager's `resolve_session_id()` accepts IDs, aliases, and friendly names (e.g., "scout-coordinator", "sessionmgr"). Approximately 20% of `sm send` commands use non-ID targets. For the communication graph, resolve what's possible (8-char hex IDs cross-reference directly with other `session_id` values) and bucket the rest as "unresolved." This gives a partial but useful view of role-to-role communication patterns.

- **Claude vs Codex**: `tool_usage.db` doesn't have a `provider` column. Session-manager's `sessions.json` tracks `provider` per session (claude / codex / codex-fork / codex-app), but it's a live state file — currently only 3 sessions — not a historical ledger. The 516+ distinct session IDs in `tool_usage.db` cannot be retroactively resolved. **To make this work historically**, either: (a) add a `provider` column to `tool_usage` in a future session-manager update (preferred — instrument once, always available), or (b) snapshot `sessions.json` periodically and build a lookup table. For now, mark Claude vs Codex as a **future dimension** pending instrumentation.

### What's NOT in tool_usage.db

**Telegram messages.** When Rajesh sends a message from his phone via Telegram, it hits the sm server's `_handle_message()` in `telegram_bot.py:1194` — but that's a direct HTTP call to the Telegram bot, not a Claude Code Bash command. These are the highest-leverage signals (mobile productivity while away from desk).

**Session lifecycle.** Sessions are created via `create_session()` in `session_manager.py:1845` and tracked in `sessions.json`, but daily session counts aren't persisted in a queryable form.

### New instrumentation (Telegram + sessions only)

Add a `telegram_telemetry` table to the **same SQLite database** as `tool_usage.db` (at `~/.local/share/claude-sessions/tool_usage.db`):

```sql
CREATE TABLE IF NOT EXISTS telegram_telemetry (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
    direction TEXT NOT NULL CHECK(direction IN ('in', 'out')),
    session_id TEXT,
    chat_id TEXT,
    result TEXT
);
CREATE INDEX IF NOT EXISTS idx_tg_timestamp ON telegram_telemetry(timestamp);
```

**Instrument two places:**
1. `_handle_message()` in `telegram_bot.py:1194` — INSERT with `direction='in'`, `result` = DELIVERED/QUEUED/FAILED
2. Wherever the bot sends outbound notifications — INSERT with `direction='out'`

Fire-and-forget async, same pattern as `tool_logger.py`.

For **session lifecycle**, add a daily snapshot to `sessions.json` processing — or simply count distinct `session_id` values from `tool_usage.db` per day (agents only run inside sessions, so this is a reasonable proxy).

### Stats endpoint (optional)

If co-located querying is easier than rsync, add `GET /stats/commands?days=7` to the sm server that queries `tool_usage.db` + `telegram_telemetry` and returns aggregated counts. But this is optional — the collection script can query the rsynced DBs directly.

---

## Part G: engram Fold Telemetry (engram repo)

### What exists today

Engram runs as a foreground CLI service (`engram run`). State lives in `.engram/engram.db` (SQLite) with these tables:

- **`dispatches`**: Fold lifecycle — `created_at`, `updated_at`, `state` (building → dispatched → validated → committed)
- **`buffer_items`**: Pending context (path, type, chars, date, drift_type)
- **`id_counters`**: Next available ID per category (C, E, W concepts)
- **`server_state`**: Singleton with poll bookmarks, `fold_from` marker, L0 stale flag

The concept registry lives in a markdown file with stable IDs (`C001`, `C002`...) and states (ACTIVE, DEAD, EVOLVED).

There is **no HTTP API** — only CLI queries via `engram status`.

### New instrumentation

Add an `engram stats` CLI command that outputs JSON (machine-readable counterpart to `engram status`):

```json
{
  "last_fold_at": "2026-03-27T14:30:00",
  "last_fold_age_hours": 3.5,
  "folds_last_7d": 12,
  "folds_last_30d": 45,
  "active_concepts": 42,
  "dead_concepts": 8,
  "buffer_fill_pct": 35,
  "buffer_items": 28
}
```

**Implementation:** Fold timestamps come from `dispatches` table (WHERE state = 'committed'). Buffer state from `server_state` table. **Concept counts** must be derived from the project's concept registry markdown file (parsing `## C{NNN}: ... (ACTIVE` vs `(DEAD` headers) — they are NOT in the SQLite tables. The `id_counters` table only has the next-available ID, not active/dead counts.

Also add `--json` flag to the existing `engram status` command as an alternative entry point.

### Collection

**Important:** Engram's `.engram/engram.db` lives at `<project_root>/.engram/engram.db`, not in the engram repo itself. The live instance is at `~/Desktop/fractal-market-simulator/.engram/engram.db` (since engram is currently watching fractal). If engram watches multiple repos, there will be multiple DBs.

The office-automate Mac Mini rsyncs the engram DB from the watched project:

```
rsync -az rajesh@<work-mac-ip>:~/Desktop/fractal-market-simulator/.engram/engram.db ~/office-automate/data/engram_state.db
```

For concept counts, also rsync the concept registry markdown:

```
rsync -az rajesh@<work-mac-ip>:~/Desktop/fractal-market-simulator/docs/decisions/concept_registry.md ~/office-automate/data/engram_concept_registry.md
```

The collection script parses `## C{NNN}:` headers from the markdown to count ACTIVE vs DEAD concepts. If engram watches additional repos in the future, add their DBs and registries to the rsync list.

---

## Part H: Project Leverage Pipeline + UI (office-automate repo)

This ticket collects Tier 3 signals from across repos and surfaces them in the Productivity tab. Depends on F (Telegram telemetry only) and G (engram stats). Most sm data is already in `tool_usage.db`.

### Data sources (4 arms)

| Source | Location | Collection method | New instrumentation? |
|--------|----------|-------------------|---------------------|
| **sm commands** | `tool_usage.db` (`~/.local/share/claude-sessions/`) | rsync from work Mac, query `bash_command LIKE 'sm %'` | **No** — already tracked |
| **sm Telegram** | `telegram_telemetry` table (Part F) | rsync or query sm server | **Yes** — Part F |
| **agent-os persona reads** | `tool_usage.db` | Same rsync, query `target_file LIKE '%agent-os/personas/%'` | **No** — already tracked |
| **engram fold state** | `<project>/.engram/engram.db` (live: `fractal-market-simulator/.engram/`) | rsync DB + concept registry markdown from work Mac | **No** — already exists (Part G adds `--json` convenience) |
| **office-automate automation** | Local `climate_actions` + `occupancy_log` tables | Direct query (same DB) | **No** — already exists |

### tool_usage.db queries for agent-os

The `tool_usage` table (schema in `session-manager/src/tool_logger.py:93-138`) has `target_file` populated for Read/Write/Edit operations. Query:

```sql
-- Daily persona reads
SELECT date(timestamp) AS date,
       target_file,
       COUNT(*) AS reads
FROM tool_usage
WHERE tool_name = 'Read'
  AND target_file LIKE '%agent-os/personas/%'
  AND timestamp >= ?
GROUP BY date(timestamp), target_file
ORDER BY date ASC;

-- Per-project adoption
SELECT project_name, COUNT(DISTINCT session_id) AS sessions_using_personas
FROM tool_usage
WHERE tool_name = 'Read'
  AND target_file LIKE '%agent-os/personas/%'
  AND timestamp >= ?
GROUP BY project_name;
```

### tool_usage.db queries for sm commands

```sql
-- Daily sm command breakdown
SELECT date(timestamp) AS date,
  SUM(CASE WHEN bash_command LIKE 'sm send%' THEN 1 ELSE 0 END) AS sends,
  SUM(CASE WHEN bash_command LIKE 'sm dispatch%' THEN 1 ELSE 0 END) AS dispatches,
  SUM(CASE WHEN bash_command LIKE 'sm spawn%' THEN 1 ELSE 0 END) AS spawns,
  SUM(CASE WHEN bash_command LIKE 'sm remind%' THEN 1 ELSE 0 END) AS reminds,
  SUM(CASE WHEN bash_command LIKE 'sm wait%' THEN 1 ELSE 0 END) AS waits,
  COUNT(*) AS total_sm
FROM tool_usage
WHERE tool_name = 'Bash'
  AND bash_command LIKE 'sm %'
  AND hook_type = 'PreToolUse'
  AND timestamp >= ?
GROUP BY date(timestamp)
ORDER BY date ASC;

-- Daily active sessions (distinct session_id with any tool use that day)
-- Note: this is sessions *touched*, not sessions *created* — creation
-- time is not in tool_usage.db. This is a reasonable proxy for daily
-- orchestration volume.
SELECT date(timestamp) AS date,
       COUNT(DISTINCT session_id) AS active_sessions
FROM tool_usage
WHERE hook_type = 'PreToolUse'
  AND timestamp >= ?
GROUP BY date(timestamp);
```

### office-automate automation query

```sql
-- Daily automation events
SELECT date(timestamp) AS date, COUNT(*) AS actions
FROM climate_actions
WHERE timestamp >= ?
GROUP BY date(timestamp);

-- Daily state transitions
SELECT date(timestamp) AS date, COUNT(*) AS transitions
FROM occupancy_log
WHERE timestamp >= ?
GROUP BY date(timestamp);
```

### Database schema

Add to `src/database.py`:

```sql
CREATE TABLE IF NOT EXISTS project_leverage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    date TEXT NOT NULL,
    project TEXT NOT NULL,
    metric TEXT NOT NULL,
    value REAL NOT NULL DEFAULT 0,
    UNIQUE(date, project, metric)
);
CREATE INDEX IF NOT EXISTS idx_proj_lev_date ON project_leverage(date);
```

Metrics stored as `(date, project, metric_name, value)` tuples. This is a flexible EAV schema that accommodates different metrics per project without schema changes.

**Metric names:**

| Project | Metric | Description |
|---------|--------|-------------|
| session-manager | `sm_dispatches` | dispatch commands that day |
| session-manager | `sm_sends` | send commands |
| session-manager | `sm_reminds` | remind commands |
| session-manager | `sm_active_sessions` | distinct sessions with tool activity that day |
| session-manager | `sm_telegram_in` | inbound Telegram messages |
| session-manager | `sm_telegram_out` | outbound Telegram messages |
| engram | `engram_last_fold_age_hours` | hours since last committed fold |
| engram | `engram_folds_7d` | folds in last 7 days |
| engram | `engram_active_concepts` | count of ACTIVE concepts |
| agent-os | `persona_reads` | total reads of persona files |
| agent-os | `persona_projects` | distinct projects using personas |
| office-automate | `automation_events` | climate actions fired |
| office-automate | `state_transitions` | occupancy state changes |

### Collection script

Extend `session_stats_parser.py` (or create `project_leverage_collector.py`) to run on the 30-min cron alongside the existing history parser. For each source:

1. **tool_usage.db** (sm commands + agent-os personas): Open the rsynced copy at `data/tool_usage.db`. Run sm command queries (below) and agent-os persona queries (above).
2. **Telegram telemetry**: Query `telegram_telemetry` table in the same rsynced `data/tool_usage.db` (Part F adds this table to the same DB). If the table doesn't exist (Part F not yet shipped), skip — Telegram is the only metric requiring Part F.
3. **engram**: Open the rsynced copy at `data/engram_state.db`. Query `SELECT created_at, state FROM dispatches WHERE state = 'committed' ORDER BY created_at DESC`.
4. **office-automate**: Query local `climate_actions` and `occupancy_log` tables directly.

Upsert all results into `project_leverage` by `(date, project, metric)`.

### Sync additions

Add to the existing rsync cron:

```
rsync -az rajesh@<work-mac-ip>:~/.local/share/claude-sessions/tool_usage.db ~/office-automate/data/tool_usage.db
rsync -az rajesh@<work-mac-ip>:~/Desktop/fractal-market-simulator/.engram/engram.db ~/office-automate/data/engram_state.db
rsync -az rajesh@<work-mac-ip>:~/Desktop/fractal-market-simulator/docs/decisions/concept_registry.md ~/office-automate/data/engram_concept_registry.md
```

### API endpoint

**`GET /history/project-leverage?days=7`**

```json
{
  "ok": true,
  "projects": {
    "session-manager": {
      "summary": "Highest leverage — 52 dispatches, 95 Telegram messages this week",
      "days": [
        {
          "date": "2026-03-27",
          "sm_dispatches": 12,
          "sm_sends": 45,
          "sm_reminds": 8,
          "sm_active_sessions": 18,
          "sm_telegram_in": 23,
          "sm_telegram_out": 19
        }
      ],
      "week": {"sm_dispatches": 52, "sm_sends": 180, "sm_telegram_in": 95}
    },
    "engram": {
      "summary": "Last fold 3.5h ago, 42 active concepts",
      "days": [...],
      "current": {
        "last_fold_age_hours": 3.5,
        "active_concepts": 42,
        "folds_7d": 12
      }
    },
    "agent-os": {
      "summary": "28 persona reads across 4 projects this week",
      "days": [...],
      "week": {"persona_reads": 28, "persona_projects": 4}
    },
    "office-automate": {
      "summary": "45 automation events, 12 state transitions this week",
      "days": [...],
      "week": {"automation_events": 45, "state_transitions": 12}
    }
  }
}
```

The `summary` field is a human-readable one-liner generated server-side from the metrics — displayed as a subtitle on the Android card.

### Android UI

Project cards live in the **Projects tab** (`ProjectsScreen.kt`), not in `ProductivityScreen.kt`. See Part D for the full UI spec including wireframes, card anatomy, and per-project content.

**Note on shared code:** The `KnownProjectColors` map is currently `private` in `ProductivityScreen.kt:65`. Move it to a shared location (e.g., `ui/theme/ProjectColors.kt`) so both `ProductivityScreen` and `ProjectsScreen` can reference it.

### Taskbar (deferred)

Taskbar metrics (window switch counts, uptime) require AppKit instrumentation that doesn't exist. Not included in this epic. Can be added when taskbar gains telemetry.

---

## Out of Scope

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

2. **Idempotency.** Run parser twice on the same 3 files. Assert row count is still 2 (INSERT OR REPLACE on session_id PK — second run replaces, not duplicates).

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

### Part F: Telegram Telemetry

25. **Telegram inbound logged.** Simulate `_handle_message()` with a test message. Assert a row appears in `telegram_telemetry` with `direction='in'` and correct `result`.

26. **Telegram outbound logged.** Simulate bot sending a notification. Assert a row with `direction='out'`.

### Part G: engram Stats

27. **engram stats output.** Insert 3 dispatches into a test `engram.db` (2 committed, 1 building). Run the stats query. Assert `folds_last_7d=2`, `last_fold_age_hours` is reasonable, `buffer_items` matches.

28. **No folds.** Empty dispatches table. Assert `last_fold_at=null`, `folds_last_7d=0`.

### Part H: Project Leverage Pipeline

29. **sm command collection.** Insert 5 `sm send` and 3 `sm dispatch` Bash rows into a test `tool_usage.db`. Run collector. Assert `project_leverage` has `sm_sends=5`, `sm_dispatches=3` for that date.

30. **agent-os persona reads.** Insert 4 Read rows with `target_file` containing `agent-os/personas/engineer.md` across 2 projects. Run collector. Assert `persona_reads=4`, `persona_projects=2`.

31. **engram fold collection.** Insert a committed dispatch 2 hours ago into test engram DB. Run collector. Assert `engram_last_fold_age_hours` ≈ 2.

32. **Project leverage endpoint.** Populate `project_leverage` with test data. Call `GET /history/project-leverage?days=7`. Assert response has all 4 project sections with correct metrics.

33. **Android project cards.** With mock project-leverage response, verify one card per project renders with correct title and values.
