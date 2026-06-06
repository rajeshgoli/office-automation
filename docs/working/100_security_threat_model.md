# Office Automate Security Threat Model

**Issue:** #100
**Status:** Draft
**Last updated:** 2026-06-06

## Goal

Ruggedize the Rust-only Office Automate deployment so a remote attacker cannot use the public application path, Cloudflare Tunnel, the Rust HTTP/WebSocket server, app update surfaces, mobile bootstrap flow, or device integrations to gain a foothold on the local network.

The top security objective is stricter than "the app requires auth":

> If an unknown 0-day exists in any public-facing parser, HTTP/WebSocket stack, auth handler, artifact handler, frontend dependency, mobile bootstrap path, or tunnel component, exploiting it from outside must not give the attacker useful access to the local LAN, device credentials, local repos, SQLite data, or the user's macOS account.

This document intentionally assumes that one layer can fail. The target posture is defense-in-depth with deny-by-default public access, minimal origin reachability, enforceable least privilege, and fast containment.

## Current Security Conclusion

The Rust port removes the Python runtime and a large dependency chain, but the current production shape is not bulletproof against the stated 0-day goal if `cloudflared` runs as a GUI LaunchAgent with normal user-level filesystem/LAN reach and the public tunnel forwards directly into the same all-in-one Rust process that:

- holds device secrets,
- controls ERV/HVAC devices,
- reads/writes SQLite state,
- scans local git repositories,
- reads session-manager telemetry databases,
- serves WebSocket and multipart upload traffic,
- runs in the logged-in user session for presence detection,
- has normal user-level filesystem access and LAN egress.

Cloudflare Access and Office OAuth reduce reachability, but they do not fully contain an exploit after a request reaches the origin. To meet the stated goal, Office Automate needs an explicit security boundary between the public web surface and the LAN/device-control authority.

## Security Supersedes Compatibility

`docs/working/62_primary_host_modern_stack.md` preserved Python-era outward compatibility for the Rust cutover. This threat model supersedes that compatibility spec wherever the old behavior is unsafe for production.

The following prior compatibility behaviors are no longer acceptable on the public production path:

- wildcard CORS,
- unauthenticated `/apps/*` and `/apk`,
- exempting `/auth/login` or static JSON from Cloudflare Access,
- Basic-auth-only public operation,
- broad `trusted_networks` bypass on a listener reached through `cloudflared`,
- `0.0.0.0` production HTTP binding,
- a public all-in-one process with device credentials, repo access, SQLite access, and LAN egress,
- public `/deploy` availability without explicit admin authorization.

Where compatibility is still needed, it must be kept local-only, behind Access, or behind a narrower signed/bootstrap-specific API.

## Desired End State

1. **No inbound public ports:** The router/firewall exposes no inbound ports to the Mac. Public access is Cloudflare Tunnel only.
2. **Cloudflare deny-by-default:** Every public hostname is protected by Cloudflare Access or explicitly blocked. No wildcard public hostname reaches Office Automate.
3. **Access-before-origin:** Unauthenticated public requests, including `/auth/login`, `/apps/*`, `/apk`, `/ws`, `/status`, `/deploy/*`, and the SPA, are blocked by Cloudflare Access before they reach Office Automate.
4. **HTTP origin is not LAN-exposed:** The public HTTP origin listens only on loopback or a Unix socket reachable by `cloudflared`, not on `0.0.0.0` or a LAN IP.
5. **Tunnel process is public edge code:** `cloudflared` runs under a quarantined tunnel/edge account, cannot read controller config/data/repos/telemetry, and cannot reach LAN/RFC1918 destinations except the local origin path.
6. **Public HTTP edge has no LAN authority:** The process that parses public HTTP/WebSocket traffic has no device credentials, no local repo access, no climate SQLite access, no telemetry DB access, and no arbitrary LAN egress.
7. **Local controller is private:** Climate control, MQTT ingress, presence, and cloud device clients run behind local-only controller boundaries.
8. **Device/cloud ingesters are constrained:** YoLink, Kumo, Tuya/ERV, Qingping, and presence helpers parse untrusted inputs with least privilege and typed IPC into the controller.
9. **MQTT is constrained:** The Qingping broker is reachable only from the expected device/network segment and accepts only the expected topic, device identity, payload shape, value ranges, and rate.
10. **Auth is layered:** Cloudflare Access, Office Automate OAuth/JWT, route-level authorization, admin authorization, CSRF/cookie protections, and mobile bootstrap rules all fail closed.
11. **Secrets are least-available:** Secrets are file-permission protected, isolated from public-facing processes, rotated after any suspected compromise, and never logged.
12. **Observability catches drift:** Cloudflare config, Access policies, DNS/tunnel routes, launchd state, listening sockets, auth failures, MQTT clients, and collector activity are monitored.
13. **Kill switch exists:** One documented command path can immediately remove all public reachability without stopping local climate safety.

## Trust Boundaries

