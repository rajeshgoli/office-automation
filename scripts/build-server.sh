#!/usr/bin/env bash
# Build office-automate-server and sign it with a stable code-signing identity.
#
# Why this exists: `cargo build --release` produces an ad-hoc, linker-signed
# binary with no Team ID. macOS keys the Local Network (TCC) grant for such a
# binary on its cdhash, so every rebuild is a new subject with no grant and ERV
# local readback silently dies until Local Network is granted again. Signing
# with a stable identity yields a designated requirement based on identifier
# plus certificate root instead, which survives rebuilds.
#
# One-time certificate setup and the full background: docs/deployment/code-signing.md
#
# Usage:
#   scripts/build-server.sh [cargo args...]   build, sign, verify
#   scripts/build-server.sh --verify-only     verify the already-deployed binary
#   scripts/build-server.sh --check-dr        validate a requirement string on stdin

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/rust/office-automate-server/Cargo.toml"
binary="${OFFICE_AUTOMATE_SERVER_BIN:-$root/target/release/office-automate-server}"

# The identity lives in the login keychain and is not in the repo. The
# identifier and certificate root are pinned because both are load-bearing:
# a change to either produces a different TCC subject and drops the grant.
identity="${OFFICE_AUTOMATE_SIGNING_IDENTITY:-Office Automate Local Signing}"
identifier="${OFFICE_AUTOMATE_SIGNING_IDENTIFIER:-com.office-automate.server}"
cert_root="${OFFICE_AUTOMATE_SIGNING_CERT_ROOT:-36fc54a873d584a34fcfeea7d1f519b19a39de72}"

doc="docs/deployment/code-signing.md"

die() {
  printf 'build-server: error: %s\n' "$*" >&2
  exit 1
}

warn() {
  printf 'build-server: warning: %s\n' "$*" >&2
}

# Validate a `codesign -d -r-` designated requirement. Kept free of any
# keychain or binary access so it can be unit-tested with fixtures.
check_dr() {
  local dr
  dr="$(cat)"

  [[ -n "${dr//[[:space:]]/}" ]] || die "empty designated requirement"

  # A cdhash term means the identity is content-keyed and will not survive a
  # rebuild. This is the exact failure #167 exists to eliminate.
  if printf '%s' "$dr" | grep -qi 'cdhash'; then
    die "designated requirement contains a cdhash term, so it will not survive a rebuild:
  $dr
The binary is ad-hoc signed. See $doc"
  fi

  # Cargo's default identifier embeds a metadata hash (office_automate_server-<hash>)
  # that changes on version and dependency bumps, so the identifier must be pinned
  # explicitly with `codesign -i`.
  if ! printf '%s' "$dr" | grep -qF "identifier \"$identifier\""; then
    die "designated requirement does not pin identifier \"$identifier\":
  $dr
Sign with: codesign -i $identifier. See $doc"
  fi

  if ! printf '%s' "$dr" | grep -qiF "certificate root = H\"$cert_root\""; then
    die "designated requirement is not anchored to the expected certificate root $cert_root:
  $dr
The signing certificate differs from the one the Local Network grant was issued to.
Either sign with the original certificate, or set OFFICE_AUTOMATE_SIGNING_CERT_ROOT
and re-grant Local Network. See $doc"
  fi
}

requirement_of() {
  # An ad-hoc binary's requirement is emitted as a "# designated => ..." comment
  # rather than a bare line, so match both forms — otherwise the cdhash check
  # never fires on the exact case it exists to catch.
  codesign -d -r- "$1" 2>/dev/null | grep -E '^#? *designated' \
    || die "could not read a designated requirement from $1"
}

verify() {
  [[ -x "$binary" ]] || die "no binary at $binary"
  local dr
  dr="$(requirement_of "$binary")"
  printf '%s\n' "$dr" | check_dr
  printf 'build-server: signature verified\n  %s\n  %s\n' "$binary" "$dr"
}

sign() {
  codesign --force --sign "$identity" -i "$identifier" "$binary" \
    || die "codesign failed. If it reported 'no identity found', the certificate is
missing or not trusted for the Code Signing policy. See $doc"
}

case "${1:-}" in
  --check-dr)
    check_dr
    exit 0
    ;;
  --verify-only)
    verify
    exit 0
    ;;
esac

cargo build --release --manifest-path "$manifest" "$@"

if [[ "$(uname -s)" != "Darwin" ]]; then
  printf 'build-server: not macOS, skipping code signing\n'
  exit 0
fi

if ! security find-identity -v -p codesigning 2>/dev/null | grep -qF "\"$identity\""; then
  if [[ "${OFFICE_AUTOMATE_ALLOW_UNSIGNED:-}" == "1" ]]; then
    warn "signing identity \"$identity\" not found and OFFICE_AUTOMATE_ALLOW_UNSIGNED=1 is set.
  The binary is ad-hoc signed. ERV local readback WILL fail after restart until
  Local Network is granted to this build, and will break again on the next build.
  See $doc"
    exit 0
  fi
  die "signing identity \"$identity\" not found in the keychain.
Create and trust it once, per $doc, or set OFFICE_AUTOMATE_ALLOW_UNSIGNED=1 to
build an unsigned binary and accept broken ERV local readback."
fi

sign
verify
