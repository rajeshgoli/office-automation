# Backend/MQTT Cutover

Ticket #78 executes the backend and Qingping MQTT cutover described in `docs/working/62_primary_host_modern_stack.md`.

This procedure is for the cutover window only. Do not leave Python and Rust running with active climate control at the same time.

## Preconditions

- Ticket #76 snapshot completed and produced a rollback snapshot directory with `manifest.json`.
- Ticket #77 shadow validation completed against the Rust backend with Python still active.
- ERV and HVAC active write gates were validated by their earlier tickets.
- Cloudflare Tunnel config was validated with the deployed `cloudflared` binary.
- Sanitized Cloudflare evidence was captured from API, Terraform, or dashboard state for the exact Office hostname.
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
export OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE="/absolute/path/to/cloudflare-evidence.json"
```

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

The Cloudflare evidence file must use the schema from `docs/deployment/shadow-mode-validation.md` and prove:

- the exact hostname is protected by an Access application,
- no policy action is Bypass,
- no policy includes public users,
- no wildcard DNS record points at Office Automate,
- the tunnel routes only to the loopback or Unix-socket origin,
- no Cloudflare private-network routes exist for this tunnel,
- the final ingress rule is `http_status:404`,
- Access audit evidence includes unauthenticated blocks and authenticated successes.

Stop the legacy Python backend and record the timestamp:

```bash
export OFFICE_AUTOMATE_LEGACY_STOPPED_AT="$(date -Iseconds)"
```

Apply the selected MQTT feed strategy:

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
  --cloudflared-config "$CLOUDFLARED_CONFIG" \
  --cloudflare-evidence "$OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE" \
  --legacy-base-url "$OFFICE_AUTOMATE_LEGACY_BASE_URL" \
  --legacy-controller-stopped-at "$OFFICE_AUTOMATE_LEGACY_STOPPED_AT" \
  --mqtt-strategy "$OFFICE_AUTOMATE_MQTT_CUTOVER_STRATEGY" \
  --snapshot-dir "$OFFICE_AUTOMATE_CUTOVER_SNAPSHOT_DIR" \
  --cutover-log "$OFFICE_AUTOMATE_CUTOVER_LOG" \
  --max-air-quality-age-seconds 300
```

The validator always sends unauthenticated public probes first. Those probes must be blocked by Cloudflare Access before they reach origin, including `/auth/login`, `/auth/callback`, `/auth/device/start`, `/auth/device/poll`, static assets, `/apps/*`, `/apk`, `/deploy/*`, `/status`, and a real `/ws` WebSocket upgrade.

For the authenticated public `/status` probe, either provide an operator-only Cloudflare Access service token:

```bash
export OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID="..."
export OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET="..."
```

or complete browser/PWA and mobile verification through Cloudflare Access plus Office auth manually, then add:

```bash
--manual-public-oauth-verified-at "$(date -Iseconds)"
```

Do not use Android app credentials or bundled APK secrets for these operator validation headers.

The command validates:

- Rust ERV and HVAC active-control flags are enabled.
- The rollback snapshot manifest exists.
- The operator recorded the legacy stop timestamp.
- The optional legacy `/status` URL no longer responds.
- The MQTT cutover strategy and Qingping device identity are recorded.
- ERV, HVAC, and YoLink live read checks pass.
- Local `/status` has fresh air-quality readings from the active Rust controller.
- Local `/ws` returns the authenticated initial status frame.
- The Cloudflare tunnel config publishes only the exact public hostname, routes it to a loopback/Unix origin, has no wildcard hostname/private-network route, and ends in `http_status:404`.
- The Cloudflare evidence proves Access app/policy/DNS/tunnel/audit state and fails cutover on Bypass, public allow, wildcard DNS, private routes, hostname drift, or missing audit observations.
- Unauthenticated public HTTP routes and `/ws` are blocked by Cloudflare Access before origin.
- Public `/status` is fresh through Cloudflare Access and Office auth when automated service-token validation is possible, or manual Access plus Office verification is recorded.
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