| Boundary | Trusted side | Untrusted side | Key risk |
| --- | --- | --- | --- |
| Public Internet to Cloudflare | Cloudflare account configuration | Any internet client | Misconfigured Access/DNS/WAF exposes app directly. |
| Cloudflare Access to `cloudflared` | Verified Access identity and token policy plus quarantined tunnel account | Cloudflare edge/request stream | Missing Access app, Bypass policy, or tunnel RCE forwards or pivots from internet traffic. |
| `cloudflared` process to local host | Loopback/Unix-socket origin path only | Controller secrets, local files, repos, LAN/RFC1918 networks | Tunnel component 0-day becomes local network foothold. |
| `cloudflared` to Rust HTTP edge | Exact local origin route | Public HTTP/WebSocket payloads | Origin parser/framework 0-day. |
| Rust HTTP edge to controller | Narrow local API/IPC | Public web process | Edge compromise becomes climate/LAN compromise. |
| Controller to LAN devices | Device clients and validated payloads | LAN devices, spoofed LAN clients | MQTT or device protocol spoofing. |
| Device/cloud ingesters to controller | Typed validated readings/events | YoLink/Kumo/Tuya/Qingping/presence payloads | Parser/client 0-day lands in climate controller. |
| Controller to cloud services | Google, YoLink, Kumo, Cloudflare | Third-party APIs and tokens | Token theft, API abuse, supply-chain outage. |
| Android client to public API | Verified app identity, user identity, signed updates | Other Android apps, LAN observers, malicious APKs | Bearer token capture or malicious update install. |
| Local user account to service files | Config, data, logs, launchd plists | Local malware, compromised process | Secrets and DB exfiltration. |
| Build/update pipeline to runtime | Signed/reviewed releases | Dependency or artifact tampering | Malicious binary/APK/config deployed. |

## Attack Surface Inventory

### Public Network Surface

| Surface | Current behavior | Risk | Hardening target |
| --- | --- | --- | --- |
| Cloudflare public hostname | Cloudflare Tunnel forwards public hostname to local Rust server. | Missing Access app, Bypass policy, wildcard DNS, or broad tunnel route exposes full app. | Exact hostname only, Access required, no Bypass policy, final 404 ingress, negative public probes. |
| `cloudflared` daemon | Outbound tunnel process supervised by launchd. | Tunnel credential theft, tunnel RCE, or config drift can publish or reach unintended local services. | Quarantined tunnel/edge account, credential file isolated from controller, pinned binary, no private-network routing, no LAN egress except origin, dashboard/API drift validation. |
| HTTP origin listener | Rust server default config is `0.0.0.0:8080`; deployment may override. | LAN clients can hit the full app directly; public tunnel RCE lands in all-in-one process. | Production public origin bound to `127.0.0.1` or Unix socket only; non-loopback requires explicit local-only config. |
| `/ws` | WebSocket upgrade path performs route-level first-message/header auth. | WebSocket parser/path is exposed before app auth completes if Access is missing. | Access before origin, origin auth before first status, message size/idle limits, connection caps. |
| Static frontend and SPA fallback | OAuth mode skips origin auth for `/`, `/index.html`, `/assets/*`, `.png`, `.json`. | Static parser/JS exposure; vulnerable JS can steal localStorage tokens. | Access before all public static routes; move browser auth to HttpOnly cookies; CSP. |
| `/auth/login`, `/auth/callback` | OAuth bootstrap endpoints are reachable when app auth permits. Android currently calls `/auth/login?platform=android` directly and expects JSON. | Exempting login for mobile re-exposes public parser/auth code and conflicts with Access-everywhere. | `/auth/login` is Access-protected on public host; mobile gets a defined Access-compatible bootstrap path. |
| `/auth/device/start`, `/auth/device/poll` | Public when OAuth is enabled. | Device-code endpoint can be abused for flow spam or phishing assist. | Disable unless needed; Access-required; rate limit; do not return refresh tokens unless required. |
| `/apps/*`, `/apk` | Artifact download routes have been unauthenticated for compatibility. | Public APK disclosure, fingerprinting, cache abuse, malicious update path if metadata is abused. | Access-protected or signed update API; full digest metadata; client digest and cert verification. |
| `/deploy/{app}` | Protected only when OAuth/Basic is configured; in Open mode middleware does not protect it. Any allowed OAuth email can upload because there is no admin role. | Multipart parser/file-write 0-day, disk fill, or malicious APK upload. | Admin-only, disabled in Open mode, unavailable through public edge, strict body/rate limits. |

### Local/LAN Surface

| Surface | Current behavior | Risk | Hardening target |
| --- | --- | --- | --- |
| HTTP if bound to LAN | Same HTTP app may be reachable by any LAN host when `host=0.0.0.0`. | Any compromised LAN device can attack full app. | Bind public HTTP to loopback; if LAN UI is needed, route through Access or a separate LAN-only listener with auth. |
| Trusted-network bypass | App can classify requests as trusted from configured CIDRs; `cloudflared` reaches origin as a loopback peer and can carry client IP headers. | Public requests forwarded by `cloudflared` can be misclassified as trusted if `X-Forwarded-For` falls in `trusted_networks`. | Disable trusted-network bypass on public listener unless Access token validation already succeeded; never use it as the only public auth path. |
| MQTT broker | Embedded `rumqttd` accepts unauthenticated v4 MQTT; max payload 20 KB; max connections 128. | LAN client can spoof Qingping sensor readings or flood connections. | Bind to exact interface, firewall to Qingping IP/VLAN, source/client constraints, payload MAC equality, rate/range/freshness checks. |
| `/occupancy` compatibility route | Accepts external presence reports when authenticated/trusted. | False presence can drive automation decisions. | Prefer internal presence poller; require auth for external reporters; deprecate if unused. |
| Tuya local ERV | Controller sends local commands with local key. | Secret theft gives device control; device protocol parser bugs. | Keep secret away from public edge; active-write gates; smoke checks; rotate local key if leaked. |
| Kumo Cloud HVAC | Controller uses username/password/device serial. | Token theft or credential abuse. | Store outside public edge; rate limit commands; rotate on compromise. |
| YoLink HTTP/MQTT | Controller authenticates to cloud and subscribes to events. | Credential theft or event spoofing at cloud boundary. | Store outside public edge; verify device IDs; reconnect backoff. |
| Device/cloud ingress parsers | YoLink/Kumo/Tuya/Qingping/presence payloads are parsed inside the controller path today. | Parser/client RCE or memory corruption lands in the privileged controller. | Split ingesters/helpers by source, least privilege, no repo/telemetry/user-home access, bounded egress, typed IPC into controller. |

