# Primary Host Modern Stack Cutover

**Ticket:** #62
**Status:** Draft

## Goal

Move Office Automate from a legacy gateway host to a modern always-on primary host by replacing the current Python backend with a small Rust backend/server binary, while preserving climate-control safety, presence accuracy, telemetry history, app artifact serving, and remote access.

This spec is intentionally generic. It does not encode local IP addresses, hostnames, private network layout, or machine-specific paths. Concrete values must come from config files, environment variables, or the operator's deployment notes.

## Recommended Topology

Run Office Automate as a small host-native service bundle on the primary host, centered on a Rust backend binary:

| Responsibility | Recommended Runtime | Why |
| --- | --- | --- |
| Backend/API/WebSocket/state machine | `office-automate-server` Rust binary supervised by launchd | One small native process owns climate orchestration, API, WebSocket broadcast, static/app artifact serving, and database access. |
| MQTT broker for local air-quality device reports | Embedded broker inside `office-automate-server` using Rust MQTT broker primitives | Avoids legacy compatibility constraints and avoids a separate broker process. |
| Presence detection | Internal Rust poller inside `office-automate-server` | The backend runs on the host with keyboard/display signals, so no separate local HTTP reporter is needed. Keep `/occupancy` only for optional external reporters and compatibility. |
| Telemetry and project-leverage collectors | Rust subcommands or scheduled launchd jobs invoking the same binary | Local repos and tool-usage DBs now live on the primary host; one binary can run collectors on intervals and write SQLite directly. |
| Public tunnel / reverse proxy | Cloudflare Tunnel binary supervised by launchd | The tunnel process is light and should terminate on the primary host next to the backend. |
| Data store | SQLite files under a configured data directory | Existing design; low overhead; easy backup and rollback. |

Do **not** make Docker the default for this cutover on macOS. Docker-style containers on macOS run through a Linux VM, which is usually heavier than the services this project needs. Containers also complicate host-integrated features: macOS presence detection, optional Bluetooth paths, LAN device access, local repository scanning, SQLite file ownership, and launchd recovery. Prefer small host-native binaries supervised by launchd.

## Rust Backend Strategy

Build a new Rust binary named `office-automate-server`. It should be the one Office Automate application binary: long-running server, embedded MQTT broker, presence poller, operational CLI, migrations, smoke tests, and collectors.

Expected launch shape:

```bash
office-automate-server serve --config "${OFFICE_AUTOMATE_CONFIG}"
```

Expected subcommands:

```text
office-automate-server
  serve              run HTTP/API/WebSocket server, state machine, MQTT ingress, and device clients
  migrate            create/upgrade SQLite schema
  smoke              run all local dependency checks without changing device state
  smoke mqtt         verify broker publish/subscribe
  smoke presence     verify keyboard/display signal detection
  smoke erv          verify local ERV control credential
  smoke hvac         verify HVAC status read
  collect telemetry  collect session-output telemetry
  collect leverage   collect project-leverage metrics
  snapshot           create pre-cutover DB/artifact snapshot
```

### Backend Modules

| Module | Responsibility |
| --- | --- |
| `config` | Load config and environment overrides; no hard-coded hostnames or addresses. |
| `db` | SQLite schema, migrations, typed queries, and snapshot helpers. |
| `state` | Direct port of PRESENT/AWAY state machine and ERV/HVAC policy. |
| `http` | REST API, OAuth/JWT auth, WebSocket broadcast, static frontend serving, app artifact downloads. |
| `presence` | Internal macOS keyboard/display poller; keeps compatibility `/occupancy` route for external reporters. |
| `mqtt` | Embedded Rust MQTT broker and Qingping topic handling. |
| `erv` | Local ERV control and local-key-invalid detection/recovery events. |
| `hvac` | HVAC cloud client, status polling, and hysteresis decisions. |
| `yolink` | Cloud auth, sensor inventory, MQTT/event handling, restored state. |
| `telemetry` | Session telemetry and project leverage collection. |
| `artifacts` | App artifact metadata, latest download, upload/deploy endpoint. |
| `health` | Structured dependency and safety checks for cutover/rollback. |

### MQTT Design

