# Dry-Run Migration And Rollback Snapshot

Ticket #76 adds a no-cutover migration rehearsal for the Rust primary-host deployment in `docs/working/62_primary_host_modern_stack.md`.

This procedure does not start launchd jobs, does not run the HTTP server, and does not enable active ERV/HVAC writes. It validates configured data inputs and creates a rollback snapshot directory.

## Inputs

Set deployment-specific paths outside the repo:

```bash
export OFFICE_AUTOMATE_CONFIG="/absolute/path/to/office-automate.yaml"
export OFFICE_AUTOMATE_SNAPSHOT_DIR="/absolute/path/to/rollback-snapshots"
export CLOUDFLARED_CONFIG="/absolute/path/to/cloudflared/config.yml"
```

The Office Automate config should point at the production candidate data files:

- `runtime.database_path`
- telemetry DB path
- project-leverage tool usage DB path
- session telemetry tool usage DB path, when it differs from the project-leverage tool usage DB
- Engram DB and registry paths
- artifacts directory
- legacy APK path if still used
- OAuth, ERV, and HVAC credential material

Cloudflare Tunnel credentials stay in the `cloudflared` config and credential file, not in Office Automate templates. If the Cloudflare config uses a relative `credentials-file` path with subdirectories, the snapshot preserves that same relative path under `cloudflared/` so the copied config can be used for rollback rehearsal.

## Snapshot Command

Build the Rust binary, then run:

```bash
cargo build --manifest-path rust/office-automate-server/Cargo.toml --release
./target/release/office-automate-server snapshot \
  --config "$OFFICE_AUTOMATE_CONFIG" \
  --output-dir "$OFFICE_AUTOMATE_SNAPSHOT_DIR" \
  --cloudflared-config "$CLOUDFLARED_CONFIG"
```

The command creates:

```text
$OFFICE_AUTOMATE_SNAPSHOT_DIR/office-automate-precutover-YYYYMMDD-HHMMSS/
```

The snapshot contains copied config/data inputs, cloudflared config and tunnel credential file, plus `manifest.json`. The office climate database is copied first and schema migration is run only against the copied database. The source database is not modified by the snapshot command.

## Validations

The snapshot command validates:

- Config file readability.
- Rollback output directory writability.
- Office climate DB readability, migration compatibility on the copied DB, and SQLite `quick_check`.
- Optional telemetry, project-leverage tool usage, session telemetry tool usage, and Engram SQLite DBs with SQLite `quick_check` when present.
- Optional Engram registry and legacy APK readability when present.
- Artifact metadata under the configured artifacts directory, including hash shape and referenced APK files.
- Presence or absence of OAuth, ERV, and HVAC credential material without printing secret values.
- Cloudflare Tunnel config readability, readable `credentials-file`, required tunnel credential JSON fields, at least one ingress rule, and copied tunnel config/credential files in the snapshot.

For an extra cloudflared-native syntax check, run:

```bash
test -r "$CLOUDFLARED_CONFIG"
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

If `cloudflared tunnel ingress validate` is unavailable in the installed version, run `cloudflared tunnel --config "$CLOUDFLARED_CONFIG" ingress validate` or verify the config with the deployed Cloudflare version's equivalent command.

## Rollback Use

Treat the generated snapshot directory as the rollback source for the cutover window. Keep it on local storage that survives process restart and user logout.

For rollback rehearsal, restore from the snapshot into a temporary directory, point a separate config at those restored files, and run:

```bash
./target/release/office-automate-server migrate --config /absolute/path/to/restored-test-config.yaml
./target/release/office-automate-server collect --config /absolute/path/to/restored-test-config.yaml telemetry --dry-run
```

Do not load launchd services during this ticket. Service bootstrap belongs to the later cutover ticket after the dry-run snapshot and validation output are reviewed.