### Local Host Surface

| Surface | Current behavior | Risk | Hardening target |
| --- | --- | --- | --- |
| launchd server job | Runs as the logged-in user for macOS presence access. | RCE has user account privileges and access to local files/repos. | Split public edge from controller/presence; run edge as dedicated low-privilege user. |
| Tunnel process | `cloudflared` launchd template currently runs as a GUI LaunchAgent without a dedicated user/sandbox boundary. | `cloudflared` RCE has user-level filesystem and LAN reach. | Run as quarantined tunnel/edge user, separate tunnel credentials, deny controller file reads and LAN/RFC1918 egress except origin. |
| Edge process | Not yet split; public parser currently shares server process. | A parser 0-day can read config/data/repos and reach LAN. | Separate user/process, no config/data/repo read access, PF denies LAN egress, only controller IPC. |
| Config file | Contains OAuth, ERV, Kumo, YoLink, Cloudflare-adjacent values depending on deployment. | Secret theft compromises devices and cloud accounts. | `0600`, owner-only dir, edge cannot read, no world-readable backups/logs. |
| SQLite DBs | Climate state, history, telemetry, app metadata. | Privacy/data exfiltration; corruption. | File permissions, online backups, integrity checks, controller-only write access. |
| App artifacts | APK uploads and metadata under data dir. | Malicious APK if deploy route abused; rollback to bad artifact. | Admin-only upload, full SHA-256 metadata, signing cert pinning, revocation/rollback metadata. |
| Telemetry collector | Runs `git log` across configured repos and reads session-manager DBs. | Repo path traversal/malicious config surface; local privacy exposure. | Static allowlist of repo roots; no public edge access to repo filesystem or telemetry DBs. |
| Logs | stdout/stderr include operational errors. | Secrets accidentally logged; logs disclose local topology. | Redaction policy, log-permission checks, bounded retention. |

### Supply Chain Surface

| Surface | Risk | Hardening target |
| --- | --- | --- |
| Rust crates | Compromised crate or transitive dependency. | `Cargo.lock` committed, `cargo audit`, `cargo deny`, minimal direct deps, release builds from clean checkout. |
| `cloudflared` | Vulnerability or malicious binary update. | Pinned install path/version, controlled update cadence, hash/provenance check, `--no-autoupdate` under launchd. |
| Frontend/npm | XSS or dependency compromise steals tokens. | Lockfile audit, CSP, no localStorage auth tokens, SRI where practical. |
| Android/Gradle | Malicious build plugin, token leakage, APK update path. | Lock dependencies, signed APK, secure token storage, verified app links or safer redirect, digest and signing identity checks. |
| GitHub/PR flow | Malicious code merged/deployed. | Required review, CI tests, security checklist for auth/network changes. |
| Cloudflare account | Account takeover or drift exposes origin or private routes. | MFA/hardware keys, least-privilege API tokens, configuration export checks, Access audit log review. |

## Threat Scenarios

### Scenario 1: Public HTTP 0-Day In Rust/Axum/Multipart Parser

**Path:** attacker reaches public hostname, passes or bypasses outer auth, sends exploit payload to HTTP/WebSocket/multipart route.

**Impact today:** If exploit becomes code execution in the all-in-one server process, attacker may get user-level filesystem access, config secrets, DBs, local git repos, and LAN egress to devices.

**Required controls:**

- Cloudflare Access blocks unauthenticated public requests before origin.
- HTTP origin listens only on loopback/Unix socket.
- Public parser process runs as a low-privilege user with no device secrets, repo/DB access, or LAN egress.
- Device controller runs separately and accepts only narrow, authenticated local commands.
- `/deploy` disabled on public edge by default.
- macOS PF/application firewall denies public-edge user outbound LAN access except explicitly needed local IPC.

### Scenario 2: Cloudflare Tunnel Misconfiguration

**Path:** tunnel ingress publishes a wildcard hostname, private IP route, SSH, MQTT, or full LAN CIDR.

**Impact today:** External clients could reach unintended services, or a compromised Cloudflare account could pivot into the private network.

**Required controls:**

- One exact public hostname route to the HTTP origin.
- Final ingress rule returns 404.
- No wildcard DNS for Office Automate.
- No Cloudflare private-network routing/WARP routing unless separately threat-modeled.
- Cloudflare "Require Access protection" enabled for the account or zone.
- Access app has no Bypass policy or public allow rule.
- Deployment validation checks Cloudflare state through API/Terraform export or dashboard evidence, not only local YAML.
- Negative public probes prove unauthenticated `/auth/login`, `/apps/*`, `/apk`, `/ws`, `/status`, and `/deploy/*` are blocked before origin.

### Scenario 3: `cloudflared` Component 0-Day

**Path:** attacker exploits an unknown vulnerability in the tunnel process or a tunnel-protocol parser before the request reaches the Rust origin.

**Impact today:** If `cloudflared` runs as the logged-in user, RCE can read local files, controller config, tunnel credentials, repos, telemetry DBs, or connect to LAN devices even if the Rust origin is perfectly isolated.

**Required controls:**

- Treat `cloudflared` as public edge code, not a trusted local daemon.
- Run `cloudflared` under a quarantined tunnel/edge account.
- Tunnel account cannot read controller config, data dir, SQLite DBs, repos, telemetry DBs, controller launchd plists, or user home content.
- Tunnel credentials are readable only by the tunnel account and are not colocated with controller secrets.
- PF/application firewall denies tunnel account outbound LAN/RFC1918 egress except the loopback or Unix-socket origin path.
- Validation attempts file reads and LAN connects as the tunnel account and proves denial.

