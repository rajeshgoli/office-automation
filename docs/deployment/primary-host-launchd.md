# Primary Host Launchd Services

Ticket #75 adds launchd templates for the Rust primary-host deployment described in `docs/working/62_primary_host_modern_stack.md`.

The templates live in `scripts/launchd/primary-host/`:

| Template | Label | Purpose |
| --- | --- | --- |
| `com.office-automate.server.plist.template` | `com.office-automate.server` | Runs `office-automate-server serve --config <config>` as the core API, WebSocket, MQTT ingress, presence, and device-client process. |
| `com.office-automate.telemetry.plist.template` | `com.office-automate.telemetry` | Runs `office-automate-server collect --config <config> telemetry` on an interval. |
| `com.office-automate.project-leverage.plist.template` | `com.office-automate.project-leverage` | Runs `office-automate-server collect --config <config> leverage` on an interval. |
| `com.office-automate.tunnel.plist.template` | `com.office-automate.tunnel` | Runs `cloudflared tunnel --no-autoupdate --config <config> run <tunnel>` as the quarantined public transport process. |

LocalTunnel is intentionally not represented. Public access is Cloudflare Tunnel only; application auth remains owned by `office-automate-server`.

## Template Values

Render the templates with deployment-specific absolute paths before loading them:

| Placeholder | Meaning |
| --- | --- |
| `__OFFICE_AUTOMATE_ROOT__` | Repository checkout or release directory. |
| `__OFFICE_AUTOMATE_SERVER_BIN__` | Absolute path to the Rust `office-automate-server` binary. |
| `__OFFICE_AUTOMATE_CONFIG__` | Absolute path to the deployment config file. |
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
| `__OFFICE_AUTOMATE_EDGE_USER__` | Public HTTP edge user for the later edge/controller split. Until that split exists, render this to the same value as `__OFFICE_AUTOMATE_TUNNEL_USER__` in PF templates. |
| `__OFFICE_AUTOMATE_ORIGIN_PORTS__` | Loopback origin ports the tunnel/edge users may reach, for example `8080`. |

Keep hostnames, public routes, credentials, and tunnel credential files in the Cloudflare config and deployment secrets, not in these templates.

Raw `.plist.template` files are not loadable plists. Render every placeholder, including integer `StartInterval` values, then lint the rendered files:

```bash
plutil -lint rendered/com.office-automate.*.plist
```

## Public Edge Quarantine

`cloudflared` is public edge code. Do not run it as the logged-in user. Use a dedicated low-privilege tunnel account with no shell, no repo access, no controller config/data access, and only the tunnel config/credential/log paths it needs.

Recommended local ownership model:

| Path | Owner | Mode | Purpose |
| --- | --- | --- | --- |
| `/Library/LaunchDaemons/com.office-automate.tunnel.plist` | `root:wheel` | `0644` | LaunchDaemon wrapper that switches to the tunnel user. |
| `/var/lib/office-automate/tunnel/` | `_office_tunnel:_office_tunnel` | `0700` | Cloudflare tunnel config and credential directory. |
| `/var/log/office-automate/tunnel/` | `_office_tunnel:_office_tunnel` | `0700` | Tunnel stdout/stderr logs. |
| Controller config, data, repos, telemetry DBs | controller user/group only | `0600` files, `0700` directories | Must be unreadable and non-traversable by `_office_tunnel` and by the later public edge user. |
| Public edge config and credentials | public edge user/group only | `0600` files, `0700` directories | Must be unreadable and non-traversable by `_office_tunnel` unless the tunnel explicitly needs them. |

Create the tunnel account through your normal macOS account-management path or MDM. The account must be non-login and dedicated to Office Automate tunnel transport. The examples below assume `_office_tunnel`.

```bash
sudo install -d -o _office_tunnel -g _office_tunnel -m 0700 /var/lib/office-automate/tunnel
sudo install -d -o _office_tunnel -g _office_tunnel -m 0700 /var/log/office-automate/tunnel
sudo install -o _office_tunnel -g _office_tunnel -m 0600 "$CLOUDFLARED_CONFIG" /var/lib/office-automate/tunnel/config.yml
sudo install -o _office_tunnel -g _office_tunnel -m 0600 "$CLOUDFLARED_CREDENTIALS" /var/lib/office-automate/tunnel/credentials.json
```

The Cloudflare config in `/var/lib/office-automate/tunnel/config.yml` should reference the credential copy in that same directory and route only to the loopback or Unix-socket origin validated by `office-automate-server validate`.

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

Use LaunchAgents only for jobs that need the logged-in macOS user session. The server currently owns presence polling, so it remains a LaunchAgent until the public edge/controller split is implemented. The tunnel does not need the user session and must run as a LaunchDaemon under the dedicated tunnel user.

```bash
mkdir -p "$HOME/Library/LaunchAgents" "$OFFICE_AUTOMATE_LOG_DIR"
cp rendered/com.office-automate.server.plist "$HOME/Library/LaunchAgents/"
cp rendered/com.office-automate.telemetry.plist "$HOME/Library/LaunchAgents/"
cp rendered/com.office-automate.project-leverage.plist "$HOME/Library/LaunchAgents/"
sudo install -o root -g wheel -m 0644 rendered/com.office-automate.tunnel.plist /Library/LaunchDaemons/com.office-automate.tunnel.plist

launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
sudo launchctl bootstrap system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

Validate loaded jobs:

```bash
launchctl print "gui/$(id -u)/com.office-automate.server"
launchctl print "gui/$(id -u)/com.office-automate.telemetry"
launchctl print "gui/$(id -u)/com.office-automate.project-leverage"
sudo launchctl print system/com.office-automate.tunnel
```

## Unload

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
sudo launchctl bootout system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

## Restart

```bash
launchctl kickstart -k "gui/$(id -u)/com.office-automate.server"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.telemetry"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.project-leverage"
sudo launchctl kickstart -k system/com.office-automate.tunnel
```

The server and tunnel templates use `KeepAlive`; launchd restarts them after crashes or non-manual exits. The tunnel template passes `--no-autoupdate` so launchd remains the only process supervisor. The collector templates use `StartInterval` and `RunAtLoad`; they run once at load and then on their configured interval.

## Logs

```bash
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.err.log
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
  --edge-readable "$OFFICE_AUTOMATE_EDGE_CREDENTIALS" \
  --edge-private "$OFFICE_AUTOMATE_EDGE_CREDENTIALS" \
  --protected-path "$OFFICE_AUTOMATE_CONFIG" \
  --protected-path "$OFFICE_AUTOMATE_DATABASE" \
  --protected-path "$OFFICE_AUTOMATE_REPO_ROOT" \
  --protected-path "$OFFICE_AUTOMATE_TELEMETRY_DB" \
  --origin-probe 127.0.0.1:8080 \
  --lan-control-user "$OFFICE_AUTOMATE_CONTROLLER_USER" \
  --lan-probe 192.168.1.1:80
```

The validation passes only if the tunnel/edge users can read the material they need, cannot read or traverse controller config/data/repos/telemetry, cannot read or traverse each other's private credentials unless explicitly required, can reach the approved loopback origin, and cannot connect to LAN/RFC1918 endpoints that the control user can reach.

## Cloudflare Tunnel Notes

The tunnel template only starts `cloudflared`; it does not define DNS, public hostnames, TLS, credentials, or application authorization. Configure those through Cloudflare and the `cloudflared` config file. The tunnel should forward only to the local Rust origin, and the Rust server should continue enforcing OAuth/JWT. Public deployments must not use `trusted_networks`.