Embed the MQTT broker in `office-automate-server`. The broker is local, low-volume, and part of the climate-control appliance. A separate broker process should not be part of the target architecture.

Use a Rust MQTT broker implementation such as `rumqttd` as a library or internal module. The server should subscribe internally to the configured sensor topics and write readings directly through the same state/database path as every other device event.

### Cloudflare Tunnel

Run the Cloudflare Tunnel process on the primary host as a separate launchd service:

```text
cloudflared tunnel run <configured-tunnel>
```

The tunnel routes the public hostname to `office-automate-server` on the same host. The tunnel is not a container and should not own application behavior. Its job is transport only.

LocalTunnel is not part of the target architecture. The existing Python route `GET /localtunnel/password` is a dead compatibility helper and must not be ported to Rust. Remote access is Cloudflare Tunnel only: public DNS and TLS terminate through Cloudflare, `cloudflared` forwards to the Rust server on the primary host, and the Rust server continues to enforce OAuth/JWT or trusted-network rules at the application layer.

### Compatibility Boundary

The Rust backend must preserve existing external contracts until clients are migrated:

- `/status`
- `/occupancy`
- `/erv`
- `/hvac`
- `/history/*`
- `/deploy/{app}` or existing app artifact endpoints
- WebSocket payload shape used by the web/PWA client
- OAuth/browser login routes
- trusted-network behavior where configured

Endpoint changes should be a separate ticket after the backend is live. The one explicit exception is `GET /localtunnel/password`: it is not required because Cloudflare Tunnel replaces LocalTunnel, no current repo consumer references it, and it should be removed rather than reimplemented.

## Current External Interface Map

This section is the parity contract for the Rust port. The Rust server should expose the same client-visible behavior unless a row explicitly says "do not port".

### HTTP Transport, Auth, And Static Assets

| Current interface | Current behavior | Rust target |
| --- | --- | --- |
| CORS | Adds `Access-Control-Allow-Origin: *`, methods `GET, POST, OPTIONS`, headers `Content-Type, Authorization`; responds to preflight `OPTIONS`. | Preserve during compatibility period. |
| OAuth middleware | If Google OAuth is configured, all API routes require `Authorization: Bearer <jwt>` unless the route is skipped or the request IP is in `trusted_networks`. Skips `/auth/*` login/device routes, `/apps/*`, `/apk`, WebSocket upgrades, `/assets/*`, `/`, `/index.html`, `.png`, and `.json` static resources. | Preserve skip rules, trusted-network bypass, JWT validation, and 401 JSON shape: `{"error": "...", "login_url": "/auth/login"}` where applicable. |
| Basic auth fallback | If OAuth is not configured and basic credentials are configured, protected HTTP routes require `Authorization: Basic ...`; `/apps/*`, `/apk`, and WebSocket upgrades bypass it. | Support as a temporary compatibility mode or explicitly migrate all deployments to OAuth before Rust cutover. Do not silently leave an authenticated Python deployment open. |
| Static web app | If `frontend/dist` exists, serves `/assets/*`, `/`, and `/{path:.*}` as SPA fallback to `index.html`. | Preserve for the PWA/dashboard. Build location should be configurable or derived from `OFFICE_AUTOMATE_ROOT`. |
| Cloudflare Tunnel | Current production remote access is Cloudflare Tunnel routing the public hostname to the backend. | Run `cloudflared` as a separate launchd service on the same primary host. It is the only public tunnel target. No LocalTunnel support. |
| `GET /localtunnel/password` | Fetches `https://loca.lt/mytunnelpassword` and returns `{"password": "..."}`; no current repo consumer references it. | Do not port. If a temporary compatibility response is needed during cleanup, return 410 Gone with a clear JSON error. The intended target is no route. |

### Realtime Status Contract

