# Legacy Service Decommission

Ticket #80 removes the old operational sync path after the primary-host Rust stack has passed cutover, rollback validation, and the rollback window in `docs/working/62_primary_host_modern_stack.md`.

Do not run this before:

- PR #76 snapshot output exists and has been copied to durable backup storage.
- PR #78 backend/MQTT cutover validation passed with Rust as the active controller.
- PR #79 rollback validation passed and the rollback window has expired.
- Primary-host launchd jobs are the only intended production services:
  - `com.office-automate.server`
  - `com.office-automate.telemetry`
  - `com.office-automate.project-leverage`
  - `com.office-automate.tunnel`

## Repo Cleanup

Legacy operational sync assets have been removed from the repo:

- `scripts/sync_session_history.sh`
- `scripts/launchd/com.office-automate.collect.plist`
- `scripts/launchd/com.office-automate.telemetry.plist`

The deleted collector plist installed as `com.office-automate.collect`; include that
actual label in decommission commands unless the host already renamed it locally.
The deleted telemetry plist used the same `com.office-automate.telemetry` label as
the primary-host telemetry job, so verify the installed telemetry plist points at
the Rust primary-host collector before treating that label as production.

The remaining sync helper is backup-only:

```bash
scripts/sync_rollback_snapshot.sh \
  "$OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR" \
  "$OFFICE_AUTOMATE_BACKUP_DIR"
```

Do not replace these with rsync jobs that shuttle live telemetry, project-leverage, or controller state between hosts. The Rust primary host owns collectors locally through the launchd templates in `scripts/launchd/primary-host/`.

## Launchd Decommission

Prepare a durable backup directory:

```bash
export OFFICE_AUTOMATE_DECOMMISSION_BACKUP_DIR="$OFFICE_AUTOMATE_BACKUP_DIR/decommission"
```

Run a dry run with the installed legacy plist paths. Include only legacy backend, presence, collector, broker, and tunnel plists that should be retired:

```bash
scripts/decommission_legacy_launchd.sh \
  --backup-dir "$OFFICE_AUTOMATE_DECOMMISSION_BACKUP_DIR" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-server.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-presence.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.collect.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-broker.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-tunnel.plist"
```

If a host renamed the old collector plist during cutover, substitute the local path
and verify its `Label` is the legacy collector label you intend to retire.

After reviewing the dry run, execute:

```bash
scripts/decommission_legacy_launchd.sh \
  --execute \
  --backup-dir "$OFFICE_AUTOMATE_DECOMMISSION_BACKUP_DIR" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-server.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-presence.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.collect.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-broker.plist" \
  "$HOME/Library/LaunchAgents/com.office-automate.legacy-tunnel.plist"
```

The script copies plist files and existing stdout/stderr logs into a timestamped backup directory before `launchctl bootout`, then renames each original plist with a `.disabled-YYYYMMDD-HHMMSS` suffix. This keeps decommissioning reversible until final cleanup is confirmed.

## Verification

Verify retired legacy services are absent:

```bash
launchctl print "gui/$(id -u)/com.office-automate.legacy-server"
launchctl print "gui/$(id -u)/com.office-automate.legacy-presence"
launchctl print "gui/$(id -u)/com.office-automate.collect"
launchctl print "gui/$(id -u)/com.office-automate.legacy-broker"
launchctl print "gui/$(id -u)/com.office-automate.legacy-tunnel"
```

Each command should fail with a missing-service result.

Then verify primary-host services remain loaded:

```bash
launchctl print "gui/$(id -u)/com.office-automate.server"
launchctl print "gui/$(id -u)/com.office-automate.telemetry"
launchctl print "gui/$(id -u)/com.office-automate.project-leverage"
launchctl print "gui/$(id -u)/com.office-automate.tunnel"
```

## Issue Update Template

Use this as the final issue update after the real decommission:

```markdown
## [Engineer]
Legacy decommission completed after rollback window.

- Disabled services:
  - com.office-automate.legacy-server
  - com.office-automate.legacy-presence
  - com.office-automate.collect
  - com.office-automate.legacy-broker
  - com.office-automate.legacy-tunnel
- Backup directory: <absolute backup path>
- Rollback snapshot backup: <absolute snapshot backup path>
- Logs copied to: <absolute log backup path>
- Primary-host services verified:
  - com.office-automate.server
  - com.office-automate.telemetry
  - com.office-automate.project-leverage
  - com.office-automate.tunnel
```
