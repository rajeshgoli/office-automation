# Rollback Validation

Ticket #79 validates rollback from Rust active control back to the legacy backend described in `docs/working/62_primary_host_modern_stack.md`.

Use this only after a backend/MQTT cutover has started or completed. The goal is to prove the legacy controller is again the only active climate-control path.

## Inputs

Set deployment-specific values outside the repo:

```bash
export OFFICE_AUTOMATE_CONFIG="/absolute/path/to/office-automate.yaml"
export OFFICE_AUTOMATE_LEGACY_BASE_URL="http://legacy-host:9001"
export OFFICE_AUTOMATE_LEGACY_PUBLIC_URL="https://office.example.com"
export OFFICE_AUTOMATE_CUTOVER_BASE_URL="http://127.0.0.1:9001"
export OFFICE_AUTOMATE_RUST_PUBLIC_URL="https://rust-office.example.com"
export OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR="/absolute/path/to/office-automate-precutover-YYYYMMDD-HHMMSS"
export OFFICE_AUTOMATE_ROLLBACK_LOG="/absolute/path/to/rollback-log.md"
```

For MQTT rollback state, choose the value that matches the rollback:

- `not-moved`: Qingping never moved off the legacy-compatible MQTT path.
- `repointed-legacy`: Qingping device was repointed to the legacy broker.

For snapshot restore verification, choose one:

- `restored-from-snapshot`: copied state was restored from the pre-cutover snapshot.
- `verified-safe-no-restore`: Rust-written state was reviewed and no restore was required.

## Command Sequence

Stop the Rust backend and primary-host Cloudflare Tunnel:

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist"
export OFFICE_AUTOMATE_RUST_STOPPED_AT="$(date -Iseconds)"
```

Start the legacy backend and legacy Cloudflare Tunnel:

```bash
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.legacy-server.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.legacy-tunnel.plist"
export OFFICE_AUTOMATE_LEGACY_STARTED_AT="$(date -Iseconds)"
```

Return Qingping to a legacy-compatible feed path if it moved during cutover, then set:

```bash
export OFFICE_AUTOMATE_MQTT_ROLLBACK_STATE="repointed-legacy"
```

Restore data from the pre-cutover snapshot if Rust wrote incompatible state. If no restore is needed after inspection, record that explicitly:

```bash
export OFFICE_AUTOMATE_RESTORE_VERIFICATION="restored-from-snapshot"
```

Use `verified-safe-no-restore` only after confirming the legacy backend can read the current state safely.

## Validation Command

Run rollback validation after the legacy backend and legacy Cloudflare Tunnel are active:

```bash
./target/release/office-automate-server validate \
  --config "$OFFICE_AUTOMATE_CONFIG" \
  rollback \
  --legacy-base-url "$OFFICE_AUTOMATE_LEGACY_BASE_URL" \
  --legacy-public-url "$OFFICE_AUTOMATE_LEGACY_PUBLIC_URL" \
  --rust-base-url "$OFFICE_AUTOMATE_CUTOVER_BASE_URL" \
  --rust-public-url "$OFFICE_AUTOMATE_RUST_PUBLIC_URL" \
  --rust-stopped-at "$OFFICE_AUTOMATE_RUST_STOPPED_AT" \
  --legacy-started-at "$OFFICE_AUTOMATE_LEGACY_STARTED_AT" \
  --mqtt-rollback-state "$OFFICE_AUTOMATE_MQTT_ROLLBACK_STATE" \
  --snapshot-dir "$OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR" \
  --restore-verification "$OFFICE_AUTOMATE_RESTORE_VERIFICATION" \
  --rollback-log "$OFFICE_AUTOMATE_ROLLBACK_LOG" \
  --max-air-quality-age-seconds 300
```

If the OAuth config cannot mint a validation JWT, complete browser/PWA and mobile checks against the legacy public URL, then add:

```bash
--manual-legacy-public-verified-at "$(date -Iseconds)"
```

The command validates:

- Rust ERV and HVAC active-control flags are disabled.
- The pre-cutover rollback snapshot manifest exists.
- Rust local/public `/status` probes no longer respond when URLs are supplied.
- The Qingping rollback state is recorded against the configured device MAC.
- The snapshot restore path was tested or explicitly verified safe.
- Legacy `/status` has the compatibility shape, fresh air-quality data, `safety_interlock`, and healthy ERV local-key state.
- Legacy `/ws` returns the authenticated initial status frame.
- Legacy public Cloudflare access reaches OAuth login and fresh `/status`, or manual browser/mobile verification is recorded.
- A rollback log is written with timestamps, checks, MQTT state, and restore decision.

Do not decommission the primary-host Rust deployment during this ticket. Keep Rust active-control flags disabled before any later shadow-mode follow-up.