| Current interface | Current behavior | Rust target |
| --- | --- | --- |
| `GET /status` | Returns the full live status object. Top-level fields include `state`, `is_present`, `presence_signal_active`, `safety_interlock`, `erv_should_run`, `verifying_departure`, `in_door_open_mode`, `sensors`, `air_quality`, `erv`, `hvac`, `manual_override`, and `notifications`. | Preserve field names and compatible nullability. Add fields only in a backward-compatible way. |
| Status `sensors` object | Current Python sends `mac_last_active`, `external_monitor`, `motion_detected`, `door_open`, `window_open`, `co2_ppm`. Web and Android client models also mention `mac_active`, but that key is absent from the current server payload and is therefore treated as missing/default today. | Preserve current keys. Adding `mac_active` is backward-compatible, but it must not replace `mac_last_active` during compatibility. |
| Status `air_quality` object | Contains `co2_ppm`, `temp_c`, `humidity`, `pm25`, `pm10`, `tvoc`, `noise_db`, `last_update`, `report_interval`, `interval_configured`. | Preserve. `last_update` remains ISO timestamp string or null. |
| Status `erv` object | Contains `running`, `tvoc_ventilation`, `speed`, `tvoc_plateau`, `tvoc_baseline`, `away_stale_flush_enabled`, `away_stale_flush_active`, `away_stale_flush_active_until`, `away_stale_flush_next_due_at`, `room_closed_since`, and `control`. Current `control` fields are `last_ok_at`, `last_local_ok_at`, `last_error`, `last_error_at`, `using_cloud`, `local_key_invalid`, `local_key_invalid_since`, and `consecutive_local_key_errors`. | Preserve the full current `erv` object and full `erv.control` object unless a future spec explicitly marks a field do-not-port. These fields are operator-visible health and safety signals, not just client decoration. |
| Status `hvac` object | Contains `mode`, `setpoint_c`, `suspended`, `temperature_bands`, `temperature_band_defaults`. | Preserve. Temperature-band values remain Fahrenheit integer fields. |
| Status `manual_override` object | Contains `erv`, `erv_speed`, `erv_expires_in`, `hvac`, `hvac_mode`, `hvac_setpoint_f`, `hvac_expires_in`. | Preserve with integer seconds remaining or null. |
| Status `notifications` array | Contains app notification objects with fields such as `id`, `type`, `severity`, `title`, `message`, `created_at`, `active`, `runbook_path`. Currently used for ERV control health notification. | Preserve notification shape and broadcast updates over WebSocket. |
| `GET /ws` | WebSocket sends a full status JSON immediately, then broadcasts the same status shape on changes. Text message `ping` returns text `pong`. If OAuth is enabled and client is not trusted, the Python server expects the first text message to be `{"type":"auth","token":"..."}` within 10 seconds; invalid auth closes with code 4001. | Preserve full-status message shape and `ping`/`pong`. Support both existing auth styles: browser first-message auth and Android `Authorization: Bearer <jwt>` on the upgrade request. Keep trusted-network bypass and close code 4001 for auth failure during compatibility. |

### Control And Configuration Routes

| Current interface | Request | Success response | Rust target |
| --- | --- | --- | --- |
| `POST /occupancy` | JSON `{"last_active_timestamp": <unix seconds>, "external_monitor": <bool>}` from the macOS detector. | `{"ok": true, "state": "present|away", "erv_should_run": <bool>}`. | Keep as compatibility route for external reporters. Internal Rust presence poller should feed the same state-machine update path. |
| `POST /presence` | JSON `{"state": "present|away"}`. Invalid values return 400 with `{"ok": false, "error": "state must be present or away"}`. | `{"ok": true, "state": "present|away", "is_present": <bool>}`. | Preserve. This remains the manual correction API for dashboard/mobile. |
| `POST /erv` | JSON `{"speed": "off|quiet|medium|turbo"}`. Invalid values return 400. | `{"ok": <bool>, "erv": {"speed": "...", "running": <bool>, "manual_override": true, "expires_in": <seconds>}}`. | Preserve and route through Rust ERV client. Must log climate action and broadcast status. |
| `POST /hvac` | JSON `{"mode": "off|heat|cool", "setpoint_f": <number>}`. Invalid mode returns 400; unconfigured HVAC returns 503. | `{"ok": true, "hvac": {"mode": "...", "setpoint_f": <number>, "setpoint_c": <number>, "manual_override": true, "expires_in": <seconds>}}`. | Preserve. Rust converts Fahrenheit to Celsius before device command and logs climate action. |
| `GET /hvac/temperature-bands` | No body. | `{"ok": true, "temperature_bands": {...}, "temperature_band_defaults": {...}}`. | Preserve. |
| `POST /hvac/temperature-bands` | JSON `{"temperature_bands": {...}}` or a raw band object. Required integer keys: `heat_on_temp_f`, `heat_off_temp_f`, `cool_off_temp_f`, `cool_on_temp_f`. Current validation ranges are 45-85, 46-90, 55-95, and 56-100 respectively, with heat-on below heat-off and cool-off below cool-on. | Same shape as GET. Invalid values return 400 with `{"ok": false, "error": "..."}`. | Preserve validation and persisted setting behavior. |
| `POST /qingping/interval` | JSON `{"interval": <int>}`; must be integer >= 15. | `{"ok": true, "interval": <int>, "message": "Device configured to report every ... seconds"}` or 503 if publish fails. | Preserve if the embedded MQTT path can publish to the device `down` topic. Otherwise make this endpoint return 503 until the MQTT command path is live. |

