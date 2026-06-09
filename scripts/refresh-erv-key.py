#!/usr/bin/env python3
"""Refresh the configured ERV Tuya local key from Smart Life sharing APIs."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterable
from pathlib import Path
from typing import Any, TextIO
from urllib.parse import parse_qs, urlparse

try:
    from tuya_sharing import LoginControl, Manager, SharingTokenListener
except ImportError:  # pragma: no cover - exercised by users without deps installed
    LoginControl = None
    Manager = None
    SharingTokenListener = object


CLIENT_ID = "HA_3y9q4ak7g4ephrvke"
SCHEMA = "haauthorize"
DEFAULT_AUTH_FILE = Path.home() / ".office-automate" / "tuya-sharing-auth.json"
DEFAULT_CONFIG_FILE = Path("config.yaml")
REQUIREMENTS_FILE = "scripts/refresh-erv-key-requirements.txt"
SERVICE_LABEL = "com.office-automate.server"


class RefreshError(Exception):
    """User-facing failure for the refresh command."""


class AuthCacheTokenListener(SharingTokenListener):
    """Persist refreshed Smart Life access tokens back to the local cache."""

    def __init__(self, auth_file: Path, auth_cache: dict[str, Any]):
        self.auth_file = auth_file
        self.auth_cache = auth_cache

    def update_token(self, token_info: dict[str, Any]):
        self.auth_cache["token_info"] = token_info
        self.auth_cache["updated_at"] = int(time.time())
        save_auth_cache(self.auth_file, self.auth_cache)


def load_roundtrip_yaml():
    try:
        from ruamel.yaml import YAML
    except ImportError as exc:  # pragma: no cover - depends on local env
        raise RefreshError(
            f"ruamel.yaml is required. Run: python3 -m pip install -r {REQUIREMENTS_FILE}"
        ) from exc

    yaml = YAML()
    yaml.preserve_quotes = True
    yaml.indent(mapping=2, sequence=4, offset=2)
    return yaml


def load_config_data(config_file: Path) -> tuple[Any, Any]:
    if not config_file.exists():
        raise RefreshError(f"config file not found: {config_file}")

    yaml = load_roundtrip_yaml()
    try:
        with config_file.open() as fh:
            data = yaml.load(fh)
    except Exception as exc:
        raise RefreshError(f"failed to parse {config_file}: {exc}") from exc

    if not isinstance(data, dict):
        raise RefreshError(f"{config_file} must contain a YAML mapping")
    return yaml, data


def read_erv_config(config_file: Path) -> tuple[str, str | None]:
    _, data = load_config_data(config_file)
    erv = data.get("erv")
    if not isinstance(erv, dict):
        raise RefreshError(f"{config_file} is missing an erv mapping")

    device_id = erv.get("device_id")
    if not isinstance(device_id, str) or not device_id.strip():
        raise RefreshError(f"{config_file} is missing erv.device_id")

    local_key = erv.get("local_key")
    if local_key is not None and not isinstance(local_key, str):
        raise RefreshError(f"{config_file} has a non-string erv.local_key")

    return device_id.strip(), local_key


def update_config_local_key(config_file: Path, new_local_key: str) -> None:
    yaml, data = load_config_data(config_file)
    erv = data.get("erv")
    if not isinstance(erv, dict):
        raise RefreshError(f"{config_file} is missing an erv mapping")

    old_value = erv.get("local_key")
    erv["local_key"] = preserve_scalar_style(old_value, new_local_key)

    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{config_file.name}.",
        suffix=".tmp",
        dir=str(config_file.parent or Path(".")),
        text=True,
    )
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "w") as fh:
            yaml.dump(data, fh)
        try:
            mode = config_file.stat().st_mode & 0o777
            tmp_path.chmod(mode)
        except OSError:
            pass
        os.replace(tmp_path, config_file)
    finally:
        if tmp_path.exists():
            tmp_path.unlink()


def preserve_scalar_style(old_value: Any, new_value: str) -> str:
    try:
        from ruamel.yaml.scalarstring import ScalarString
    except ImportError:  # pragma: no cover
        return new_value

    if isinstance(old_value, ScalarString):
        return old_value.__class__(new_value)
    return new_value


def load_auth_cache(auth_file: Path) -> dict[str, Any]:
    if not auth_file.exists():
        raise RefreshError(
            f"auth cache not found: {auth_file}. Run with --init-auth --user-code <code> first."
        )

    try:
        with auth_file.open() as fh:
            auth_cache = json.load(fh)
    except Exception as exc:
        raise RefreshError(f"failed to read auth cache {auth_file}: {exc}") from exc

    require_keys(auth_cache, ["user_code", "terminal_id", "endpoint", "token_info"])
    require_keys(
        auth_cache["token_info"],
        ["t", "expire_time", "uid", "access_token", "refresh_token"],
    )
    return auth_cache


def save_auth_cache(auth_file: Path, auth_cache: dict[str, Any]) -> None:
    auth_file.parent.mkdir(parents=True, exist_ok=True)
    try:
        auth_file.parent.chmod(0o700)
    except OSError:
        pass

    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{auth_file.name}.",
        suffix=".tmp",
        dir=str(auth_file.parent),
        text=True,
    )
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "w") as fh:
            json.dump(auth_cache, fh, indent=2, sort_keys=True)
            fh.write("\n")
        tmp_path.chmod(0o600)
        os.replace(tmp_path, auth_file)
    finally:
        if tmp_path.exists():
            tmp_path.unlink()


def require_keys(data: Any, keys: list[str]) -> None:
    if not isinstance(data, dict):
        raise RefreshError("auth cache has an invalid shape")
    missing = [key for key in keys if not data.get(key)]
    if missing:
        raise RefreshError(f"auth cache is missing: {', '.join(missing)}")


def init_auth(
    user_code: str,
    auth_file: Path,
    timeout_seconds: int,
    poll_interval: float,
    stdout: TextIO,
    stderr: TextIO,
) -> dict[str, Any]:
    ensure_tuya_sdk()
    login = LoginControl()

    response = login.qr_code(CLIENT_ID, SCHEMA, user_code)
    if not response.get("success"):
        raise RefreshError(format_tuya_error("failed to create Smart Life QR token", response))

    result = response.get("result", {})
    token = extract_login_token(result)
    qr_payload = extract_qr_payload(result, token)

    print("Scan this QR with Smart Life or Tuya Smart to authorize this daemon.", file=stderr)
    print("Smart Life path: Me -> Settings -> Account and Security -> User Code.", file=stderr)
    print("", file=stderr)
    print_qr(qr_payload, stderr)
    print("", file=stderr)
    print(f"QR payload: {qr_payload}", file=stderr)
    print(f"Waiting up to {timeout_seconds}s for authorization...", file=stderr)

    deadline = time.monotonic() + timeout_seconds
    last_error = None
    while time.monotonic() < deadline:
        ok, login_response = login.login_result(token, CLIENT_ID, user_code)
        if ok:
            auth_cache = build_auth_cache(user_code, login_response)
            save_auth_cache(auth_file, auth_cache)
            print(f"saved Smart Life auth cache to {auth_file}", file=stderr)
            return auth_cache

        last_error = login_response
        code = str(login_response.get("code", ""))
        msg = str(login_response.get("msg", "")).lower()
        if (
            code
            and code not in {"1001", "1106", "1110", "E0020003"}
            and "pending" not in msg
            and "scan and try again" not in msg
        ):
            raise RefreshError(format_tuya_error("Smart Life QR authorization failed", login_response))
        print(".", end="", file=stderr, flush=True)
        time.sleep(poll_interval)

    if last_error:
        raise RefreshError(format_tuya_error("Smart Life QR authorization timed out", last_error))
    raise RefreshError("Smart Life QR authorization timed out")


def build_auth_cache(user_code: str, login_response: dict[str, Any]) -> dict[str, Any]:
    token_info = {
        "t": require_token_value(login_response, "t"),
        "expire_time": require_token_value(login_response, "expire_time", "expireTime"),
        "uid": require_token_value(login_response, "uid"),
        "access_token": require_token_value(login_response, "access_token", "accessToken"),
        "refresh_token": require_token_value(login_response, "refresh_token", "refreshToken"),
    }
    return {
        "client_id": CLIENT_ID,
        "schema": SCHEMA,
        "user_code": user_code,
        "terminal_id": require_token_value(login_response, "terminal_id", "terminalId"),
        "endpoint": normalize_endpoint(require_token_value(login_response, "endpoint", "endPoint")),
        "token_info": token_info,
        "created_at": int(time.time()),
    }


def require_token_value(data: dict[str, Any], *keys: str) -> Any:
    for key in keys:
        value = data.get(key)
        if value not in (None, ""):
            return value
    raise RefreshError(f"Smart Life auth response did not include {'/'.join(keys)}")


def normalize_endpoint(endpoint: str) -> str:
    endpoint = endpoint.rstrip("/")
    if endpoint.startswith("http://") or endpoint.startswith("https://"):
        return endpoint
    return f"https://{endpoint}"


def extract_login_token(result: Any) -> str:
    for value in walk_values(result):
        if not isinstance(value, str):
            continue
        token = token_from_string(value)
        if token:
            return token
    raise RefreshError(f"Smart Life QR response did not include a login token: {result!r}")


def extract_qr_payload(result: Any, token: str) -> str:
    if isinstance(result, dict):
        for key in ("qrcode", "qrCode", "qr_code", "url", "qrCodeUrl", "qrcodeUrl"):
            value = result.get(key)
            if isinstance(value, str) and value.strip().startswith("tuyaSmart--qrLogin?"):
                return value.strip()
    return f"tuyaSmart--qrLogin?token={token}"


def walk_values(value: Any) -> Iterable[Any]:
    if isinstance(value, dict):
        priority_keys = (
            "token",
            "qrCodeToken",
            "qrcodeToken",
            "loginToken",
            "qrcode",
            "qrCode",
            "url",
        )
        for key in priority_keys:
            if key in value:
                yield value[key]
        for key, item in value.items():
            if key not in priority_keys:
                yield from walk_values(item)
    elif isinstance(value, list):
        for item in value:
            yield from walk_values(item)
    else:
        yield value


def token_from_string(value: str) -> str | None:
    value = value.strip()
    if not value:
        return None

    parsed = urlparse(value)
    if parsed.query:
        query = parse_qs(parsed.query)
        for key in ("token", "code", "qrcode", "qrCode"):
            values = query.get(key)
            if values and values[0]:
                return values[0]

    match = re.search(r"/tokens/([^/?#]+)", value)
    if match:
        return match.group(1)

    if re.fullmatch(r"[A-Za-z0-9._:-]{8,}", value) and not parsed.scheme:
        return value
    return None


def print_qr(payload: str, stderr: TextIO) -> None:
    try:
        import qrcode
    except ImportError:
        print(
            f"qrcode is not installed; install {REQUIREMENTS_FILE} to render the QR.",
            file=stderr,
        )
        return

    qr = qrcode.QRCode(border=1)
    qr.add_data(payload)
    qr.make(fit=True)
    qr.print_ascii(out=stderr)


def fetch_current_local_key(
    auth_file: Path,
    auth_cache: dict[str, Any],
    device_id: str,
) -> tuple[str, str | None]:
    ensure_tuya_sdk()
    listener = AuthCacheTokenListener(auth_file, auth_cache)
    manager = Manager(
        CLIENT_ID,
        auth_cache["user_code"],
        auth_cache["terminal_id"],
        auth_cache["endpoint"],
        auth_cache["token_info"],
        listener,
    )

    device = fetch_device_detail(manager, device_id)
    if device is None:
        device = fetch_device_from_cache(manager, device_id)
    if device is None:
        raise RefreshError(f"device {device_id} was not returned by Smart Life")

    local_key = get_device_field(device, "local_key", "localKey")
    if not isinstance(local_key, str) or not local_key:
        raise RefreshError(
            f"Smart Life returned device {device_id}, but no localKey/local_key field"
        )
    name = get_device_field(device, "name")
    return local_key, name if isinstance(name, str) else None


def fetch_device_detail(manager: Any, device_id: str) -> Any | None:
    customer_api = getattr(manager, "customer_api", None)
    if customer_api is None or not hasattr(customer_api, "get"):
        return None

    try:
        response = customer_api.get("/v1.0/m/life/ha/devices/detail", {"devIds": device_id})
    except Exception:
        return None

    result = response.get("result") if isinstance(response, dict) else None
    for device in normalize_device_results(result):
        if get_device_field(device, "id", "devId", "device_id") == device_id:
            return device
    return None


def fetch_device_from_cache(manager: Any, device_id: str) -> Any | None:
    try:
        manager.update_device_cache()
    except Exception as exc:
        raise RefreshError(f"failed to list Smart Life devices: {exc}") from exc

    device_map = getattr(manager, "device_map", {})
    if isinstance(device_map, dict):
        if device_id in device_map:
            return device_map[device_id]
        for device in device_map.values():
            if get_device_field(device, "id", "devId", "device_id") == device_id:
                return device
    return None


def normalize_device_results(result: Any) -> list[Any]:
    if isinstance(result, list):
        return result
    if isinstance(result, dict):
        for key in ("devices", "list", "data"):
            value = result.get(key)
            if isinstance(value, list):
                return value
        return [result]
    return []


def get_device_field(device: Any, *names: str) -> Any:
    if isinstance(device, dict):
        for name in names:
            if name in device:
                return device[name]
        return None

    for name in names:
        if hasattr(device, name):
            return getattr(device, name)
    return None


def restart_orchestrator() -> None:
    uid = os.getuid()
    subprocess.run(
        ["launchctl", "kickstart", "-k", f"gui/{uid}/{SERVICE_LABEL}"],
        check=True,
    )


def ensure_tuya_sdk() -> None:
    if LoginControl is None or Manager is None:
        raise RefreshError(
            f"tuya-device-sharing-sdk is required. Run: python3 -m pip install -r {REQUIREMENTS_FILE}"
        )


def format_tuya_error(prefix: str, response: dict[str, Any]) -> str:
    code = response.get("code")
    msg = response.get("msg") or response.get("message")
    if code or msg:
        return f"{prefix}: code={code} msg={msg}"
    return f"{prefix}: {response}"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Fetch the current ERV Tuya local key via the Smart Life HA sharing API.",
    )
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG_FILE)
    parser.add_argument("--auth-file", type=Path, default=DEFAULT_AUTH_FILE)
    parser.add_argument(
        "--init-auth",
        action="store_true",
        help="authorize the daemon with a Smart Life QR scan before fetching the key",
    )
    parser.add_argument(
        "--user-code",
        help="Smart Life user code for QR authorization",
    )
    parser.add_argument("--auth-timeout", type=int, default=180)
    parser.add_argument("--poll-interval", type=float, default=3.0)
    parser.add_argument(
        "--print",
        dest="print_key",
        action="store_true",
        help="print the current local key (default unless --update-config is used)",
    )
    parser.add_argument(
        "--update-config",
        action="store_true",
        help="replace erv.local_key in config.yaml if it changed",
    )
    parser.add_argument(
        "--restart",
        action="store_true",
        help=f"kickstart the launchctl {SERVICE_LABEL} service after --update-config changes the key",
    )
    return parser


def main(
    argv: list[str] | None = None,
    stdout: TextIO = sys.stdout,
    stderr: TextIO = sys.stderr,
) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.print_key and args.update_config:
        parser.error("--print and --update-config are mutually exclusive")
    if args.restart and not args.update_config:
        parser.error("--restart requires --update-config")
    if args.init_auth and not args.user_code:
        parser.error("--init-auth requires --user-code")

    try:
        device_id, configured_key = read_erv_config(args.config)

        if args.init_auth:
            auth_cache = init_auth(
                args.user_code,
                args.auth_file,
                args.auth_timeout,
                args.poll_interval,
                stdout,
                stderr,
            )
        else:
            auth_cache = load_auth_cache(args.auth_file)

        current_key, device_name = fetch_current_local_key(
            args.auth_file,
            auth_cache,
            device_id,
        )

        if args.update_config:
            if current_key == configured_key:
                print("no rotation needed", file=stdout)
                return 0

            update_config_local_key(args.config, current_key)
            suffix = f" for {device_name}" if device_name else ""
            print(f"updated {args.config}: erv.local_key refreshed{suffix}", file=stdout)
            if args.restart:
                restart_orchestrator()
                print(f"restarted {SERVICE_LABEL}", file=stdout)
            return 0

        print(current_key, file=stdout)
        return 0

    except RefreshError as exc:
        print(f"error: {exc}", file=stderr)
        return 1
    except subprocess.CalledProcessError as exc:
        print(f"error: launchctl restart failed: {exc}", file=stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