### Scenario 4: Trusted-Network Bypass Through `cloudflared`

**Path:** public request is forwarded through `cloudflared`, reaches origin from loopback, and carries a client IP header that falls inside `trusted_networks`.

**Impact today:** Protected routes could become public if trusted-network bypass is enabled on the public listener. Current direct-spoofing tests are useful but do not prove that a `cloudflared` loopback request cannot be trusted.

**Required controls:**

- Trusted-network bypass is disabled entirely on public listeners unless Access token validation has already succeeded.
- Public listener is not shared with direct LAN traffic.
- `127.0.0.1/32` is never used as a trust shortcut for proxied public requests.
- Forwarded client headers are accepted only from known local proxy peers and only after Access context is validated.
- Validation includes a `cloudflared`-shaped loopback request with `X-Forwarded-For` inside `trusted_networks` and proves it does not bypass Office auth.

### Scenario 5: Android Bootstrap Under Access-Everywhere

**Path:** Android currently calls `/auth/login?platform=android` directly and expects JSON before launching browser OAuth. Access-everywhere would return Access auth/block behavior instead.

**Impact today:** If implementers exempt `/auth/login` to keep Android working, the public auth parser remains internet-reachable and violates the 0-day goal.

**Required controls:**

- Choose one supported mobile bootstrap model before enforcing Access-everywhere:
  - preferred: explicit local operator pairing with `oa register-device`, a short one-time code, and an Android-held private key/client certificate,
  - Access browser session plus verified app handoff,
  - local-only bootstrap while on trusted LAN,
  - or a separate minimal signed update/control API with no controller authority.
- In the preferred pairing flow, `oa register-device` runs on the trusted local host, creates a named pending device registration, prints a six-character one-time code, and expires it quickly.
- `oa` is the operator CLI surface of the same Rust `office-automate-server` binary, exposed as an alias/symlink/wrapper or first-class binary name, not a separate implementation.
- Android generates a non-exportable device private key in Android Keystore before redemption; the private key never leaves the phone.
- The six-character code is never redeemable over the public Cloudflare hostname. Redemption is local-only through a short-lived pairing listener or local controller IPC path started by `oa register-device`.
- The Android app must be on the trusted local network, enter the code, and send only the public key or CSR plus proof of key possession during the pairing window.
- Pairing records the device public key, issues a long-lived but revocable device identity, and may issue a per-device client certificate for Cloudflare mTLS.
- Future remote API calls use the Android-held private key/client certificate so Cloudflare can deny requests that do not present a valid enrolled device credential before origin.
- Short-lived app API tokens may expire normally, but they refresh remotely using the long-lived enrolled device key/certificate; re-registration is not required unless the device credential is revoked, lost, rotated, or suspected compromised.
- The pairing code is one-use, rate-limited, and invalid after too many attempts; it is not a reusable password.
- `oa list-devices` shows enrolled devices and `oa revoke-device <id>` immediately disables the origin device credential and, when Cloudflare edge credentials are issued, revokes the corresponding Cloudflare credential too.
- Public `/auth/login` must not be exempted from Cloudflare Access.
- Static Cloudflare Access service tokens must never be bundled in APKs and should not be the preferred Android model.
- Android public URLs must use HTTPS only; cleartext is allowed only for explicit local development or local-only bootstrap.
- Custom-scheme callbacks must be replaced or constrained with verified App Links where practical.

### Scenario 6: Device/Cloud Ingress Parser Compromise

**Path:** attacker or compromised third-party service sends a payload that exploits an unknown parser/client vulnerability in YoLink, Kumo, Tuya/ERV, Qingping MQTT, or a presence helper.

**Impact today:** Device and cloud clients run in the privileged controller path, so parser RCE could access controller secrets, SQLite files, local repos, telemetry DBs, and LAN devices.

**Required controls:**

- Split device/cloud ingesters by source where practical.
- Ingesters run least privilege with no repo, telemetry, broad user-home, artifact-write, or unrelated credential access.
- Ingesters have bounded egress only to the required cloud endpoint, device endpoint, MQTT listener, or local controller IPC.
- Ingesters pass typed validated readings/events into the controller; they do not get arbitrary controller API or filesystem access.
- Presence runs as a narrow helper with only the macOS session access it needs, then sends typed presence snapshots to the controller.

### Scenario 7: MQTT Sensor Spoofing

**Path:** LAN device publishes fake `qingping/{mac}/up` messages.

**Impact:** False CO2/tVOC readings could trigger ventilation, suppress HVAC behavior, or poison history.

**Required controls:**

- Bind MQTT to only the interface/IP needed by Qingping.
- Firewall MQTT to the Qingping device IP or isolated IoT VLAN.
- Reject wrong topic, wrong payload MAC, malformed payloads, stale timestamps, unreasonable sensor ranges, unreasonable jumps, and over-rate publishes.
- Enforce raw payload storage limits.
- Alert on unexpected client count/IPs/client IDs/topics.

### Scenario 8: OAuth/JWT Token Theft Via XSS Or Mobile Capture

**Path:** frontend XSS, malicious extension, LAN observer, or another Android app captures bearer tokens.

**Impact:** Attacker can operate app as user until token expires.

**Required controls:**

