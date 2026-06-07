# Incident Response And Kill Switch

Ticket #114 covers the operational response for suspected Office Automate compromise. The first objective is to remove public reachability before rotating secrets. The second objective is to preserve useful evidence without leaking raw secrets into chat, tickets, or PRs.

## Severity Triggers

Treat any of these as an incident:

- Cloudflare Access policy, tunnel route, DNS, or hostname drift.
- Unexpected public success from an unauthenticated probe to `office.rajeshgo.li`.
- Unknown Android mTLS device, repeated pairing failures, or unexpected device revocation.
- Artifact upload or metadata change you did not initiate.
- MQTT/Qingping wrong MAC, unexpected client, value-range rejection spike, or raw payload flood.
- ERV/HVAC/YoLink/Kumo command failures after valid credentials recently worked.
- Unknown listener, launchd restart loop, DB integrity failure, or unexplained binary hash change.

## Handling Rules

- Capture evidence before rotation when it is safe to do so.
- Store raw evidence only in a local encrypted location. FileVault-on local storage is acceptable for the capture script; otherwise use an encrypted disk image or pass `--allow-unencrypted-local` only when you deliberately accept that risk.
- Do not paste raw `ps eww`, logs, config files, Cloudflare exports, Access JWTs, cookies, Android cert material, or token-bearing output into tickets, chats, PRs, or issue comments.
- Share redacted excerpts, hashes, timestamps, process IDs, file modes, and exact commands instead of raw secret-bearing files.

## 1. Capture Evidence

Run this from the repo root before changing secrets or restarting services:

```bash
scripts/security/capture-incident-evidence.sh \
  --config "${OFFICE_AUTOMATE_CONFIG:-config.yaml}" \
  --database "${OFFICE_AUTOMATE_DATABASE:-data/office_climate.db}" \
  --server-bin "${OFFICE_AUTOMATE_SERVER_BIN:-target/release/office-automate-server}" \
  --cloudflared-config "${CLOUDFLARED_CONFIG:-/var/lib/office-automate/tunnel/config.yml}"
```

The script writes raw files under `tmp/incident-evidence/<timestamp>/` by default, creates redacted summaries under `redacted/`, and refuses to run when FileVault is not reported as enabled unless `--allow-unencrypted-local` is passed.

Also preserve Cloudflare-side evidence before changing policies:

```bash
cloudflared tunnel list
cloudflared tunnel info office
cloudflared tunnel route ip show
cloudflared tunnel ingress validate --config "$CLOUDFLARED_CONFIG"
```

If the Cloudflare dashboard is the available source of truth, export or screenshot these exact fields before edits: Access application destinations, policy list and order, no Bypass policy, Service Auth valid-certificate policy, login allow policy, tunnel public hostname route, DNS records for `office.rajeshgo.li`, no wildcard DNS, no private network routes, and Access audit logs around the incident window.

## 2. Kill Public Access

Stop the tunnel first. In the hardened target deployment it is a system LaunchDaemon:

```bash
sudo launchctl bootout system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

If the host is still using the older user-session tunnel job, use:

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist"
```

If the public edge is deployed separately, stop it after the tunnel:

```bash
sudo launchctl bootout system /Library/LaunchDaemons/com.office-automate.edge.plist
```

Confirm public access is gone:

```bash
curl -i https://office.rajeshgo.li/status
curl -i https://office.rajeshgo.li/auth/login
```

Both requests must fail before reaching the origin. Acceptable results are Cloudflare Access denial, tunnel unavailable, DNS failure during emergency DNS removal, or connection failure. A JSON Office Automate origin response means the kill switch did not work.

## 3. Keep Local Climate Safety Running

The local controller should continue without public access. Check the controller job:

```bash
launchctl print "gui/$(id -u)/com.office-automate.server"
```

Run non-mutating smoke checks:

```bash
./oa smoke erv
./oa smoke hvac
./oa smoke presence
```

