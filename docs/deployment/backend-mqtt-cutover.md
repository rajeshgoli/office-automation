# Backend/MQTT Cutover

Ticket #78 executes the backend and Qingping MQTT cutover described in `docs/working/62_primary_host_modern_stack.md`.

This procedure is for the cutover window only. Do not leave Python and Rust running with active climate control at the same time.

## Preconditions

- Ticket #76 snapshot completed and produced a rollback snapshot directory with `manifest.json`.
- Ticket #77 shadow validation completed against the Rust backend with Python still active.
- ERV and HVAC active write gates were validated by their earlier tickets.
- Cloudflare Tunnel config was validated with the deployed `cloudflared` binary.
- Browser/PWA and mobile OAuth behavior was manually checked against the public Cloudflare URL when automated JWT validation is unavailable.

Cloudflare Tunnel is the public transport. LocalTunnel is not part of this cutover.

## Inputs

Set deployment-specific values outside the repo:

```bash
export OFFICE_AUTOMATE_CONFIG="/absolute/path/to/office-automate.yaml"
export OFFICE_AUTOMATE_CUTOVER_BASE_URL="http://127.0.0.1:9001"
export OFFICE_AUTOMATE_PUBLIC_URL="https://office.example.com"
export OFFICE_AUTOMATE_LEGACY_BASE_URL="http://legacy-host:9001"
export OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR="/absolute/path/to/office-automate-precutover-YYYYMMDD-HHMMSS"
export OFFICE_AUTOMATE_CUTOVER_LOG="/absolute/path/to/cutover-log.md"
export OFFICE_AUTOMATE_MQTT_CUTOVER_STRATEGY="atomic-switch"
```

Use `bridge-mirror` instead of `atomic-switch` only when the active climate controller continues receiving mirrored fresh Qingping readings for the whole transition.

The Rust config used for the active cutover must have:

```yaml
erv:
  active_control_enabled: true

mitsubishi:
  active_control_enabled: true
```

Do not run `office-automate-server serve` with those flags while the Python active controller is still running.

## Cutover Sequence

Build and run final preflight checks:

```bash
cargo build --manifest-path rust/office-automate-server/Cargo.toml --release
./target/release/office-automate-server migrate --config "$OFFICE_AUTOMATE_CONFIG"
./target/release/office-automate-server smoke --config "$OFFICE_AUTOMATE_CONFIG"
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

Stop the legacy Python backend and record the timestamp:

```bash
export OFFICE_AUTOMATE_LEGACY_STOPPED_AT="$(date -Iseconds)"
```

Apply the selected MQTT feed strategy:

- `bridge-mirror`: keep Qingping publishing to the current active path while bridge/mirror forwarding proves Rust receives the same fresh `qingping/{DEVICE_MAC}/up` reports.
- `atomic-switch`: move Qingping to the Rust embedded broker in the same window that Python active control stops and Rust active control starts.

Start the Rust backend and Cloudflare Tunnel services with launchd or the equivalent foreground commands:

```bash
./target/release/office-automate-server serve --config "$OFFICE_AUTOMATE_CONFIG"
cloudflared tunnel --config "$CLOUDFLARED_CONFIG" run "$CLOUDFLARED_TUNNEL"
```

## Validation Command

Run cutover validation after Rust is the only active climate controller:

```bash
./target/release/office-automate-server validate \
  --config "$OFFICE_AUTOMATE_CONFIG" \
  cutover \
  --base-url "$OFFICE_AUTOMATE_CUTOVER_BASE_URL" \
  --public-url "$OFFICE_AUTOMATE_PUBLIC_URL" \
  --legacy-base-url "$OFFICE_AUTOMATE_LEGACY_BASE_URL" \
  --legacy-controller-stopped-at "$OFFICE_AUTOMATE_LEGACY_STOPPED_AT" \
  --mqtt-strategy "$OFFICE_AUTOMATE_MQTT_CUTOVER_STRATEGY" \
  --snapshot-dir "$OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR" \
  --cutover-log "$OFFICE_AUTOMATE_CUTOVER_LOG" \
  --max-air-quality-age-seconds 300
```

If the OAuth config cannot mint a validation JWT, complete browser/PWA and mobile OAuth manually, then add:

```bash
--manual-public-oauth-verified-at "$(date -Iseconds)"
```

The command validates:

- Rust ERV and HVAC active-control flags are enabled.
- The rollback snapshot manifest exists.
- The operator recorded the legacy stop timestamp.
- The optional legacy `/status` URL no longer responds.
- The MQTT cutover strategy and Qingping device identity are recorded.
- ERV, HVAC, and YoLink live read checks pass.
- Local `/status` has fresh air-quality readings from the active Rust controller.
- Local `/ws` returns the authenticated initial status frame.
- Public `/auth/login` returns a Cloudflare-reachable OAuth start payload.
- Public `/status` is fresh through Cloudflare when automated auth is possible, or manual OAuth verification is recorded.
- The cutover log is written with timestamps, checks, and rollback point.

## Cutover Log

Keep `$OFFICE_AUTOMATE_CUTOVER_LOG` with the deployment record. It intentionally records paths, URLs, strategy, check results, and rollback source, not secret values.

The log is the handoff artifact for deciding whether to keep Rust active through the validation window.

## Rollback

If validation fails:

1. Stop `office-automate-server` and the Cloudflare Tunnel service on the primary host.
2. Start the legacy backend and legacy tunnel.
3. Repoint Qingping to the legacy broker if it moved.
4. Restore state from `$OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR` if Rust wrote incompatible data.
5. Keep Rust active-control flags disabled before starting any shadow-mode follow-up.

Do not decommission the legacy gateway during this ticket. Decommissioning belongs to the later rollback-window and cleanup tickets.