- Move browser session tokens to `HttpOnly; Secure; SameSite=Lax/Strict` cookies.
- Add CSRF tokens for state-changing POSTs.
- Add CSP that blocks inline scripts and unexpected origins.
- Store Android tokens in platform secure storage instead of normal preferences where feasible.
- Use verified App Links or stronger redirect handling for mobile callback capture.
- Keep token expiry short and support server-side revocation.
- Do not return Google refresh tokens from device flow unless a real client need exists.

### Scenario 9: Artifact Update Abuse

**Path:** attacker exploits upload route, auth bug, local compromise, or metadata weakness to upload or advertise malicious APK.

**Impact:** Android client may install attacker-controlled app update.

**Required controls:**

- Keep `/deploy/{app}` admin-only and disabled on public edge.
- Fail startup if deploy is enabled in Open mode.
- Verify APK signature/certificate fingerprint before accepting upload.
- Store full SHA-256 digest metadata, not only a short prefix.
- Android recomputes and compares full SHA-256 before launching installer.
- Android pins expected signing certificate before install.
- Maintain rollback and revocation metadata so a bad artifact can be blocked after publication.

### Scenario 10: Local Host Compromise Through Public Edge

**Path:** public request compromises the tunnel process or Rust edge process and uses local filesystem/LAN.

**Impact:** Local network foothold, credential theft, repo exfiltration.

**Required controls:**

- Dedicated public-edge process/user.
- Dedicated tunnel process/user, or the same quarantined account when that creates a tighter boundary.
- Tunnel and edge users cannot read each other's credentials unless required by the local origin path.
- Edge user cannot read config, data dir, repos, telemetry DBs, SQLite DBs, or launchd controller plists.
- Tunnel/edge users cannot connect to RFC1918 LAN subnets or device IPs.
- Tunnel can reach only the Rust edge origin; edge can reach only narrow controller IPC.
- Controller process owns device credentials and has local-only IPC.

## Hardening Plan

### P0: Public Path Must Fail Closed

1. **Cloudflare Access everywhere**
   - Add a self-hosted Access application for the exact Office Automate hostname.
   - Allow only the intended Google account/group.
   - Require MFA where available.
   - Set short session duration for admin/control routes.
   - Enable account/zone "Require Access protection" so new hostnames are blocked by default.
   - No Bypass policy, public include rule, service-auth hole, or route exemption for `/auth/login`, `/apps/*`, `/apk`, `/ws`, `/status`, `/deploy/*`, or static assets.
   - Enable Access token validation at `cloudflared` or origin so origin rejects requests that bypass Access.

2. **Public probe validation**
   - Add negative probes from a public network with no Access session. These must be blocked by Cloudflare before origin:
     - `GET /`
     - `GET /status`
     - `GET /auth/login?platform=android`
     - `GET /auth/callback`
     - `POST /auth/device/start`
     - `POST /auth/device/poll`
     - `GET /assets/<known-asset>`
     - `GET /<static>.json`
     - `GET /<static>.png`
     - `GET /apps/office-climate/meta.json`
     - `GET /apk`
     - real WebSocket upgrade to `/ws`
     - `POST /deploy/office-climate`
   - The validation source of truth must enumerate every current origin auth-skip route and fail when new auth-skip routes are added without a matching negative public probe.
   - Add authenticated Access probes that prove the same routes then reach origin and still require/perform Office OAuth or route-level authorization as designed.
   - Treat any origin Office OAuth JSON response to unauthenticated public `/auth/login` as a release blocker.

3. **Tunnel ingress deny-by-default**
   - Tunnel ingress must contain only the exact Office hostname to `http://127.0.0.1:<port>` or a Unix socket adapter.
   - Final ingress rule must return 404.
   - Do not publish MQTT, SSH, private IPs, wildcard hostnames, WARP private routes, or CIDR routes.
   - Validate with Cloudflare API/Terraform export or dashboard screenshots showing Access app, policies, hostname, no Bypass, no private routes, and final deny.
   - Local `cloudflared tunnel ingress validate` is necessary but not sufficient.

4. **HTTP bind hardening**
   - Change production config to bind HTTP to `127.0.0.1`, not `0.0.0.0`.
   - Consider changing Rust default from `0.0.0.0` to `127.0.0.1` so exposure is opt-in.
   - Add startup warning or hard error when auth is disabled and host is non-loopback.

5. **Disable unsafe public auth modes**
   - Basic auth is a compatibility fallback only.
   - Production public deployment must require Cloudflare Access plus Office OAuth/JWT.
   - Add validation failure for public URL when Basic auth is the only app auth.
   - Add validation failure for public URL when `trusted_networks` can bypass Office auth without validated Access context.

### P0: Contain Tunnel And Origin 0-Day Blast Radius

1. **Quarantine `cloudflared`**
   - Treat `cloudflared` as public edge code.
   - Run it under a dedicated quarantined tunnel account or the same low-privilege edge account if that yields a smaller local authority set.
   - Tunnel account has only tunnel config, tunnel credentials, logs, and the local origin target it needs.
   - Tunnel account cannot read controller config, controller data dir, SQLite DBs, telemetry DBs, configured repos, user home data, or controller launchd plists.
   - Tunnel credentials are separated from controller secrets and are not readable by controller-unrelated processes.
   - PF/application firewall denies tunnel account outbound LAN/RFC1918 egress except loopback or Unix-socket origin access.
   - Validation must attempt file reads and LAN connects as the tunnel user and fail.

2. **Split edge from controller**
   - Introduce a public HTTP edge mode/process that owns only public HTTP parsing, OAuth session handling, static assets, and a narrow controller client.
   - The controller process owns MQTT, presence, ERV/HVAC/YoLink/Kumo credentials, SQLite writes, telemetry collectors, and LAN egress.
   - Edge process must not read device secrets, telemetry repos, session-manager DBs, climate SQLite files, artifact write directories, or controller config.

