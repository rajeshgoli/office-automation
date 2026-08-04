# Code Signing the Server Binary

`office-automate-server` reads the ERV over the LAN. On macOS that is gated by
the Local Network privacy permission, and the permission is attached to the
binary's code-signing identity — not to its path. This document explains why the
binary must be signed with a stable identity, how to set that up once, and how
to verify it afterwards.

Background: #166 (root cause) and #167 (this fix).

## Why

`cargo build --release` produces an ad-hoc, linker-signed binary with no Team ID.
For such a binary macOS keys the Local Network grant on the **cdhash**, which is
derived from the binary's contents. Every rebuild therefore mints a new subject
with no grant.

The consequence is silent: a denied Local Network connection surfaces as an
immediate `EHOSTUNREACH` — "No route to host (os error 65)" — 0.4 ms after the
connect call, with the device pingable and its port open the whole time. It does
not look like a permissions problem. It cost two tickets and roughly 24 hours of
dead ERV readback before it was identified.

Signing with a stable certificate replaces the content-derived requirement with
one based on identifier plus certificate root:

```
designated => identifier "com.office-automate.server" and certificate root = H"36fc54a8..."
```

Neither term changes when the code changes, so one Local Network grant survives
every subsequent rebuild.

## One-time setup

All three steps are required. Each has been observed to be load-bearing.

### 1. Create a self-signed code-signing certificate

An Apple Developer ID is **not** required; a self-signed identity is honoured
(verified on macOS 26.5.1).

Keychain Access → **Certificate Assistant** → **Create a Certificate…**

| Field | Value |
| --- | --- |
| Name | `Office Automate Local Signing` |
| Identity Type | Self Signed Root |
| Certificate Type | **Code Signing** |
| Validity | Tick "Let me override defaults" and set 7300 days, so it does not lapse in a year |

### 2. Trust it for code signing

Double-click the certificate in the **login** keychain → expand **Trust** →
set **Code Signing** to **Always Trust** → close the window and authenticate.

Without this, `codesign` fails with `CSSMERR_TP_NOT_TRUSTED` and then
`no identity found`.

Verify:

```bash
security find-identity -v -p codesigning
```

The identity must appear here. Note that plain `security find-identity`, which
uses the X.509 Basic policy, will still report `CSSMERR_TP_NOT_TRUSTED` for this
certificate. That is expected and harmless — only the codesigning policy matters.

### 3. Allow `codesign` to use the private key

Keychain Access → **Keys** → the **private** key named `Office Automate Local
Signing` → **Get Info** → **Access Control**. Either select "Allow all
applications to access this item", or keep "Confirm before allowing access",
untick "Ask for Keychain password", and add `/usr/bin/codesign` to the list.

Skipping this makes every build prompt for the login keychain password. The
public key's access control is not consulted and does not need changing.

### 4. Grant Local Network once, after the first signed build

Adopting signing **changes the binary's identity**, so any existing grant does
not carry over. Build and restart once, then approve the Local Network prompt
for `office-automate-server` (or enable it under System Settings → Privacy &
Security → Local Network). After that it is stable across rebuilds.

Do not read anything into a single successful boot read taken immediately after
signing — the grant state settles a restart later, and a first-restart success
before any grant exists has been observed. Confirm with a second restart.

## Building

Use the build script. It builds, signs, and verifies in one step:

```bash
scripts/build-server.sh
```

Extra arguments are passed through to `cargo build --release`, so
`scripts/build-server.sh --locked` works.

A bare `cargo build --release` overwrites the signed binary in place with an
ad-hoc one and silently breaks ERV local readback on the next restart. If you
do run one, re-sign afterwards by running `scripts/build-server.sh` again.

The script fails closed: if the signing identity is missing it stops rather than
deploying a binary that will lose the grant. `OFFICE_AUTOMATE_ALLOW_UNSIGNED=1`
overrides this and warns loudly.

| Variable | Default | Purpose |
| --- | --- | --- |
| `OFFICE_AUTOMATE_SIGNING_IDENTITY` | `Office Automate Local Signing` | Keychain identity to sign with |
| `OFFICE_AUTOMATE_SIGNING_IDENTIFIER` | `com.office-automate.server` | Pinned signing identifier |
| `OFFICE_AUTOMATE_SIGNING_CERT_ROOT` | `36fc54a8…` | Expected certificate root hash |
| `OFFICE_AUTOMATE_ALLOW_UNSIGNED` | unset | Build unsigned and accept broken readback |

### Why the identifier is pinned

Cargo's default signing identifier embeds a metadata hash —
`office_automate_server-652815103961bb17` — that changes on version bumps,
dependency bumps, and toolchain updates. Signing without `codesign -i` would
pass a two-rebuild test today and reintroduce this bug on the next `cargo
update`. `scripts/build-server.sh` pins it and refuses to accept a requirement
that does not.

## Verifying

Run this after any build, and whenever ERV local readback looks wrong:

```bash
scripts/build-server.sh --verify-only
```

It reads the deployed binary's designated requirement and fails if a cdhash term
is present, if the identifier is not pinned, or if the certificate root is not
the expected one.

## Restarting

Restart after deploying a new binary:

```bash
launchctl kickstart -k gui/$(id -u)/com.office-automate.server
launchctl kickstart -p gui/$(id -u)/com.office-automate.server
```

`launchctl kickstart` **without** `-k` does not restart an already-running job —
it no-ops and leaves the PID unchanged. The two-command sequence above is the
one verified to work on this host.

Then confirm readback actually recovered:

```bash
grep "ERV boot read" logs/office-automate-server.out.log | tail -1
```

A success looks like `ERV boot read: running=false speed=off`. A failure looks
like `ERV boot read failed: ... No route to host (os error 65)`, which means the
Local Network grant is missing for the current identity.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `no identity found` from `codesign` | Certificate missing, or not trusted for the Code Signing policy. See steps 1–2. |
| Keychain password prompt on every build | Private key access control not set. See step 3. |
| `designated requirement contains a cdhash term` | The binary is ad-hoc signed — something ran a bare `cargo build --release`. Rebuild with `scripts/build-server.sh`. |
| `not anchored to the expected certificate root` | The certificate was regenerated. It is a new TCC subject: re-grant Local Network and update `OFFICE_AUTOMATE_SIGNING_CERT_ROOT`. |
| Boot read fails instantly with `os error 65` while `ping` and `nc` to port 6668 succeed | Local Network denial for the current identity, not a network fault. |

## Certificate rotation

The certificate is the anchor for the grant, so replacing it is a re-grant event.
If it expires or is regenerated: create and trust the new certificate, update
`OFFICE_AUTOMATE_SIGNING_CERT_ROOT` to the new root hash (visible in
`codesign -d -r-` output), rebuild, restart, and approve Local Network once more.