### History And Productivity Routes

All history routes clamp `days` to 1-30 where used and `hours` to 1-168 where used. Error responses are JSON `{"ok": false, "error": "..."}` with HTTP 400 unless stated otherwise.

| Current interface | Query | Response contract | Rust target |
| --- | --- | --- | --- |
| `GET /history` | `hours` default 24, max 168; `limit` default 1000, max 10000. | `{"ok": true, "hours": <int>, "sensor_readings": [...], "occupancy_history": [...], "device_events": [...], "climate_actions": [...]}`. | Preserve for web/debug compatibility. |
| `GET /history/sessions` | `days` default 7. | `{"ok": true, "days": <int>, "sessions": [{"date", "arrival", "departure", "duration_hours", "gaps": [{"left", "returned", "duration_min"}]}], "summary": {"avg_arrival", "avg_departure", "avg_duration_hours", "std_arrival_min", "std_departure_min", "total_hours_week"}}`. | Preserve. |
| `GET /history/co2-ohlc` | `hours` default 24; optional `bucket_minutes`. | `{"ok": true, "hours": <int>, "bucket_minutes": <int>, "candles": [{"timestamp", "open", "high", "low", "close", "avg", "readings"}]}`. | Preserve bucket defaults: 5, 15, 60, or 240 minutes based on range. |
| `GET /history/temperature` | `hours` default 24. | `{"ok": true, "hours": <int>, "bucket_minutes": <int>, "points": [{"timestamp", "avg_f", "min_f", "max_f", "readings"}]}`. | Preserve bucket defaults: 5, 15, 30, or 120 minutes based on range. |
| `GET /history/daily-stats` | `days` default 7. | `{"ok": true, "days": <int>, "stats": [{"date", "door_events", "erv_runtime_min", "hvac_runtime_min", "presence_hours"}]}`. | Preserve. |
| `GET /history/openings` | `days` default 7. | `{"ok": true, "days": [{"date", "door": [{"open", "close"}], "window": [{"open", "close"}]}]}`. | Preserve interval splitting by date. |
| `GET /history/orchestration` | `days` default 7. | `{"ok": true, "days": [{"date", "messages", "sessions", "first_prompt", "last_prompt", "by_tool": {"claude": <int>, "codex": <int>}, "timestamps": [{"time", "tool"}]}]}`. | Preserve. Collector implementation can move to Rust, but endpoint payload stays stable. |
| `GET /history/project-focus` | `days` default 7. | `{"ok": true, "days": [{"date", "total", "projects": [{"name", "messages", "first_prompt", "last_prompt"}]}]}`. | Preserve project normalization behavior. |
| `GET /history/leverage` | `days` default 7. | `{"ok": true, "days": [daily leverage objects], "week": <aggregate>}`. Daily and weekly objects include prompts, sessions, lines added/removed/changed, files modified, commits, PRs opened/merged, average PR cycle, lines per prompt, commits per prompt, lines per session minute, and week `active_days`. | Preserve. Rust may attach/read `telemetry.db` differently, but output must remain compatible with Android `LeverageResponse`. |
| `GET /history/project-leverage` | `days` default 7. | `{"ok": true, "projects": {"session-manager": {...}, "engram": {...}, "agent-os": {...}, "office-automate": {...}}}` with per-project `summary`, `days`, and `week` or `current` sections. | Preserve known project keys and metric names consumed by Android Projects tab. |