3. **Hard OS boundary**
   - Run edge under a dedicated low-privilege macOS user.
   - Edge user's home/config directories contain only edge config and static assets.
   - File permissions must prove the edge user cannot read:
     - `config.yaml`,
     - data dir and SQLite DBs,
     - telemetry DBs,
     - configured repos,
     - Cloudflare tunnel credentials,
     - controller launchd plist if it contains secrets.
   - PF/application firewall must deny edge outbound LAN access except local controller IPC.
   - Validation must attempt file reads and LAN connects as the edge user and fail.

4. **Narrow controller IPC**
   - Prefer Unix domain socket with file permissions over a TCP listener.
   - Require a local capability token or peer credential check.
   - Expose only typed commands needed by the UI.
   - No arbitrary proxying, no path-forwarding tunnel, no raw LAN fetch endpoint.
   - Rate limit control commands and keep existing active-write safety gates.

### P1: Application-Layer Hardening

1. **Route authorization matrix**
   - Define route classes: Access-blocked public static, authenticated read, authenticated control, admin-only upload, local-only internal, mobile bootstrap.
   - Add tests proving each route enforces its class under OAuth, Basic, trusted-network, and open modes.
   - Add startup failure if `/deploy` is enabled in Open mode or reachable from the public edge.
   - Add an admin authorization source, such as explicit configured admin email allowlist, separate from normal allowed users.

2. **CSRF and token storage**
   - Move browser JWT from localStorage to HttpOnly Secure cookie.
   - Add CSRF token/header requirement for POST routes.
   - Store Android bearer tokens in platform secure storage where feasible.
   - Use verified App Links or safer redirect handling for mobile OAuth callbacks.
   - Keep Android bearer flow explicit and disallow cleartext for public URLs.
   - Prefer explicit local pairing: `oa register-device` creates a short-lived one-time code and a local-only pairing listener/path, Android redeems it once while on the trusted local network, and the server stores a revocable per-device public-key credential record.
   - Implement `oa` device commands in the same Rust binary as Office Automate; `oa` may be an alias, symlink, wrapper, or installed binary name for `office-automate-server`.
   - Android generates a non-exportable private key in Android Keystore; pairing sends only the public key or CSR and proof of possession.
   - For remote use, Cloudflare should require a per-device client certificate or equivalent proof-of-possession edge credential before forwarding Android API requests.
   - Short-lived Android API sessions refresh remotely using the enrolled device key/certificate; frequent re-registration is not required.
   - The public Cloudflare hostname must not expose a code redemption endpoint.
   - Add `oa list-devices` and `oa revoke-device <id>` for local trusted administration; revocation disables both the origin device credential and any associated Cloudflare client certificate or edge credential.
   - Do not bundle static Cloudflare Access service tokens in APKs.

3. **Request limits**
   - Add global request body limits; keep tighter multipart limits.
   - Add WebSocket max message size, idle timeout, and connection cap.
   - Add per-IP/per-session rate limits for auth, device flow, control routes, and deploy.

4. **Security headers**
   - Add CSP, HSTS on public responses, `X-Content-Type-Options: nosniff`, `Referrer-Policy`, and frame restrictions.
   - Narrow CORS from `*` to the configured public origin once all clients support it.

5. **Artifact verification**
   - Verify APK signature/cert fingerprint before accepting uploads.
   - Store and serve full SHA-256 digest metadata.
   - Android must recompute digest and verify expected signing cert before launching installer.
   - Add artifact revocation and rollback metadata.
   - Make `/apps/*` Access-protected or add a signed update API that carries no controller authority.

### P1: LAN And Device Hardening

1. **Device/cloud ingress containment**
   - Treat YoLink, Kumo, Tuya/ERV, Qingping, and presence inputs as untrusted parser surfaces.
   - Split ingesters/helpers by source where practical.
   - Run ingesters least privilege with no repo, telemetry, broad user-home, artifact-write, or unrelated credential access.
   - Bound each ingester's egress to the required cloud endpoint, device endpoint, MQTT listener, or local controller IPC.
   - Send only typed validated events/readings into the controller.
   - Presence should become a narrow helper with only the macOS session access it needs.

2. **MQTT confinement**
   - Bind only to the LAN IP required by Qingping or an IoT VLAN.
   - Add firewall rules to allow only the Qingping device IP.
   - Add broker-side client/IP/client ID logging and unexpected-client alerts.
   - Reject payloads when the embedded MAC does not equal the configured device MAC.
   - Reject out-of-range CO2, PM, temperature, humidity, tVOC, noise, battery, and timestamp values.
   - Reject stale timestamps and unreasonable deltas from the previous accepted reading.
   - Keep payload max low, rate limit by client/IP/topic, and bound raw payload storage.

3. **Device credential isolation**
   - Separate config files by process role: edge config has no device credentials.
   - Ensure config and credential files are `0600`.
   - Rotate ERV local key, Kumo password, YoLink secret, OAuth client secret, JWT secret, and tunnel credentials after any suspected origin compromise.

4. **Trusted-network policy**
   - Trusted-network bypass is local convenience only.
   - Do not use broad `192.168.0.0/16` on any listener reachable through public proxy.
   - Do not enable trusted-network bypass on the public listener unless Access context is validated first and the listener is not shared with LAN-direct traffic.
   - Add validation that public requests are never classified as trusted solely because the peer is `127.0.0.1` or because `X-Forwarded-For` is in a configured CIDR.

### P1: Cloudflare Drift Validation

