"""Tests for the Smart Life ERV local key refresh script."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace


def load_script_module():
    script_path = Path(__file__).resolve().parents[1] / "scripts" / "refresh-erv-key.py"
    spec = importlib.util.spec_from_file_location("refresh_erv_key", script_path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def write_config(path: Path, local_key: str = "old-local-key") -> None:
    path.write_text(
        "\n".join(
            [
                "# Office Climate Automation Configuration",
                "erv:",
                '  type: "tuya"',
                '  device_id: "erv-device-id"',
                f'  local_key: "{local_key}"  # keep this comment',
                '  ip: "192.0.2.25"',
                "",
            ]
        )
    )


def write_auth(path: Path) -> None:
    path.write_text(
        json.dumps(
            {
                "user_code": "us-user-code",
                "terminal_id": "terminal-id",
                "endpoint": "https://example.tuyaus.com",
                "token_info": {
                    "t": 1770000000000,
                    "expire_time": 7200,
                    "uid": "uid",
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                },
            }
        )
    )


class FakeManager:
    latest_listener = None

    def __init__(self, client_id, user_code, terminal_id, endpoint, token_info, listener):
        self.customer_api = None
        self.device_map = {
            "erv-device-id": SimpleNamespace(
                id="erv-device-id",
                name="ERVQ-H-F-BM",
                local_key="new-local-key",
            )
        }
        FakeManager.latest_listener = listener

    def update_device_cache(self):
        FakeManager.latest_listener.update_token(
            {
                "t": 1770000001000,
                "expire_time": 7200,
                "uid": "uid",
                "access_token": "new-access-token",
                "refresh_token": "new-refresh-token",
            }
        )


def test_update_config_preserves_comments_and_auth_refresh(tmp_path, monkeypatch):
    module = load_script_module()
    monkeypatch.setattr(module, "Manager", FakeManager)
    monkeypatch.setattr(module, "LoginControl", object)

    config = tmp_path / "config.yaml"
    auth = tmp_path / "auth.json"
    write_config(config)
    write_auth(auth)

    stdout = io.StringIO()
    stderr = io.StringIO()
    rc = module.main(
        [
            "--config",
            str(config),
            "--auth-file",
            str(auth),
            "--update-config",
        ],
        stdout=stdout,
        stderr=stderr,
    )

    assert rc == 0
    assert "updated" in stdout.getvalue()
    assert "new-local-key" in config.read_text()
    assert "# keep this comment" in config.read_text()

    auth_data = json.loads(auth.read_text())
    assert auth_data["token_info"]["access_token"] == "new-access-token"
    assert auth_data["token_info"]["refresh_token"] == "new-refresh-token"


def test_same_key_reports_no_rotation_and_does_not_restart(tmp_path, monkeypatch):
    module = load_script_module()
    monkeypatch.setattr(module, "Manager", FakeManager)
    monkeypatch.setattr(module, "LoginControl", object)
    monkeypatch.setattr(
        module,
        "restart_orchestrator",
        lambda: (_ for _ in ()).throw(AssertionError("restart should not be called")),
    )

    config = tmp_path / "config.yaml"
    auth = tmp_path / "auth.json"
    write_config(config, local_key="new-local-key")
    write_auth(auth)
    before = config.read_text()

    stdout = io.StringIO()
    stderr = io.StringIO()
    rc = module.main(
        [
            "--config",
            str(config),
            "--auth-file",
            str(auth),
            "--update-config",
            "--restart",
        ],
        stdout=stdout,
        stderr=stderr,
    )

    assert rc == 0
    assert stdout.getvalue().strip() == "no rotation needed"
    assert config.read_text() == before


def test_same_key_print_mode_still_outputs_key(tmp_path, monkeypatch):
    module = load_script_module()
    monkeypatch.setattr(module, "Manager", FakeManager)
    monkeypatch.setattr(module, "LoginControl", object)

    config = tmp_path / "config.yaml"
    auth = tmp_path / "auth.json"
    write_config(config, local_key="new-local-key")
    write_auth(auth)
    before = config.read_text()

    stdout = io.StringIO()
    stderr = io.StringIO()
    rc = module.main(
        [
            "--config",
            str(config),
            "--auth-file",
            str(auth),
        ],
        stdout=stdout,
        stderr=stderr,
    )

    assert rc == 0
    assert stdout.getvalue().strip() == "new-local-key"
    assert config.read_text() == before


def test_print_mode_outputs_key_without_updating_config(tmp_path, monkeypatch):
    module = load_script_module()
    monkeypatch.setattr(module, "Manager", FakeManager)
    monkeypatch.setattr(module, "LoginControl", object)

    config = tmp_path / "config.yaml"
    auth = tmp_path / "auth.json"
    write_config(config)
    write_auth(auth)
    before = config.read_text()

    stdout = io.StringIO()
    stderr = io.StringIO()
    rc = module.main(
        ["--config", str(config), "--auth-file", str(auth)],
        stdout=stdout,
        stderr=stderr,
    )

    assert rc == 0
    assert stdout.getvalue().strip() == "new-local-key"
    assert config.read_text() == before


def test_qr_payload_wraps_home_assistant_qrcode_token():
    module = load_script_module()

    result = {"qrcode": "qr-token-value"}

    assert module.extract_login_token(result) == "qr-token-value"
    assert (
        module.extract_qr_payload(result, "qr-token-value")
        == "tuyaSmart--qrLogin?token=qr-token-value"
    )


def test_restart_uses_current_launchd_server_label(monkeypatch):
    module = load_script_module()
    calls = []

    monkeypatch.setattr(module.os, "getuid", lambda: 501)

    def fake_run(command, check):
        calls.append((command, check))
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(module.subprocess, "run", fake_run)

    module.restart_orchestrator()

    assert calls == [
        (
            [
                "launchctl",
                "kickstart",
                "-k",
                "gui/501/com.office-automate.server",
            ],
            True,
        )
    ]
