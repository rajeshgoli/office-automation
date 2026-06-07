# Primary Host Launchd Services

Ticket #75 adds launchd templates for the Rust primary-host deployment described in `docs/working/62_primary_host_modern_stack.md`.

The templates live in `scripts/launchd/primary-host/`:

| Template | Label | Purpose |
| --- | --- | --- |
| `com.office-automate.server.plist.template` | `com.office-automate.server` | Runs `office-automate-server serve --config <config>` as the core API, WebSocket, MQTT ingress, presence, and device-client process. |
| `com.office-automate.edge.plist.template` | `com.office-automate.edge` | Runs `office-automate-server serve-edge --config <edge-config>` as the quarantined public HTTP edge and narrow controller client. |
| `com.office-automate.telemetry.plist.template` | `com.office-automate.telemetry` | Runs `office-automate-server collect --config <config> telemetry` on an interval. |
| `com.office-automate.project-leverage.plist.template` | `com.office-automate.project-leverage` | Runs `office-automate-server collect --config <config> leverage` on an interval. |
| `com.office-automate.tunnel.plist.template` | `com.office-automate.tunnel` | Runs `cloudflared tunnel --no-autoupdate --config <config> run <tunnel>` as the quarantined public transport process. |

LocalTunnel is intentionally not represented. Public access is Cloudflare Tunnel only. `cloudflared` should route to `office-automate-server serve-edge`; the edge then calls the controller over the local authenticated IPC token.

## Template Values

Render the templates with deployment-specific absolute paths before loading them:

| Placeholder | Meaning |
| --- | --- |
| `__OFFICE_AUTOMATE_ROOT__` | Repository checkout or release directory. |
| `__OFFICE_AUTOMATE_PYTHON__` | Python interpreter with `paho-mqtt` installed, usually the repo virtualenv Python. |
| `__OFFICE_AUTOMATE_SERVER_BIN__` | Absolute path to the Rust `office-automate-server` binary. |
| `__OFFICE_AUTOMATE_CONFIG__` | Absolute path to the deployment config file. |
| `__OFFICE_AUTOMATE_EDGE_CONFIG__` | Absolute path to the edge-only config file. This file must not contain device, telemetry, repo, or climate database settings. |
| `__OFFICE_AUTOMATE_EDGE_WORKING_DIRECTORY__` | Working directory for the edge process, usually a release directory readable by the edge user. |
| `__OFFICE_AUTOMATE_EDGE_LOG_DIR__` | Directory for edge stdout/stderr logs. |
| `__OFFICE_AUTOMATE_PATH__` | PATH available to launchd jobs, usually including Homebrew and system paths. |
| `__OFFICE_AUTOMATE_RUST_LOG__` | Rust log filter, for example `info,office_automate_server=info`. |
| `__OFFICE_AUTOMATE_LOG_DIR__` | Directory for stdout/stderr logs. Create it before loading jobs. |
| `__OFFICE_AUTOMATE_TELEMETRY_INTERVAL_SECONDS__` | Telemetry collector interval in seconds, for example `1800`. |
| `__OFFICE_AUTOMATE_PROJECT_LEVERAGE_INTERVAL_SECONDS__` | Project-leverage collector interval in seconds, for example `7200`. |
| `__CLOUDFLARED_BIN__` | Absolute path to the `cloudflared` binary. |
| `__CLOUDFLARED_CONFIG__` | Absolute path to the Cloudflare Tunnel config file. |
| `__CLOUDFLARED_TUNNEL__` | Cloudflare Tunnel name or UUID. |
| `__CLOUDFLARED_WORKING_DIRECTORY__` | Directory where `cloudflared` should run. |
| `__OFFICE_AUTOMATE_TUNNEL_USER__` | Dedicated low-privilege user that runs only `cloudflared`, for example `_office_tunnel`. |
| `__OFFICE_AUTOMATE_TUNNEL_GROUP__` | Dedicated group for the tunnel user, for example `_office_tunnel`. |
| `__OFFICE_AUTOMATE_EDGE_USER__` | Dedicated low-privilege user that runs only the public HTTP edge, for example `_office_edge`. |
| `__OFFICE_AUTOMATE_EDGE_GROUP__` | Dedicated group for the edge user, for example `_office_edge`. |
| `__OFFICE_AUTOMATE_ORIGIN_PORTS__` | Loopback origin ports the tunnel/edge users may reach, for example `8080`. |