1. **Configuration evidence**
   - Capture Cloudflare dashboard screenshots or API/Terraform exports for:
     - exact hostname,
     - Access app binding,
     - no Bypass policies,
     - no wildcard DNS,
     - no private network routes,
     - tunnel origin service target,
     - final 404/deny ingress rule.
   - Store sanitized validation output under deployment logs.

2. **Runtime probes**
   - Run negative unauthenticated public probes from outside the Access session.
   - Run authenticated Access probes.
   - Confirm Cloudflare Access audit logs show the expected allow/deny events.

### P2: Supply Chain And Build Integrity

1. **Rust dependency gates**
   - Add `cargo audit` and `cargo deny` in CI.
   - Track direct dependencies and justify network/parser crates.
   - Keep `Cargo.lock` committed and release from clean checkout.

2. **Cloudflared provenance**
   - Pin installed `cloudflared` version in deployment notes.
   - Keep launchd `--no-autoupdate`.
   - Update on an operator-controlled cadence after changelog review.

3. **Frontend/Android gates**
   - Run lockfile audit for frontend and Android dependencies.
   - Add a security checklist for changes touching auth, deploy, artifact, WebSocket, tunnel, Android storage/deep links, or MQTT code.

### P2: Detection, Recovery, And Operations

1. **Security validation command**
   - Add `office-automate-server validate security`.
   - It should check listener bind addresses, auth mode, trusted-network settings, Access probe results, tunnel config shape, Cloudflare export, file permissions, launchd service state, and edge/controller separation.

2. **Runtime alerts**
   - Alert on auth failure spikes, unexpected MQTT clients, artifact uploads, service restarts, tunnel config changes, DB integrity failures, and Cloudflare Access policy changes.

3. **Kill switch**
   - Document and test:
     ```bash
     launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.office-automate.tunnel.plist"
     ```
   - Confirm local controller continues climate safety without public access.

4. **Incident evidence capture**
   - Capture evidence before rotation when safe:
     ```bash
     date
     launchctl print "gui/$(id -u)/com.office-automate.server"
     launchctl print "gui/$(id -u)/com.office-automate.tunnel"
     lsof -nP -iTCP -sTCP:LISTEN
     ps eww -p "$(pgrep -f office-automate-server | head -1)"
     stat -f "%Sp %Su %Sg %N" config.yaml data/*.db logs/* 2>/dev/null
     shasum -a 256 target/release/office-automate-server
     find data/apps -type f -maxdepth 3 -print0 | xargs -0 shasum -a 256
     sqlite3 data/office_climate.db "PRAGMA quick_check;"
     ```
   - Preserve `cloudflared` logs, Office logs, Access audit logs, tunnel route/export state, launchd plists, file permissions, artifact hashes, MQTT client evidence, and recent DB integrity output.
   - Store raw evidence only in a local encrypted evidence directory.
   - Do not paste raw `ps eww`, logs, config, Access exports, or token-bearing output into tickets, chats, PRs, or issue comments.
   - Sanitize secrets and bearer material before sharing externally.
   - Prefer sharing hashes, timestamps, process IDs, file modes, and redacted excerpts over complete secret-bearing files.

5. **Dependency-ordered rotation**
   - Stop public tunnel first.
   - Rotate Cloudflare tunnel credentials and Access tokens/policies.
   - Rotate Office JWT secret and OAuth client secret.
   - Revoke bad Android artifacts and rotate signing/update metadata if needed.
   - Rotate ERV local key.
   - Rotate Kumo credentials.
   - Rotate YoLink credentials.
   - Rebuild from clean checkout and restore DB only after integrity checks.

## Required Implementation Tickets

This document is filed as epic #100 with these sub-tickets:

1. **#101 - P0: Cloudflare Access and tunnel fail-closed validation**
   - Access required before origin, exact hostname only, no Bypass, final 404 ingress rule, no private routing, Access token validation, negative public probes for every auth-skip route, authenticated Access-origin probes, real WebSocket upgrade probe.

2. **#102 - P0: Bind HTTP origin to loopback and fail unsafe startup configs**
   - Production HTTP default loopback, validation rejects public non-authenticated/non-loopback configs, public Basic-only configs, and public trusted-network bypass configs.

3. **#103 - P0: Quarantine `cloudflared` and public edge processes**
   - Tunnel/edge accounts cannot read controller config/data/repos/telemetry, cannot read each other's credentials unless required, and cannot reach LAN/RFC1918 except loopback or Unix-socket origin/IPC paths.

4. **#104 - P0: Split public edge from LAN/device controller**
   - Edge has no device credentials, no repo/data access, no telemetry DB access, no climate SQLite access, and no broad LAN egress.

5. **#105 - P0: Mobile bootstrap under Access-everywhere**
   - Replace Android direct public `/auth/login` dependency with preferred `oa register-device` local-only pairing implemented in the same Rust binary, enrolling an Android-held private key/client certificate for future Cloudflare-gated remote access, or another design that preserves the same public 0-day containment.

6. **#106 - P1: Route authorization matrix, admin authorization, and trusted-network hardening**
   - Tests for each route class, `/deploy` admin-only, Open-mode deploy disabled, public proxy bypass resistance.

7. **#107 - P1: Browser and Android session hardening**
   - HttpOnly Secure cookies, CSRF tokens, CSP/security headers, narrow CORS, Android secure storage, verified redirect handling, no cleartext public URLs, non-exportable Android device keys, no bundled static Access service token.

8. **#108 - P1: Device/cloud ingress containment**
   - YoLink/Kumo/Tuya/Qingping/presence helpers least privilege, bounded egress, no repo/telemetry/user-home access, typed IPC into controller.

9. **#109 - P1: MQTT confinement and spoofing resistance**
   - Listener/firewall guidance, client logging, payload MAC equality, range/freshness/delta checks, rate limits, storage limits, alerts.

