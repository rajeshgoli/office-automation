"""Unit tests for ERV local Tuya control health tracking."""

from types import ModuleType
import sys


if "tinytuya" not in sys.modules:
    tinytuya = ModuleType("tinytuya")
    tinytuya.Device = object
    tinytuya.Cloud = object
    sys.modules["tinytuya"] = tinytuya


from src.erv_client import ERVClient


def _client() -> ERVClient:
    return ERVClient(device_id="device-id", ip="192.0.2.10", local_key="local-key")


def _local_key_error() -> dict:
    return {"Error": "Check device key or version", "Err": "914", "Payload": None}


def test_local_key_error_emits_once_when_threshold_crosses():
    client = _client()
    events: list[dict] = []
    client.on_health_event(events.append)

    for _ in range(client.LOCAL_KEY_ERROR_THRESHOLD - 1):
        client._record_local_failure("Local status failed", _local_key_error())

    assert client.local_key_invalid is False
    assert events == []

    client._record_local_failure("Local status failed", _local_key_error())

    assert client.local_key_invalid is True
    assert len(events) == 1
    assert events[0]["type"] == "erv_local_key_invalid"

    client._record_local_failure("Local status failed", _local_key_error())

    assert len(events) == 1


def test_local_success_after_invalid_state_emits_recovery():
    client = _client()
    events: list[dict] = []
    client.on_health_event(events.append)

    for _ in range(client.LOCAL_KEY_ERROR_THRESHOLD):
        client._record_local_failure("Local status failed", _local_key_error())

    client._record_local_success()

    assert client.local_key_invalid is False
    assert client.consecutive_local_key_errors == 0
    assert client.local_key_invalid_since is None
    assert [event["type"] for event in events] == [
        "erv_local_key_invalid",
        "erv_local_key_recovered",
    ]


def test_non_key_local_error_resets_consecutive_key_errors():
    client = _client()

    client._record_local_failure("Local status failed", _local_key_error())
    client._record_local_failure("Local status failed", {"Error": "network timeout"})

    assert client.consecutive_local_key_errors == 0
    assert client.local_key_invalid is False