Keep hostnames, public routes, credentials, and tunnel credential files in the Cloudflare config and deployment secrets, not in these templates.

Raw `.plist.template` files are not loadable plists. Render every placeholder, including integer `StartInterval` values, then lint the rendered files:

```bash
plutil -lint rendered/com.office-automate.*.plist
```

## Public Edge Quarantine

`cloudflared` and `serve-edge` are public edge code. Do not run either as the logged-in user. Use dedicated low-privilege tunnel and edge accounts with no shell, no repo access, no controller config/data access, and only the config/credential/log/static paths they need.

Recommended local ownership model:

| Path | Owner | Mode | Purpose |
| --- | --- | --- | --- |
| `/Library/LaunchDaemons/com.office-automate.tunnel.plist` | `root:wheel` | `0644` | LaunchDaemon wrapper that switches to the tunnel user. |
| `/Library/LaunchDaemons/com.office-automate.edge.plist` | `root:wheel` | `0644` | LaunchDaemon wrapper that switches to the edge user. |
| `/var/lib/office-automate/tunnel/` | `_office_tunnel:_office_tunnel` | `0700` | Cloudflare tunnel config and credential directory. |
| `/var/lib/office-automate/edge/` | `_office_edge:_office_edge` | `0700` | Edge-only config and controller IPC token. |
| `/var/log/office-automate/tunnel/` | `_office_tunnel:_office_tunnel` | `0700` | Tunnel stdout/stderr logs. |
| `/var/log/office-automate/edge/` | `_office_edge:_office_edge` | `0700` | Edge stdout/stderr logs. |
| Frontend static assets | `_office_edge:_office_edge` or release owner with edge read-only access | `0550` dirs, `0440` files | The edge may read static assets only; it does not need repo, DB, telemetry, artifact-write, or device-secret access. |
| Controller config, data, repos, telemetry DBs | controller user/group only | `0600` files, `0700` directories | Must be unreadable and non-traversable by `_office_tunnel` and `_office_edge`. |
| Public edge config and credentials | public edge user/group only | `0600` files, `0700` directories | Must be unreadable and non-traversable by `_office_tunnel` unless the tunnel explicitly needs them. |

Create the tunnel account through your normal macOS account-management path or MDM. The account must be non-login and dedicated to Office Automate tunnel transport. The examples below assume `_office_tunnel`.

```bash
sudo install -d -o _office_tunnel -g _office_tunnel -m 0700 /var/lib/office-automate/tunnel
sudo install -d -o _office_tunnel -g _office_tunnel -m 0700 /var/log/office-automate/tunnel
sudo install -d -o _office_edge -g _office_edge -m 0700 /var/lib/office-automate/edge
sudo install -d -o _office_edge -g _office_edge -m 0700 /var/log/office-automate/edge
sudo install -o _office_tunnel -g _office_tunnel -m 0600 "$CLOUDFLARED_CONFIG" /var/lib/office-automate/tunnel/config.yml
sudo install -o _office_tunnel -g _office_tunnel -m 0600 "$CLOUDFLARED_CREDENTIALS" /var/lib/office-automate/tunnel/credentials.json
sudo install -o _office_edge -g _office_edge -m 0600 "$OFFICE_AUTOMATE_EDGE_CONFIG" /var/lib/office-automate/edge/config.yaml
```

The edge config must use the edge-only schema:

```yaml
orchestrator:
  host: "127.0.0.1"
  port: 8080
  google_oauth:
    client_id: "<google-client-id>"
    client_secret: "<google-client-secret>"
    allowed_emails:
      - "you@example.com"
    jwt_secret: "<stable-jwt-secret>"
    trusted_networks: []
controller:
  base_url: "http://127.0.0.1:9001"
  token: "<same value as controller orchestrator.controller_ipc_token>"
runtime:
  frontend_dist: "/opt/office-automate/frontend/dist"
```

The edge config parser rejects unrelated top-level sections such as `qingping`, `yolink`, `erv`, `mitsubishi`, `telemetry`, or `thresholds`. Keep the controller IPC token out of rendered plists; store it in the edge config and in the controller config or `OFFICE_AUTOMATE_CONTROLLER_IPC_TOKEN`.

