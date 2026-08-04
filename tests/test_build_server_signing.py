"""Tests for the designated-requirement guard in scripts/build-server.sh.

These exercise `--check-dr`, which validates a `codesign -d -r-` requirement
string read from stdin. That seam needs no keychain, no certificate, and no
built binary, so the guard that keeps the Local Network grant stable is
testable on any machine.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "build-server.sh"

IDENTIFIER = "com.office-automate.server"
CERT_ROOT = "36fc54a873d584a34fcfeea7d1f519b19a39de72"

STABLE_DR = f'designated => identifier "{IDENTIFIER}" and certificate root = H"{CERT_ROOT}"'


def check_dr(requirement: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(SCRIPT), "--check-dr"],
        input=requirement,
        capture_output=True,
        text=True,
    )


def test_accepts_stable_requirement():
    result = check_dr(STABLE_DR)
    assert result.returncode == 0, result.stderr


def test_rejects_cdhash_requirement():
    """An ad-hoc binary is keyed on content, so its grant dies on every rebuild."""
    result = check_dr('designated => cdhash H"4032bf3d8b5ba710b7485e855b2a0a4c746d85ad"')
    assert result.returncode != 0
    assert "cdhash" in result.stderr


def test_rejects_commented_cdhash_requirement():
    """codesign emits an ad-hoc binary's requirement as a `# designated => ...` comment.

    The extraction must match that form too, or the cdhash check never fires on
    the very case it exists to catch.
    """
    result = check_dr('# designated => cdhash H"9a915dfa23d06a637a27ef789c14387ab92e3db1"')
    assert result.returncode != 0
    assert "cdhash" in result.stderr


def test_rejects_cargo_default_identifier():
    """Cargo's identifier embeds a metadata hash that churns on version bumps.

    Signing without `-i` passes a naive two-rebuild test and then breaks
    readback on the next dependency bump, which is the regression #167 exists
    to prevent.
    """
    result = check_dr(
        'designated => identifier "office_automate_server-652815103961bb17" '
        f'and certificate root = H"{CERT_ROOT}"'
    )
    assert result.returncode != 0
    assert IDENTIFIER in result.stderr


def test_rejects_unexpected_certificate_root():
    """A regenerated certificate is a different TCC subject; the grant will not match."""
    result = check_dr(
        f'designated => identifier "{IDENTIFIER}" '
        'and certificate root = H"0000000000000000000000000000000000000000"'
    )
    assert result.returncode != 0
    assert CERT_ROOT in result.stderr


def test_rejects_empty_requirement():
    result = check_dr("   \n")
    assert result.returncode != 0
    assert "empty" in result.stderr