10. **#110 - P1: Artifact upload/download hardening**
   - Admin-only deploy, APK signature verification, full digest metadata, Android digest/signing cert verification, rollback/revocation.

11. **#111 - P1: Cloudflare drift validation**
   - API/Terraform/dashboard evidence, Access audit log checks, negative public probes, authenticated probes.

12. **#112 - P2: Secrets/file-permission/security validation CLI**
    - `validate security`, credential permission checks, launchd/tunnel checks, tunnel-user and edge-user read/connect denial checks.

13. **#113 - P2: Supply-chain gates**
    - `cargo audit`, `cargo deny`, frontend/Android lockfile audit, release provenance.

14. **#114 - P2: Incident response and kill-switch runbook**
    - Evidence capture commands, secret-safe evidence handling, dependency-ordered secret rotation, recovery validation.

## Acceptance Criteria

Security hardening is complete only when all of these are true:

- Unauthenticated public requests to every current auth-skip route are blocked by Cloudflare Access before origin, including `/`, `/status`, `/auth/login`, `/auth/callback`, `/auth/device/start`, `/auth/device/poll`, `/assets/*`, static `.json`/`.png`, `/apps/*`, `/apk`, and `/deploy/*`.
- A real unauthenticated WebSocket upgrade to `/ws` is blocked by Cloudflare Access before origin.
- Any future origin auth-skip route requires a matching negative public Access probe.
- Authenticated Access requests reach origin and still pass Office OAuth/JWT, route authorization, and admin checks.
- Cloudflare config evidence proves exact hostname, Access app, no Bypass policy, no wildcard DNS, no private network route, loopback origin target, and final deny/404 ingress.
- If Cloudflare Access is misconfigured, the origin still requires Office Automate auth for protected routes.
- Public listener does not use trusted-network bypass unless Access context is validated first and the listener is not shared with LAN-direct traffic.
- `cloudflared` loopback requests with spoofed or trusted `X-Forwarded-For` cannot bypass Office auth.
- If the tunnel process is compromised, it cannot read controller secrets, controller config, data dirs, local repos, telemetry DBs, climate SQLite files, or user home content.
- If the public HTTP process is compromised, it cannot read device secrets, local repos, telemetry DBs, climate SQLite files, or controller config.
- If the tunnel or public HTTP process is compromised, OS/network policy prevents arbitrary LAN/RFC1918 egress except approved loopback/Unix-socket origin or IPC paths.
- Validation as the tunnel and edge users proves file-read and LAN-connect denial.
- Edge/controller IPC is narrow, authenticated, rate-limited, and not an arbitrary proxy.
- Android bootstrap works without exempting `/auth/login` from Cloudflare Access.
- Preferred Android registration is explicit local-only pairing: `oa register-device` issues a short-lived one-time code, Android redeems it once over a trusted-local pairing path, generates a non-exportable device private key, registers only the public key/CSR, stores the resulting device credential in secure storage, and local `oa list-devices` / `oa revoke-device <id>` can inspect or revoke enrolled devices.
- The public Cloudflare hostname has no unauthenticated registration-code redemption endpoint.
- Android remote access through Cloudflare is denied unless the app presents the enrolled per-device client certificate or equivalent private-key proof-of-possession edge credential.
- Short-lived Android API sessions can refresh remotely using the enrolled device key/certificate; re-registration is only needed after revocation, device wipe, credential loss, credential rotation, or suspected compromise.
- If Android uses a Cloudflare edge credential for future remote access, that credential is per-device, provisioned only during local pairing, and revoked with `oa revoke-device <id>`.
- Android public URLs use HTTPS; bearer tokens are stored securely; mobile redirect handling resists custom-scheme capture where feasible.
- No static Cloudflare Access service token is bundled in APKs.
- Device/cloud ingesters and presence helpers run least privilege, have bounded egress, cannot read repos/telemetry/user-home data, and send only typed validated IPC into the controller.
- MQTT is reachable only by the expected device/network segment and rejects wrong topic, wrong payload MAC, bad ranges, stale/flooded data, and unexpected clients.
- `/deploy` cannot be used by a normal authenticated user, unauthenticated public client, Open-mode deployment, or public edge.
- Artifact clients verify full digest and expected signing identity before install.
- Browser tokens are not readable by JavaScript.
- Route authorization behavior is covered by tests.
- `office-automate-server validate security` fails deployment on unsafe bind/auth/tunnel/Access/file-permission/edge-boundary settings.
- The public tunnel can be disabled immediately without stopping local climate safety.
- Incident response evidence capture, secret-safe evidence handling, and secret rotation order are documented and rehearsed.

## External References

- Cloudflare Tunnel creates outbound-only connections and avoids public inbound origin ports: https://developers.cloudflare.com/tunnel/
- Cloudflare "Require Access protection" blocks hostnames without Access applications by default: https://developers.cloudflare.com/cloudflare-one/access-controls/access-settings/require-access-protection/
- Cloudflare Access self-hosted applications provide per-application policy enforcement: https://developers.cloudflare.com/cloudflare-one/access-controls/applications/choose-application-type/
- Cloudflare self-hosted public applications recommend Access token validation at the origin or via `cloudflared`: https://developers.cloudflare.com/cloudflare-one/applications/configure-apps/self-hosted-apps/

## Specification Classification

This is an **epic**, not a single ticket. One agent can draft this threat model, but implementation requires multiple security-sensitive changes across Cloudflare configuration, launchd deployment, Rust HTTP/auth, process boundaries, MQTT, Android, artifacts, CI, and operations. Each sub-ticket above should be implemented and reviewed separately.
