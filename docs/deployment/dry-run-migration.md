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

Cloudflare Tunnel credentials stay in the `cloudflared` config and credential file, not in Office Automate templates. The copied Cloudflare config rewrites `credentials-file` to the credential copy under `cloudflared/`, preserving relative subdirectories when present, so rollback rehearsal does not depend on the original host path.

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

The snapshot contains copied config/data inputs, rewritten cloudflared config and tunnel credential file, `restore-env.sh`, and `manifest.json`. SQLite inputs are captured through SQLite's online backup API so committed WAL-mode data is included without copying raw database files out from under the writer. The office climate database is backed up first and schema migration is run only against the backed-up database. The source database is not modified by the snapshot command.

Restore paths intentionally match the Rust runtime data layout. `restore-env.sh` sets `OFFICE_AUTOMATE_DATA_DIR` to the snapshot directory, so the snapshot stores the office database at `office_climate.db`, runtime app artifacts under `apps/`, the legacy APK at `app-debug.apk`, and optional telemetry/tool/Engram databases at their runtime filenames.

`restore-env.sh` exports the effective runtime paths and environment-backed deployment values used during validation, including device credentials when those values were present in the merged config. Keep the snapshot directory private; the file is written with owner-only permissions on Unix.

## Validations

The snapshot command validates:

- Config file readability.
- Rollback output directory writability.
- Office climate DB readability, migration compatibility on the backed-up DB, and SQLite `quick_check`.
- Optional telemetry, project-leverage tool usage, session telemetry tool usage, and Engram SQLite DBs with SQLite online backup plus `quick_check` when present.
- Optional Engram registry and legacy APK readability when present.
- Artifact metadata under the configured artifacts directory, including hash shape and referenced APK files.
- Presence or absence of OAuth, ERV, and HVAC credential material without printing secret values.
- Cloudflare Tunnel config readability, readable `credentials-file`, required tunnel credential JSON fields, at least one ingress rule, and copied tunnel config/credential files in the snapshot.
- Effective restore environment written to `restore-env.sh`.

For an extra cloudflared-native syntax check, run:

```bash
test -r "$CLOUDFLARED_CONFIG"
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

If `cloudflared tunnel ingress validate` is unavailable in the installed version, run `cloudflared tunnel --config "$CLOUDFLARED_CONFIG" ingress validate` or verify the config with the deployed Cloudflare version's equivalent command.

## Rollback Use

Treat the generated snapshot directory as the rollback source for the cutover window. Keep it on local storage that survives process restart and user logout.

For rollback rehearsal, restore from the snapshot into a temporary directory, source the captured environment, and run:

```bash
source /absolute/path/to/restored-snapshot/restore-env.sh
./target/release/office-automate-server migrate --config "$OFFICE_AUTOMATE_CONFIG"
./target/release/office-automate-server collect --config "$OFFICE_AUTOMATE_CONFIG" telemetry --dry-run
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

Do not load launchd services during this ticket. Service bootstrap belongs to the later cutover ticket after the dry-run snapshot and validation output are reviewed.
