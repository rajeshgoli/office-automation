# Primary Host Launchd Services

Ticket #75 adds launchd templates for the Rust primary-host deployment described in `docs/working/62_primary_host_modern_stack.md`.

The templates live in `scripts/launchd/primary-host/`:

| Template | Label | Purpose |
| --- | --- | --- |
| `com.office-automate.server.plist.template` | `com.office-automate.server` | Runs `office-automate-server serve --config <config>` as the core API, WebSocket, MQTT ingress, presence, and device-client process. |
| `com.office-automate.telemetry.plist.template` | `com.office-automate.telemetry` | Runs `office-automate-server collect --config <config> telemetry` on an interval. |
| `com.office-automate.project-leverage.plist.template` | `com.office-automate.project-leverage` | Runs `office-automate-server collect --config <config> leverage` on an interval. |
| `com.office-automate.tunnel.plist.template` | `com.office-automate.tunnel` | Runs `cloudflared tunnel --config <config> run <tunnel>` as the public transport process. |

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
| `__OFFICE_AUTOMATE_TELEMETRY_INTERVAL_SECONDS__` | Telemetry collector interval marker. The template ships with lintable default `1800`; render this integer for deployment. |
| `__OFFICE_AUTOMATE_PROJECT_LEVERAGE_INTERVAL_SECONDS__` | Project-leverage collector interval marker. The template ships with lintable default `7200`; render this integer for deployment. |
| `__CLOUDFLARED_BIN__` | Absolute path to the `cloudflared` binary. |
| `__CLOUDFLARED_CONFIG__` | Absolute path to the Cloudflare Tunnel config file. |
| `__CLOUDFLARED_TUNNEL__` | Cloudflare Tunnel name or UUID. |
| `__CLOUDFLARED_WORKING_DIRECTORY__` | Directory where `cloudflared` should run. |

Keep hostnames, public routes, credentials, and tunnel credential files in the Cloudflare config and deployment secrets, not in these templates.

## Install

Use LaunchAgents when the service needs the logged-in macOS user session, which is the target for internal presence polling.

```bash
mkdir -p "$HOME/Library/LaunchAgents" "$OFFICE_AUTOMATE_LOG_DIR"
cp rendered/com.office-automate.*.plist "$HOME/Library/LaunchAgents/"

launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist"
```

Validate loaded jobs:

```bash
launchctl print "gui/$(id -u)/com.office-automate.server"
launchctl print "gui/$(id -u)/com.office-automate.telemetry"
launchctl print "gui/$(id -u)/com.office-automate.project-leverage"
launchctl print "gui/$(id -u)/com.office-automate.tunnel"
```

## Unload

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.telemetry.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.project-leverage.plist"
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist"
```

## Restart

```bash
launchctl kickstart -k "gui/$(id -u)/com.office-automate.server"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.telemetry"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.project-leverage"
launchctl kickstart -k "gui/$(id -u)/com.office-automate.tunnel"
```

The server and tunnel templates use `KeepAlive`; launchd restarts them after crashes or non-manual exits. The collector templates use `StartInterval` and `RunAtLoad`; they run once at load and then on their configured interval.

## Logs

```bash
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-server.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-telemetry.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-project-leverage.err.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-tunnel.out.log
tail -F "$OFFICE_AUTOMATE_LOG_DIR"/office-automate-tunnel.err.log
```

## Cloudflare Tunnel Notes

The tunnel template only starts `cloudflared`; it does not define DNS, public hostnames, TLS, credentials, or application authorization. Configure those through Cloudflare and the `cloudflared` config file. The tunnel should forward to the local Rust server, and the Rust server should continue enforcing OAuth/JWT or trusted-network rules.