If the controller itself is suspected compromised, leave the tunnel down and stop the controller only after deciding whether additional local evidence is needed:

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.server.plist"
```

Manual fallback while the controller is down is the physical ERV/HVAC controls and vendor apps on the LAN.

## 4. Rotate In Dependency Order

Rotate from the public edge inward so a still-exposed public component cannot immediately consume fresh internal credentials.

1. Keep the Cloudflare tunnel stopped.
2. Rotate Cloudflare tunnel credentials and verify the tunnel still has no private network routes.
3. Review Cloudflare Access app state: exact hostname, no Bypass policy, Service Auth valid certificate policy first, login allow policy second, no wildcard DNS, final deny/404 route.
4. Revoke or rotate Android device registrations as needed:

   ```bash
   ./oa list-devices
   ./oa revoke-device <device-id>
   ```

5. Rotate Office JWT secret and Google OAuth client secret if browser sessions or origin config may be exposed.
6. Revoke bad Android artifacts, remove bad metadata, and publish a known-good signed APK if update metadata may be exposed.
7. Rotate ERV local key if local device credentials or config may be exposed.
8. Rotate Kumo credentials.
9. Rotate YoLink credentials.
10. Rotate any remaining operator tokens used for Cloudflare API evidence, GitHub release work, or local automation.

Record each rotation timestamp in the incident notes. Do not store new secret values in those notes.

## 5. Recovery Validation

Before restoring public access:

```bash
./oa migrate
./oa smoke erv
./oa smoke hvac
./oa smoke presence
sqlite3 data/office_climate.db "PRAGMA quick_check;"
scripts/security/validate-edge-quarantine.sh \
  --tunnel-user _office_tunnel \
  --edge-user _office_edge \
  --tunnel-readable /var/lib/office-automate/tunnel/config.yml \
  --tunnel-readable /var/lib/office-automate/tunnel/credentials.json \
  --edge-readable /var/lib/office-automate/edge/config.yaml \
  --protected-path "${OFFICE_AUTOMATE_CONFIG:-config.yaml}" \
  --protected-path "${OFFICE_AUTOMATE_DATABASE:-data/office_climate.db}" \
  --protected-path "$PWD" \
  --origin-probe 127.0.0.1:8080 \
  --lan-control-user "$(id -un)" \
  --lan-probe 192.168.5.1:80
```

Then reload the public edge and tunnel:

```bash
sudo launchctl bootstrap system /Library/LaunchDaemons/com.office-automate.edge.plist
sudo launchctl bootstrap system /Library/LaunchDaemons/com.office-automate.tunnel.plist
```

Run cutover validation with Cloudflare evidence and Access service credentials:

```bash
./oa validate cutover \
  --cloudflared-config "$CLOUDFLARED_CONFIG" \
  --cloudflare-evidence "$OFFICE_AUTOMATE_CLOUDFLARE_EVIDENCE" \
  --cloudflare-access-client-id "$OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_ID" \
  --cloudflare-access-client-secret "$OFFICE_AUTOMATE_CLOUDFLARE_ACCESS_CLIENT_SECRET"
```

Final public checks:

- Unauthenticated public requests to `/status`, `/auth/login`, `/auth/callback`, `/auth/device/start`, `/auth/device/poll`, `/apps/office-climate/meta.json`, `/apk`, `/ws`, static `.json` and `.png` paths, and `/deploy/` must be blocked by Cloudflare before origin.
- Authenticated browser access for the operator must reach the dashboard.
- Enrolled Android mTLS access must reach the API without browser OAuth.
- Unknown Android mTLS certificates must be blocked by Cloudflare or rejected by Office Automate enrollment checks.
- Local controller smoke checks must still pass.

## 6. Closeout

Keep the evidence directory until the incident is closed. The closeout note should include:

- Incident window in local time and UTC.
- Initial trigger and first containment time.
- Public kill-switch command used and validation result.
- Hash of the evidence directory manifest, not raw secret-bearing files.
- Secrets rotated, by name and timestamp only.
- Recovery validation commands and results.
- Follow-up tickets for any failed validation, missing alert, missing Cloudflare evidence, or manual-only step.
