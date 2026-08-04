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
# cargo always writes here regardless of any deploy-path override below.
cargo_bin="$root/target/release/office-automate-server"
binary="${OFFICE_AUTOMATE_SERVER_BIN:-$cargo_bin}"

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

  # `codesign -d -r-` only reads the embedded designated requirement; it does
  # not confirm the signature is cryptographically valid over the file's
  # current contents. Without --verify, a binary edited or corrupted after
  # signing could still show a clean DR here and defeat the fail-closed check.
  codesign --verify --strict "$binary" \
    || die "signature on $binary does not verify (codesign --verify --strict failed).
The binary may have been modified after signing. Re-run scripts/build-server.sh."

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

command -v jq >/dev/null 2>&1 || die "jq is required to locate cargo's build output reliably"

is_darwin=false
[[ "$(uname -s)" == "Darwin" ]] && is_darwin=true

# Determine signing availability before cargo runs. Otherwise a build that
# turns out to be unsignable has already overwritten the deployed binary
# (default path or --verify-only path) with an ad-hoc artifact by the time
# the identity check fails, leaving Local Network readback broken despite
# the script exiting nonzero — the exact silent-breakage this PR removes.
#
# Matching on name alone is not enough: a regenerated certificate (or a
# duplicate-named identity) can share the name while its root differs from
# the pinned $cert_root, in which case name-only matching says "available"
# but signing would produce a different TCC subject and fail verify() only
# after cargo build has already overwritten the deployed binary. For a
# self-signed identity, the SHA-1 `security find-identity` reports for it is
# the same hash `codesign -d -r-` reports as its certificate root, so check
# both before touching anything.
signing_available=false
if $is_darwin; then
  identity_line="$(security find-identity -v -p codesigning 2>/dev/null | grep -F "\"$identity\"" || true)"
  # grep exits 1 on no match, e.g. when identity_line is empty; that is an
  # expected outcome here (identity not found), not a script-ending error.
  identity_hash="$(printf '%s' "$identity_line" | grep -oE '[0-9A-Fa-f]{40}' | tr 'A-F' 'a-f' || true)"
  if [[ -n "$identity_line" && "$identity_hash" == "$cert_root" ]]; then
    signing_available=true
  fi
fi

if $is_darwin && ! $signing_available && [[ "${OFFICE_AUTOMATE_ALLOW_UNSIGNED:-}" != "1" ]]; then
  die "no keychain identity named \"$identity\" with certificate root $cert_root was found.
Either the identity is missing, or it was regenerated and no longer matches
OFFICE_AUTOMATE_SIGNING_CERT_ROOT. Create and trust it once per $doc, update
OFFICE_AUTOMATE_SIGNING_CERT_ROOT to the new root and re-grant Local Network,
or set OFFICE_AUTOMATE_ALLOW_UNSIGNED=1 to build unsigned and accept broken
ERV local readback."
fi

# Ask cargo where it actually put the binary rather than assuming $cargo_bin.
# --target-dir/--target, --config overrides of build.target-dir/build.target,
# CARGO_BUILD_TARGET_DIR/CARGO_BUILD_TARGET, and .cargo/config.toml can all
# relocate cargo's output — enumerating every such override is an arms race
# this script keeps losing. Reading cargo's own build-plan output cannot miss
# a future one.
json_log="$(mktemp)"
trap 'rm -f "$json_log"' EXIT

cargo build --release --manifest-path "$manifest" --message-format=json-render-diagnostics "$@" \
  | tee "$json_log" \
  | jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' >&2
build_status="${PIPESTATUS[0]}"
[[ "$build_status" -eq 0 ]] || exit "$build_status"

built_bin="$(jq -r --arg pkg "office-automate-server" '
  select(.reason == "compiler-artifact"
    and .target.name == $pkg
    and (.target.kind | index("bin"))
    and .executable != null) | .executable
' "$json_log" | tail -n1)"

[[ -n "$built_bin" && -x "$built_bin" ]] \
  || die "could not determine the built binary's path from cargo's output"

# Deploy the binary cargo actually produced. $binary defaults to $cargo_bin,
# which built_bin will equal for a plain build with no output override — but
# never assume that; always deploy from what cargo reported.
if [[ "$binary" != "$built_bin" ]]; then
  cp -p "$built_bin" "$binary"
fi

if ! $is_darwin; then
  printf 'build-server: not macOS, skipping code signing\n'
  exit 0
fi

if ! $signing_available; then
  warn "signing identity \"$identity\" not found and OFFICE_AUTOMATE_ALLOW_UNSIGNED=1 is set.
  The binary is ad-hoc signed. ERV local readback WILL fail after restart until
  Local Network is granted to this build, and will break again on the next build.
  See $doc"
  exit 0
fi

sign
verify
