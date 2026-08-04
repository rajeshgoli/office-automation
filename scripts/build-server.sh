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
# Deploy path — what launchd runs. Independent of where cargo actually builds;
# see the CARGO_TARGET_DIR default below and why the two must stay decoupled.
cargo_bin="$root/target/release/office-automate-server"
binary="${OFFICE_AUTOMATE_SERVER_BIN:-$cargo_bin}"

# Build into a target directory that is never the deploy path, so a build
# whose signing later fails cannot have already clobbered $binary before we
# know that. Persistent (not a fresh mktemp -d per run) so cargo's dependency
# and incremental cache carries over between builds — forcing a clean
# target-dir every time would turn every build into a full rebuild. Only used
# as a default: an operator's own CARGO_TARGET_DIR, --target-dir, or --config
# build.target-dir is respected untouched.
: "${CARGO_TARGET_DIR:=$root/target-signing}"
export CARGO_TARGET_DIR

# The identity lives in the login keychain and is not in the repo. The
# identifier and certificate root are pinned because both are load-bearing:
# a change to either produces a different TCC subject and drops the grant.
identity="${OFFICE_AUTOMATE_SIGNING_IDENTITY:-Office Automate Local Signing}"
identifier="${OFFICE_AUTOMATE_SIGNING_IDENTIFIER:-com.office-automate.server}"
cert_root="${OFFICE_AUTOMATE_SIGNING_CERT_ROOT:-36fc54a873d584a34fcfeea7d1f519b19a39de72}"
# Normalized once here so every comparison against it (the preflight identity
# hash, which security find-identity reports uppercase, and check_dr's
# case-insensitive grep) is comparing like with like.
cert_root="$(printf '%s' "$cert_root" | tr 'A-F' 'a-f')"

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

# Both take an optional path, defaulting to the deploy path $binary. The main
# build flow passes an explicit temp path so it can sign/verify a candidate
# before ever touching $binary; --verify-only calls these with no argument to
# check the binary already deployed there.
verify() {
  local target="${1:-$binary}"
  [[ -x "$target" ]] || die "no binary at $target"

  # `codesign -d -r-` only reads the embedded designated requirement; it does
  # not confirm the signature is cryptographically valid over the file's
  # current contents. Without --verify, a binary edited or corrupted after
  # signing could still show a clean DR here and defeat the fail-closed check.
  codesign --verify --strict "$target" \
    || die "signature on $target does not verify (codesign --verify --strict failed).
The binary may have been modified after signing. Re-run scripts/build-server.sh."

  local dr
  dr="$(requirement_of "$target")"
  printf '%s\n' "$dr" | check_dr
  printf 'build-server: signature verified\n  %s\n  %s\n' "$target" "$dr"
}

sign() {
  local target="${1:-$binary}"
  codesign --force --sign "$identity" -i "$identifier" "$target" \
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

# Fast-fail before invoking cargo at all when we can already tell signing
# will not work. This is an optimization, not the safety net — cargo building
# into an isolated target-dir plus the sign-a-temp-copy-then-atomically-move
# sequence below is what actually guarantees $binary is never touched by a
# build that turns out to be unsignable, including failure modes this check
# cannot see in advance (a keychain-locked or ACL-denied private key, for
# example).
#
# Matching on identity name alone is not enough: a regenerated certificate
# (or a duplicate-named identity) can share the name while its root differs
# from the pinned $cert_root. For a self-signed identity, the SHA-1
# `security find-identity` reports for it is the same hash `codesign -d -r-`
# reports as its certificate root, so check both.
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
# a future one. With our own CARGO_TARGET_DIR default above, this ordinarily
# resolves under target-signing/, not under $binary's directory.
json_log="$(mktemp)"
tmp_binary=""
trap 'rm -f "$json_log" "${tmp_binary:-}"' EXIT

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

mkdir -p "$(dirname "$binary")"

# built_bin and binary can be the same file (no signing on this platform, or
# --target-dir/--config pointed cargo straight at the deploy path), in which
# case cargo already left the right bytes there and `cp` would fail with
# "are the same file".
deploy_unsigned() {
  [[ "$built_bin" -ef "$binary" ]] || cp -p "$built_bin" "$binary"
}

if ! $is_darwin; then
  deploy_unsigned
  printf 'build-server: not macOS, skipping code signing\n'
  exit 0
fi

if ! $signing_available; then
  deploy_unsigned
  warn "signing identity \"$identity\" not found and OFFICE_AUTOMATE_ALLOW_UNSIGNED=1 is set.
  The binary is ad-hoc signed. ERV local readback WILL fail after restart until
  Local Network is granted to this build, and will break again on the next build.
  See $doc"
  exit 0
fi

# Sign and verify a copy, never $binary directly. If codesign fails for any
# reason — including ones the preflight check above cannot predict, such as
# the keychain being locked or the private key's access control denying a
# non-interactive request — $binary is left exactly as it was: the previous,
# already-signed, working deployment.
tmp_binary="$(mktemp "$(dirname "$binary")/.office-automate-server.XXXXXX")"
cp -p "$built_bin" "$tmp_binary"
sign "$tmp_binary"
verify "$tmp_binary"
mv -f "$tmp_binary" "$binary"
tmp_binary=""

printf 'build-server: deployed\n  %s\n' "$binary"