### App Artifact Routes

| Current interface | Current behavior | Rust target |
| --- | --- | --- |
| `POST /deploy/{app}` | Multipart upload. App name must match `^[a-z0-9][a-z0-9-]*$`. Required file part is `file`; optional text parts are `version_code` and `version_name`. Max artifact size is 100 MB. Stores `latest.apk`, creates immutable `{hash}.apk` using first 8 hex chars of SHA-256, writes `meta.json`, and returns `{"ok": true, "app": "...", "size_bytes": <int>, "download_url": "/apps/{app}/latest.apk"}`. | Preserve. Use atomic writes and keep metadata schema stable. |
| `GET /apps/{app}/latest.apk` | Redirects to `/apps/{app}/{artifact_hash}.apk` using metadata. Response has `Cache-Control: no-cache`. | Preserve redirect and cache behavior. |
| `GET /apps/{app}/{artifact_hash}.apk` | Serves immutable APK where `artifact_hash` matches `^[0-9a-f]{8}$`, with `Cache-Control: public, max-age=31536000, immutable` and `Content-Disposition: attachment; filename={app}.apk`. | Preserve. |
| `GET /apps/{app}/meta.json` | Returns metadata JSON with `artifact_hash`, `uploaded_at`, `size_bytes`, `uploaded_by`, optional `version_code`, optional `version_name`. | Preserve. Android update check depends on this for `office-climate`. |
| `GET /apk` | Legacy single-app APK download, `Content-Disposition: attachment; filename=office-climate.apk`. | Preserve until Android clients no longer need it. |

### OAuth Routes

| Current interface | Current behavior | Rust target |
| --- | --- | --- |
| `GET /auth/login` | Starts Google OAuth authorization-code flow with PKCE. Optional `platform=android` is stored with the state. Response JSON: `{"authorization_url": "...", "state": "..."}`. Redirect URI is built from request scheme/host, honoring `X-Forwarded-Proto` and local hosts. | Preserve. Must work behind Cloudflare Tunnel and on local/trusted hosts. |
| `GET /auth/callback` | Handles Google callback. On success for web, returns HTML that stores `auth_token` and `user_email` in localStorage and redirects to `/`. On success for Android, redirects to `officeclimate://auth?token=...&email=...`. Errors return HTML or text with appropriate 400/403/501 status. | Preserve until clients are migrated to a different auth flow. |
| `POST /auth/logout` | Requires Bearer token; verifies JWT, invalidates in-memory session if present, returns `{"ok": true, "message": "Logged out"}`. | Preserve response shape. If Rust persists sessions differently, JWT invalidation semantics must be no weaker than current behavior. |
| `POST /auth/device/start` | Starts Google device flow and returns Google's device-code payload. | Preserve if any headless reporter still needs it; otherwise deprecate only after external reporter migration is complete. |
| `POST /auth/device/poll` | JSON `{"device_code": "..."}`. Missing code returns 400. Otherwise returns the current device-flow polling result. | Preserve while `/occupancy` external reporters can use device flow. |

### MQTT And External Device Protocols

| Current interface/dependency | Current behavior | Rust target |
| --- | --- | --- |
| Qingping local MQTT | Current Python process connects as an MQTT client to a configured broker at `qingping.mqtt_broker:mqtt_port`, subscribes to `qingping/{DEVICE_MAC}/up`, parses both local `sensorData` and cloud-style `sensor_data` payloads, and publishes interval config to `qingping/{DEVICE_MAC}/down`. | Rust server owns an embedded MQTT broker on configured host/port, accepts the same `qingping/{DEVICE_MAC}/up` payloads, and publishes the same `down` config messages for `/qingping/interval`. Device reconfiguration points to `OFFICE_AUTOMATE_MQTT_HOST` and port. |
| YoLink cloud HTTP/MQTT | Current Python process authenticates to YoLink cloud HTTP, discovers devices, then subscribes to `yl-home/{home_id}/+/report` on YoLink MQTT for door/window/motion events. This is outbound from Office Automate, not an API Office Automate serves. | Rust must preserve device-state semantics and reconnect behavior. This is a device-client parity requirement, not a public Office Automate route. |
| ERV local Tuya/Shelly and Kumo HVAC | Current server issues outbound device commands and exposes their state through `/status`, `/erv`, and `/hvac`. | Rust must keep outbound clients behind smoke checks and must not enable active writes until parity and credential validation pass. |