The Cloudflare config in `/var/lib/office-automate/tunnel/config.yml` should reference the credential copy in that same directory and route only to the loopback edge origin. The edge should then reach only the loopback controller IPC URL.

Render and install the PF anchor template in `scripts/pf/office-automate-edge-anchor.conf.template` after replacing every placeholder:

```bash
sudo install -o root -g wheel -m 0644 rendered/office-automate-edge-anchor.conf /etc/pf.anchors/office-automate-edge
```

Then include the anchor from `/etc/pf.conf`:

```pf
anchor "office-automate-edge"
load anchor "office-automate-edge" from "/etc/pf.anchors/office-automate-edge"
```

Load and inspect PF:

```bash
sudo pfctl -f /etc/pf.conf
sudo pfctl -E
sudo pfctl -sr | rg office-automate -A5
```

The anchor must deny the tunnel/edge users outbound RFC1918/LAN traffic while still allowing loopback origin access. A compromised `cloudflared` process must not be able to connect to ERV/HVAC/MQTT/LAN addresses or read controller secrets.

## Install

Use LaunchAgents only for jobs that need the logged-in macOS user session. The controller currently owns presence polling, so `com.office-automate.server` remains a LaunchAgent. The edge and tunnel do not need the user session and must run as LaunchDaemons under their dedicated users.

```bash
mkdir -p "$HOME/Library/LaunchAgents" "$OFFICE_AUTOMATE_LOG_DIR"
cp rendered/com.office-automate.server.plist "$HOME/Library/LaunchAgents/"
cp rendered/com.office-automate.telemetry.plist "$HOME/Library/LaunchAgents/"
cp rendered/com.office-automate.project-leverage.plist "$HOME/Library/LaunchAgents/"
sudo install -o root -g wheel -m 0644 rendered/com.office-automate.edge.plist /Library/LaunchDaemons/com.office-automate.edge.plist
sudo install -o root -g wheel -m 0644 rendered/com.office-automate.tunnel.plist /Library/LaunchDaemons/com.office-automate.tunnel.plist

launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
sudo launchctl bootstrap system /Library/LaunchDaemons/com.office-automate.edge.plist
sudo launchctl bootstrap system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

Validate loaded jobs:

```bash
launchctl print "gui/$(id -u)/com.office-automate.server"
launchctl print "gui/$(id -u)/com.office-automate.telemetry"
launchctl print "gui/$(id -u)/com.office-automate.project-leverage"
sudo launchctl print system/com.office-automate.edge
sudo launchctl print system/com.office-automate.tunnel
```

## Unload

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
sudo launchctl bootout system /Library/LaunchDaemons/com.office-automate.edge.plist
sudo launchctl bootout system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

## Restart

```bash
launchctl kickstart -k "gui/$(id -u)/com.office-automate.server"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.telemetry"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.project-leverage"
sudo launchctl kickstart -k system/com.office-automate.edge
sudo launchctl kickstart -k system/com.office-automate.tunnel
```

The server, edge, and tunnel templates use `KeepAlive`; launchd restarts them after crashes or non-manual exits. The tunnel template passes `--no-autoupdate` so launchd remains the only process supervisor. The collector templates use `StartInterval` and `RunAtLoad`; they run once at load and then on their configured interval.

## Logs

```bash
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.err.log
sudo tail -F /var/log/office-automate/edge/office-automate-edge.out.log
sudo tail -F /var/log/office-automate/edge/office-automate-edge.err.log
sudo tail -F /var/log/office-automate/tunnel/office-automate-tunnel.out.log
sudo tail -F /var/log/office-automate/tunnel/office-automate-tunnel.err.log
```

## Quarantine Validation

After rendering the tunnel plist and PF anchor, run the quarantine validator as an operator check. Use a LAN probe that is expected to be reachable from the current user or from `--lan-control-user`; the validator requires that positive control before it accepts tunnel/edge denial as evidence. Prefer a numeric LAN IP to avoid treating DNS behavior as firewall evidence.

Before the public edge/controller split exists, render `--edge-user` to the tunnel user and omit edge-private credential checks because there is no separate edge credential owner yet:

```bash
scripts/security/validate-edge-quarantine.sh \
  --tunnel-user _office_tunnel \
  --edge-user _office_tunnel \
  --tunnel-readable /var/lib/office-automate/tunnel/config.yml \
  --tunnel-readable /var/lib/office-automate/tunnel/credentials.json \
  --protected-path "$OFFICE_AUTOMATE_CONFIG" \
  --protected-path "$OFFICE_AUTOMATE_DATABASE" \
  --protected-path "$OFFICE_AUTOMATE_REPO_ROOT" \
  --protected-path "$OFFICE_AUTOMATE_TELEMETRY_DB" \
  --origin-probe 127.0.0.1:8080 \
  --lan-probe 192.168.1.1:80
