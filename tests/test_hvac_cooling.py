"""Unit tests for automated cooling coordination and interlocks."""

import asyncio
from types import ModuleType, SimpleNamespace
import sys


def _install_dependency_stubs():
    """Provide lightweight module stubs so orchestrator can be imported in unit tests."""
    if "aiomqtt" not in sys.modules:
        aiomqtt = ModuleType("aiomqtt")
        aiomqtt.Message = object
        aiomqtt.Client = object
        sys.modules["aiomqtt"] = aiomqtt

    if "tinytuya" not in sys.modules:
        tinytuya = ModuleType("tinytuya")
        tinytuya.Device = object
        tinytuya.Cloud = object
        sys.modules["tinytuya"] = tinytuya

    if "jwt" not in sys.modules:
        jwt = ModuleType("jwt")
        jwt.decode = lambda *args, **kwargs: {}
        jwt.encode = lambda *args, **kwargs: "token"
        jwt.ExpiredSignatureError = Exception
        jwt.InvalidTokenError = Exception
        sys.modules["jwt"] = jwt

    if "google" not in sys.modules:
        sys.modules["google"] = ModuleType("google")
    if "google.oauth2" not in sys.modules:
        sys.modules["google.oauth2"] = ModuleType("google.oauth2")
    if "google.oauth2.id_token" not in sys.modules:
        id_token = ModuleType("google.oauth2.id_token")
        id_token.verify_oauth2_token = lambda *args, **kwargs: {}
        sys.modules["google.oauth2.id_token"] = id_token
    if "google.auth" not in sys.modules:
        sys.modules["google.auth"] = ModuleType("google.auth")
    if "google.auth.transport" not in sys.modules:
        sys.modules["google.auth.transport"] = ModuleType("google.auth.transport")
    if "google.auth.transport.requests" not in sys.modules:
        requests_mod = ModuleType("google.auth.transport.requests")
        requests_mod.Request = object
        sys.modules["google.auth.transport.requests"] = requests_mod
    if "google_auth_oauthlib" not in sys.modules:
        sys.modules["google_auth_oauthlib"] = ModuleType("google_auth_oauthlib")
    if "google_auth_oauthlib.flow" not in sys.modules:
        flow_mod = ModuleType("google_auth_oauthlib.flow")

        class _Flow:
            @staticmethod
            def from_client_config(*args, **kwargs):
                return _Flow()

        flow_mod.Flow = _Flow
        sys.modules["google_auth_oauthlib.flow"] = flow_mod


_install_dependency_stubs()

from src.config import ThresholdsConfig
from src.orchestrator import Orchestrator
from src.state_machine import OccupancyState


class FakeKumo:
    def __init__(self, mode: str = "off"):
        self.mode = mode
        self.calls: list[tuple[str, float | None]] = []

    async def get_full_status(self):
        return {
            "power": 0 if self.mode == "off" else 1,
            "operationMode": self.mode,
            "spHeat": 22.0,
            "spCool": 25.5555555556,
        }

    async def turn_off(self):
        self.calls.append(("turn_off", None))
        self.mode = "off"

    async def set_heat(self, temp):
        self.calls.append(("set_heat", temp))
        self.mode = "heat"

    async def set_cool(self, temp):
        self.calls.append(("set_cool", temp))
        self.mode = "cool"

    async def set_auto(self, heat_setpoint, cool_setpoint):
        self.calls.append(("set_auto", heat_setpoint))
        self.mode = "auto"


class FakeDB:
    def __init__(self):
        self.actions: list[tuple[str, str, str | None]] = []

    def log_climate_action(self, device, action, co2_ppm=None, reason=None, setpoint=None):
        self.actions.append((device, action, reason))


def _build_orchestrator(temp_f: float, *, state=OccupancyState.PRESENT, safety_interlock=False, hvac_mode="off"):
    thresholds = ThresholdsConfig(
        hvac_cool_on_temp_f=81,
        hvac_cool_off_temp_f=78,
        hvac_cool_setpoint_f=78,
    )
    orchestrator = Orchestrator.__new__(Orchestrator)
    orchestrator.config = SimpleNamespace(thresholds=thresholds)
    orchestrator.kumo = FakeKumo(mode=hvac_mode)
    orchestrator.qingping = SimpleNamespace(
        latest_reading=SimpleNamespace(temp_c=(temp_f - 32) * 5 / 9)
    )
    orchestrator.db = FakeDB()
    orchestrator.state_machine = SimpleNamespace(
        state=state,
        safety_interlock_active=safety_interlock,
    )
    orchestrator._erv_running = False
    orchestrator._hvac_mode = hvac_mode
    orchestrator._hvac_setpoint_c = 22.0
    orchestrator._hvac_heat_setpoint_c = 22.0
    orchestrator._hvac_cool_setpoint_c = (78 - 32) * 5 / 9
    orchestrator._hvac_suspended = False
    orchestrator._hvac_last_mode = "heat"
    orchestrator._hvac_temp_band_paused = False
    orchestrator._hvac_temp_band_mode = None
    orchestrator._manual_hvac_override = False
    orchestrator._manual_hvac_mode = None
    orchestrator._manual_hvac_setpoint_f = None
    orchestrator._manual_hvac_override_at = None
    orchestrator._manual_override_timeout = 1800
    orchestrator._main_loop = None
    orchestrator._check_manual_override_expiry = lambda: None
    orchestrator._is_within_occupancy_hours = lambda: True
    orchestrator._get_temp_f = Orchestrator._get_temp_f.__get__(orchestrator, Orchestrator)
    orchestrator._get_target_setpoint_c = Orchestrator._get_target_setpoint_c.__get__(orchestrator, Orchestrator)
    orchestrator._apply_hvac_mode = Orchestrator._apply_hvac_mode.__get__(orchestrator, Orchestrator)
    return orchestrator


def test_auto_cooling_starts_above_81f_when_present():
    orchestrator = _build_orchestrator(82.0)

    asyncio.run(Orchestrator._evaluate_hvac_state(orchestrator))

    assert orchestrator._hvac_mode == "cool"
    assert orchestrator.kumo.calls == [("set_cool", orchestrator._hvac_cool_setpoint_c)]


def test_auto_cooling_does_not_start_with_safety_interlock():
    orchestrator = _build_orchestrator(84.0, safety_interlock=True)

    asyncio.run(Orchestrator._evaluate_hvac_state(orchestrator))

    assert orchestrator._hvac_mode == "off"
    assert orchestrator.kumo.calls == []


def test_safety_interlock_turns_off_running_cooling():
    orchestrator = _build_orchestrator(
        84.0,
        safety_interlock=True,
        hvac_mode="cool",
    )
    orchestrator._hvac_last_mode = "cool"

    asyncio.run(Orchestrator._evaluate_hvac_state(orchestrator))

    assert orchestrator._hvac_mode == "off"
    assert orchestrator._hvac_suspended is True
    assert orchestrator._hvac_last_mode == "cool"
    assert orchestrator.kumo.calls == [("turn_off", None)]