## Service Model

The primary host should have four service groups:

1. **Core service:** `office-automate-server`
   - Runs the Rust backend binary.
   - Reads one configured deployment config.
   - Owns `office_climate.db`, API routes, WebSocket routes, app artifact serving, presence polling, and climate-device coordination.

2. **Sensor ingress:** `mqtt`
   - Runs inside the backend binary.
   - Accepts reports from local air-quality sensors.
   - Restricts listening/binding through config, not hard-coded addresses.
   - Enables persistence only if the deployment requires retained/offline messages.

3. **Collectors:** `telemetry`, `project-leverage`
   - Session telemetry runs on an interval where the session/tool DB and watched repositories live.
   - Project leverage collection runs locally for sources that now live on the primary host.
   - Prefer invoking `office-automate-server collect ...` rather than maintaining separate scripts.

4. **External access:** `tunnel`
   - Routes the public hostname to `office-automate-server`.
   - Keeps credentials outside the repo.
   - Must not be required for trusted local service-to-service calls.

## Configuration Surface

All deployment-specific values should be expressed through config or environment variables:

| Setting | Purpose |
| --- | --- |
| `OFFICE_AUTOMATE_ROOT` | Project checkout directory. |
| `OFFICE_AUTOMATE_CONFIG` | Runtime config file path. |
| `OFFICE_AUTOMATE_DATA_DIR` | SQLite, app artifacts, and generated state. |
| `OFFICE_AUTOMATE_BASE_URL` | Base URL for local health checks and optional external clients. |
| `OFFICE_AUTOMATE_PUBLIC_URL` | Public URL used by browser/mobile clients. |
| `OFFICE_AUTOMATE_MQTT_HOST` | Embedded broker address configured into MQTT-producing devices. |
| `OFFICE_AUTOMATE_MQTT_PORT` | Broker port. |
| `OFFICE_AUTOMATE_BACKUP_DIR` | Local backup/snapshot directory. |

Launchd plists should be templates or generated files. They should reference those variables or generated absolute paths, not embed local addresses.

## Data And Secret Migration

Before any cutover:

1. Stop nonessential collector jobs on both hosts.
2. Snapshot the legacy data directory.
3. Copy these files to the primary host:
   - climate SQLite DB
   - telemetry SQLite DB
   - app artifact directory and metadata
   - worktree/project mapping data
   - OAuth client material
   - local device-control credentials
   - tunnel credentials
4. Validate file ownership and permissions.
5. Run Rust migration and smoke checks without starting active climate control.

The cutover must include a rollback backup that can restore the legacy gateway to its previous state.

## MQTT Modernization

Use the embedded broker inside `office-automate-server`.

Recommended minimal embedded broker config shape:

```toml
[v4.1]
name = "office-automate"
listen = "${OFFICE_AUTOMATE_MQTT_HOST}:${OFFICE_AUTOMATE_MQTT_PORT}"
next_connection_delay_ms = 1

[v4.1.connections]
connection_timeout_ms = 60000
max_payload_size = 20480
max_inflight_count = 100
dynamic_filters = true
```

Authentication can be added later if the broker is reachable beyond the trusted local segment. For the first cutover, keep the broker simple and limit exposure with bind/listener configuration and host firewall rules.

Cutover steps:

1. Build `office-automate-server`.
2. Start `office-automate-server serve` with MQTT enabled in shadow/read-only mode if supported.
3. Subscribe through the server's smoke command to the expected sensor topic pattern.
4. Reconfigure the air-quality device to publish to `OFFICE_AUTOMATE_MQTT_HOST`.
5. Confirm live reports arrive at the embedded broker.
6. Confirm fresh sensor readings are visible through the server API.
7. Disable the legacy broker only after fresh readings are visible through the Rust server API.