```

After the public edge/controller split is installed with distinct users, validate reciprocal credential separation explicitly:

```bash
scripts/security/validate-edge-quarantine.sh \
  --tunnel-user _office_tunnel \
  --edge-user _office_edge \
  --tunnel-readable /var/lib/office-automate/tunnel/config.yml \
  --tunnel-readable /var/lib/office-automate/tunnel/credentials.json \
  --tunnel-private /var/lib/office-automate/tunnel/config.yml \
  --tunnel-private /var/lib/office-automate/tunnel/credentials.json \
  --edge-readable /var/lib/office-automate/edge/config.yaml \
  --edge-private /var/lib/office-automate/edge/config.yaml \
  --protected-path "$OFFICE_AUTOMATE_CONFIG" \
  --protected-path "$OFFICE_AUTOMATE_DATABASE" \
  --protected-path "$OFFICE_AUTOMATE_REPO_ROOT" \
  --protected-path "$OFFICE_AUTOMATE_TELEMETRY_DB" \
  --origin-probe 127.0.0.1:8080 \
  --lan-control-user "$OFFICE_AUTOMATE_CONTROLLER_USER" \
  --lan-probe 192.168.1.1:80
```

The validation passes only if the tunnel/edge users can read the material they need, cannot read or traverse controller config/data/repos/telemetry, cannot read or traverse each other's private credentials unless explicitly required, can reach the approved loopback origin, and cannot connect to LAN/RFC1918 endpoints that the control user can reach.

## Qingping MQTT

`com.office-automate.server` owns the embedded MQTT broker for Qingping. Configure the physical Qingping device's Private Access MQTT host to the primary host LAN address and port from `qingping.mqtt_broker` and `qingping.mqtt_port`. The Rust server subscribes to `qingping/<device_mac>/up` and publishes interval commands to `qingping/<device_mac>/down`; no separate MQTT bridge or legacy broker is part of the target deployment.

## Android Device Enrollment

Use the repo-root `oa` wrapper for local device administration. It executes the same Rust `office-automate-server` binary and defaults `OFFICE_AUTOMATE_CONFIG` to `config.yaml` when run from the checkout:

```bash
./oa migrate
./oa register-device --device-name phone
./oa list-devices
./oa revoke-device <device-id>
```

`register-device` starts a short-lived LAN pairing listener, prints a six-character one-time code, and records audit events for registration creation, successful pairing, rejected codes, rejected CSR/proof attempts, and revocation. A pending code is invalidated after five failed CSR/proof attempts. Unknown-code audit entries store a hash of the submitted code, not the raw code.

The default device mTLS CA files are repo-local and gitignored:

| Path | Purpose |
| --- | --- |
| `certs/device-ca.pem` | CA certificate uploaded to Cloudflare Access as the client-certificate root. |
| `certs/device-ca.key` | Local signing key used only by `oa register-device`; do not upload it to Cloudflare. |

Override these paths with `--device-ca-cert`, `--device-ca-key`, `OFFICE_AUTOMATE_DEVICE_CA_CERT`, or `OFFICE_AUTOMATE_DEVICE_CA_KEY` if a deployment keeps private key material elsewhere.

## Cloudflare Tunnel Notes

The tunnel template only starts `cloudflared`; it does not define DNS, public hostnames, TLS, credentials, or application authorization. Configure those through Cloudflare and the `cloudflared` config file. The tunnel should forward only to the local Rust edge origin, and the edge/controller pair should continue enforcing OAuth/JWT plus the local controller IPC token. Public deployments must not use `trusted_networks`.
