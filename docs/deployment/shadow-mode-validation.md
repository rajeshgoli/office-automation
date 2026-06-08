# Rust Shadow-Mode Validation

Ticket #77 validates the Rust primary-host backend before backend/MQTT cutover. Python remains the active climate controller during this procedure.

Shadow mode means:

- ERV active writes are disabled.
- HVAC active writes are disabled.
- Rust may read live devices and serve APIs.
- Python continues to receive the active Qingping feed and remains the only climate controller.
- Cloudflare Tunnel may point at the Rust shadow server only for validation traffic; do not remove the legacy access path during this ticket.

## Inputs

Set deployment-specific values outside the repo:

```bash
export OFFICE_AUTOMATE_CONFIG="/absolute/path/to/office-automate.yaml"
export OFFICE_AUTOMATE_SHADOW_BASE_URL="http://127.0.0.1:9001"
export OFFICE_AUTOMATE_SHADOW_PUBLIC_URL="https://office.example.com"
export CLOUDFLARED_CONFIG="/absolute/path/to/cloudflared/config.yml"
export OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE="/absolute/path/to/cloudflare-evidence.json"
```

The Office Automate config used for this ticket must have:

```yaml
erv:
  active_control_enabled: false

mitsubishi:
  active_control_enabled: false
```

Do not run this validation with active-control flags enabled.

## Start Rust Shadow Server

Build the Rust binary:

```bash
cargo build --manifest-path rust/office-automate-server/Cargo.toml --release
```

Start the Rust server on a non-production port:

```bash
./target/release/office-automate-server serve \
  --config "$OFFICE_AUTOMATE_CONFIG"
```

Keep the Python backend running as the active controller. Do not reconfigure Qingping exclusively to the Rust broker unless the Python controller still receives mirrored fresh readings.

## Validate Cloudflare Tunnel

Cloudflare Tunnel is the public transport for this cutover. LocalTunnel is not part of the target architecture.

Validate the tunnel config separately:

```bash
test -r "$CLOUDFLARED_CONFIG"
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

If the installed `cloudflared` uses a different subcommand shape, run the deployed version's equivalent ingress validation. The tunnel should forward the shadow hostname to `OFFICE_AUTOMATE_SHADOW_BASE_URL`, and application auth remains enforced by `office-automate-server`.

## Capture Cloudflare Drift Evidence

Local `cloudflared` YAML validation is necessary but not enough. Capture a sanitized Cloudflare API export, Terraform export, or dashboard screenshot manifest that proves the account-side state for the exact public hostname:

```json
{
  "source": "cloudflare_api",
  "captured_at": "2026-06-07T14:30:00-07:00",
  "hostname": "office.example.com",
  "access_application": {
    "hostname": "office.example.com",
    "require_access": true,
    "policies": [
      {
        "name": "allow-device-mtls",
        "action": "Service Auth",
        "includes_public": false,
        "includes_common_name": true,
        "includes_valid_certificate": false
      },
      {
        "name": "allow-rajesh",
        "action": "Allow",
        "includes_public": false
      }
    ]
  },
  "dns": {
    "wildcard_records": []
  },
  "tunnel": {
    "hostname": "office.example.com",
    "origin_service": "http://127.0.0.1:9001",
    "private_network_routes": [],
    "final_ingress_service": "http_status:404"
  },
  "access_audit": {
    "checked_at": "2026-06-07T14:35:00-07:00",
    "unauthenticated_blocks_seen": true,
    "authenticated_success_seen": true
  }
}
```

The evidence file must not contain API tokens, service-token secrets, tunnel credential JSON, bearer tokens, or raw Access log payloads. Store the raw export/screenshots privately and keep only this sanitized manifest in the deployment log directory.

## Run Shadow Validation

Run the Rust validation command:

```bash
./target/release/office-automate-server validate \
  --config "$OFFICE_AUTOMATE_CONFIG" \
  shadow \
  --base-url "$OFFICE_AUTOMATE_SHADOW_BASE_URL" \
  --public-url "$OFFICE_AUTOMATE_SHADOW_PUBLIC_URL" \
  --cloudflared-config "$CLOUDFLARED_CONFIG" \
  --cloudflare-evidence "$OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE" \
  --max-air-quality-age-seconds 300
```

When the public URL is supplied, the validator sends unauthenticated public probes that must be blocked by Cloudflare Access before origin. To also automate the authenticated public `/status` probe, provide an operator-only Cloudflare Access service token:

```bash
export OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID="..."
export OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET="..."
```

If no service token is supplied, manually verify browser/PWA and mobile access through Cloudflare Access plus Office auth and add:

```bash
--manual-public-access-verified-at "$(date -Iseconds)"
```

Do not use Android app credentials or bundled APK secrets for these operator validation headers.

The command validates:

- ERV and HVAC active-control gates are disabled.
- Copied SQLite databases pass `PRAGMA quick_check`.
- Office history tables are readable through the Rust compatibility query path.
- ERV local Tuya status can be read without writing.
- HVAC Kumo status can be read without writing.
- YoLink cloud auth and inventory read succeed.
- `/status` has the expected compatibility shape.
- `/status.air_quality.last_update` is fresh enough to prove Rust sees the shadow Qingping feed.
- `/history`, `/history/project-leverage`, `/apps/office-climate/meta.json`, and `/auth/login` retain their expected interface behavior.
- `/ws` accepts the configured auth mode and delivers the initial status frame.
- The Cloudflare tunnel config publishes only the exact public hostname, routes it to a loopback/Unix origin, has no wildcard hostname/private-network route, and ends in `http_status:404` when supplied.
- The sanitized Cloudflare evidence proves the Access app, no Bypass/public policies, device mTLS through per-device Common Name selectors rather than broad Valid Certificate, exact hostname, no wildcard DNS, no private routes, final deny rule, and Access audit allow/deny observations when supplied.
- Unauthenticated public HTTP routes and `/ws` are blocked by Cloudflare Access before origin when a public URL is supplied.
- Public `/status` reaches Rust through Cloudflare Access and Office auth when a service token or manual verification timestamp is supplied.

For OAuth deployments, local automated HTTP and WebSocket validation uses `google_oauth.jwt_secret` to mint a validation JWT for the first allowed email. If `jwt_secret` is intentionally omitted, the validator falls back to the first `trusted_networks` entry for the local shadow URL. Do not use that trusted-network fallback as the public Cloudflare auth path.

`--skip-live-devices` and `--skip-http-interface` are available for local development only. Do not use them for the final shadow validation gate.

## Manual Interface Checks

After the command passes, validate browser/mobile auth behavior against both local and Cloudflare URLs:

```bash
open "$OFFICE_AUTOMATE_SHADOW_BASE_URL/"
open "$OFFICE_AUTOMATE_SHADOW_PUBLIC_URL/"
```

Confirm:

- Browser/PWA can authenticate and load `/status`.
- Browser/PWA WebSocket receives live updates after auth.
- Mobile app can authenticate and read status over the Cloudflare URL.
- Manual ERV/HVAC buttons are not used during shadow validation.

## Result Record

Record the result before starting the cutover ticket:

```text
Shadow validation date:
Rust commit:
Config path:
Snapshot directory from #76:
Python active controller URL:
Rust shadow base URL:
Rust Cloudflare URL:
cloudflared ingress validation:
office-automate-server validate shadow output:
Manual browser/PWA auth:
Manual mobile auth:
Known skips or follow-ups:
Cutover approved by:
```

Do not start ticket #78 until this record is complete and reviewed.