## Backend Cutover

The backend should never run with active climate control on two hosts at the same time.

Cutover steps:

1. On the primary host, build the Rust release binary.
2. Run `office-automate-server migrate`.
3. Run `office-automate-server smoke`.
4. Validate local device credentials before persistent launch:
   - ERV local control can connect.
   - HVAC status can be read.
   - cloud sensor APIs authenticate.
   - MQTT sensor readings are fresh.
5. Stop the legacy backend.
6. Start `office-automate-server serve`.
7. Verify `/status` reports:
   - fresh air-quality timestamp
   - correct door/window/motion states
   - valid ERV control state
   - no local-key invalid alert
   - expected OAuth/trusted-network behavior
8. Keep the legacy backend disabled during the validation window.

If ERV local control fails on the primary host, do not complete the cutover. Recover the device-control credential first or keep the legacy gateway as the active climate controller.

## Presence Cutover

Presence runs on the host that owns the real keyboard, pointing device, and external displays.

Cutover steps:

1. Run `office-automate-server smoke presence` and confirm external displays are detected.
2. Enable internal presence polling in the Rust server config.
3. Disable any old standalone presence reporters.
4. Confirm the state machine sees `external_monitor=true` and recent activity timestamps from the primary host.
5. Confirm manual presence controls still work.

Presence can safely move before the full backend cutover only if the compatibility reporter remains pointed at the currently active backend URL. The target state is internal polling inside the Rust server.

## Collector Consolidation

After the primary host owns the repositories and session/tool DBs:

1. Run `office-automate-server collect telemetry` locally on the primary host.
2. Run `office-automate-server collect leverage` locally for tool usage, agent/persona reads, repository mappings, and Office Automate data.
3. Remove legacy rsync/push workflows that copied telemetry from one host to another.
4. Keep only backup/snapshot sync, not operational sync.

This is one of the main simplifications from the cutover: the system no longer needs to shuttle generated telemetry between a workstation and a gateway host.

## Launchd Layout

Recommended labels:

| Label | Program |
| --- | --- |
| `com.office-automate.server` | `office-automate-server serve --config <config>` |
| `com.office-automate.telemetry` | `office-automate-server collect telemetry --config <config>` interval job |
| `com.office-automate.project-leverage` | `office-automate-server collect leverage --config <config>` interval job |
| `com.office-automate.tunnel` | public tunnel client |

Each plist should define:

- `WorkingDirectory`
- `ProgramArguments`
- `EnvironmentVariables`
- `RunAtLoad`
- `KeepAlive` for long-running services
- `StartInterval` for collectors
- stdout/stderr log paths

## Validation Gates

Do not decommission the legacy gateway until all gates pass:

| Gate | Check |
| --- | --- |
| API live | `/status` returns 200 from `office-automate-server`. |
| Fresh sensors | air-quality `last_update` is recent. |
| Presence live | internal Rust poller sees primary-host keyboard/display activity. |
| ERV safe | local control succeeds and no local-key-invalid alert is active. |
| HVAC readable | HVAC status polling succeeds. |
| Door/window/motion live | YoLink events or restored state match reality. |
| MQTT live | embedded broker receives sensor reports after device reconfiguration. |
| Remote access | public URL reaches `office-automate-server` through Cloudflare Tunnel and OAuth works. |
| Mobile/PWA | mobile app or PWA reads status and can apply manual controls. |
| Interface parity | Every route and protocol listed in "Current External Interface Map" either matches the Python behavior or is explicitly marked do-not-port. |
| Telemetry | leverage endpoints show nonzero recent data after collector run. |
| Artifacts | app artifact endpoint serves latest metadata and download. |
| Restart recovery | after reboot/login, required launchd jobs restart automatically. |

## Rollback

Rollback must be one command sequence, documented before cutover:

1. Stop `office-automate-server` and the primary-host tunnel.
2. Start legacy backend and legacy tunnel.
3. Repoint MQTT-producing devices to the legacy broker if they were moved.
4. Re-enable legacy presence only if the real workstation signals are back on that host.
5. Restore data from the pre-cutover snapshot if the primary wrote incompatible state.

Presence and telemetry rollback are lower risk than climate-control rollback. Climate-control rollback must be tested first.

## Implementation Plan

1. Create the Rust workspace and `office-automate-server` binary.
2. Port config loading, SQLite migrations, and read-only `/status` parity first.
3. Port state machine and write parity tests against the current Python behavior.
4. Port HTTP/JSON/WebSocket/OAuth/static/artifact contracts with parity tests against the interface map.
5. Port embedded MQTT broker and Qingping passive ingestion.
6. Port YoLink passive sensor inventory, restored state, and event handling.
7. Port ERV read/health/smoke support, including full `erv.control` status parity, with no active writes enabled.
8. Port ERV active write control behind explicit smoke gates and a config-controlled active-control enable flag.
9. Port HVAC status/read behavior and temperature-band persistence with no active writes enabled.
10. Port HVAC active write control behind explicit smoke gates and a config-controlled active-control enable flag.
11. Port internal presence poller and compatibility `/occupancy`.
12. Port telemetry/project-leverage collectors as binary subcommands.
13. Add launchd templates for server, collectors, and Cloudflare Tunnel.
14. Run migration/snapshot dry-runs against copied data and artifacts.
15. Run shadow-mode validation against copied data and live read-only device checks.
16. Cut over backend and embedded MQTT only after ERV and HVAC write gates are validated.
17. Validate rollback from the Rust-written state back to the legacy gateway.
18. Remove legacy rsync/push jobs and decommission legacy launchd jobs after a rollback window.

## Risks

| Risk | Mitigation |
| --- | --- |
| Duplicate climate controllers | Never leave active-control backends running on both hosts during cutover. |
| Stale ERV local key | Validate local ERV control before persistent Rust server launch. |
| Sensor MQTT misconfiguration | Verify broker subscription before disabling legacy broker. |
| Missing tunnel credentials | Keep local LAN access path working before changing public access. |
| Broken telemetry due to path changes | Make repo paths configurable and verify collector dry-runs. |
| Launchd runs before network is ready | Use KeepAlive and service health checks; collectors should tolerate failures. |

## Non-Goals

- Changing the presence state machine behavior, except for porting it exactly to Rust.
- Replacing SQLite with a server database.
- Kubernetes or a container orchestrator.
- Docker-first deployment on macOS.
- Changing climate-control thresholds during the host cutover.

## Ticket Classification

**Epic.** This should be split before implementation because it crosses infrastructure, device configuration, climate-control safety, telemetry pipelines, and legacy decommissioning. Proposed implementation tickets:

1. Rust backend workspace, config loading, DB migrations, and read-only status API.
2. State machine and climate-policy parity tests, with no device writes.
3. HTTP/JSON/WebSocket/OAuth/static/artifact contract parity from the interface map.
4. Embedded MQTT broker plus Qingping passive ingestion and interval-command path.
5. YoLink passive sensor inventory, restored state, MQTT event handling, and reconnect behavior.
6. ERV read/health/smoke parity, including full `erv.control` status shape and local-key-invalid notification state, with active writes disabled.
7. ERV active write control for manual and automated speed changes, enabled only after smoke checks and behind an explicit active-control gate.
8. HVAC status/read parity and temperature-band persistence, with active writes disabled.
9. HVAC active write control for mode/setpoint changes, enabled only after smoke checks and behind an explicit active-control gate.
10. Internal Rust presence poller plus compatibility `/occupancy` route.
11. Telemetry and project-leverage collector subcommands.
12. Primary-host launchd templates and Cloudflare Tunnel service setup.
13. Data, secret, artifact, and credential migration dry-run with pre-cutover snapshots.
14. Shadow-mode validation against copied data and live read-only dependencies.
15. Backend/MQTT cutover execution with one active climate controller.
16. Rollback validation from Rust-written state back to the legacy gateway.
17. Legacy rsync/push cleanup and legacy launchd decommissioning after the rollback window.
