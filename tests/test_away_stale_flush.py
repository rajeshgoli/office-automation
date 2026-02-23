"""Unit tests for periodic stale-air flushing while AWAY."""

from collections import deque
from datetime import datetime, timedelta
from types import ModuleType, SimpleNamespace
import sys


def _install_dependency_stubs():
    """Provide lightweight module stubs so orchestrator can be imported in unit tests."""
    if "aiomqtt" not in sys.modules:
        aiomqtt = ModuleType("aiomqtt")

        class Message:
            pass

        aiomqtt.Message = Message
        aiomqtt.Client = object
        sys.modules["aiomqtt"] = aiomqtt

    if "tinytuya" not in sys.modules:
        tinytuya = ModuleType("tinytuya")
        tinytuya.Device = object
        tinytuya.Cloud = object
        sys.modules["tinytuya"] = tinytuya

    if "jwt" not in sys.modules:
        jwt = ModuleType("jwt")

        class ExpiredSignatureError(Exception):
            pass

        class InvalidTokenError(Exception):
            pass

        jwt.decode = lambda *args, **kwargs: {}
        jwt.encode = lambda *args, **kwargs: "token"
        jwt.ExpiredSignatureError = ExpiredSignatureError
        jwt.InvalidTokenError = InvalidTokenError
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

            def authorization_url(self, **kwargs):
                return ("http://example.com", None)

            def fetch_token(self, **kwargs):
                return None

            @property
            def credentials(self):
                return SimpleNamespace(token="token", refresh_token=None, id_token="id")

        flow_mod.Flow = _Flow
        sys.modules["google_auth_oauthlib.flow"] = flow_mod


_install_dependency_stubs()

from src.config import ThresholdsConfig
from src.orchestrator import Orchestrator
from src.state_machine import OccupancyState, StateConfig, StateMachine


class FakeERV:
    def __init__(self):
        self.calls: list[tuple[str, str | None]] = []

    def turn_on(self, fan_speed):
        self.calls.append(("on", fan_speed.value))

    def turn_off(self):
        self.calls.append(("off", None))


class FakeDB:
    def __init__(self):
        self.actions: list[tuple[str, str, str | None]] = []

    def log_climate_action(self, device, action, co2_ppm=None, reason=None, setpoint=None):
        self.actions.append((device, action, reason))


def _build_orchestrator(thresholds: ThresholdsConfig) -> Orchestrator:
    orchestrator = Orchestrator.__new__(Orchestrator)
    orchestrator.config = SimpleNamespace(thresholds=thresholds)
    orchestrator.state_machine = StateMachine(
        StateConfig(
            motion_timeout_seconds=thresholds.motion_timeout_seconds,
            co2_critical_ppm=thresholds.co2_critical_ppm,
            co2_refresh_target_ppm=thresholds.co2_refresh_target_ppm,
        )
    )
    orchestrator.qingping = SimpleNamespace(latest_reading=None)
    orchestrator.erv = FakeERV()
    orchestrator.db = FakeDB()

    orchestrator._erv_running = False
    orchestrator._erv_speed = "off"

    orchestrator._co2_history = deque(maxlen=thresholds.co2_history_size)
    orchestrator._outdoor_co2_baseline = None
    orchestrator._plateau_detected = False
    orchestrator._away_start_time = None

    orchestrator._tvoc_away_history = deque(maxlen=thresholds.tvoc_away_history_size)
    orchestrator._tvoc_away_ventilation_active = False
    orchestrator._tvoc_baseline = None
    orchestrator._tvoc_plateau_detected = False

    orchestrator._manual_erv_override = False
    orchestrator._manual_erv_speed = None
    orchestrator._manual_erv_override_at = None
    orchestrator._manual_hvac_override = False
    orchestrator._manual_hvac_mode = None
    orchestrator._manual_hvac_setpoint_f = None
    orchestrator._manual_hvac_override_at = None
    orchestrator._manual_override_timeout = 30 * 60

    orchestrator._room_closed_since = datetime.now()
    orchestrator._away_stale_flush_active_until = None
    orchestrator._away_stale_flush_next_due_at = None

    return orchestrator


def _reading(co2_ppm: int, tvoc: int):
    return SimpleNamespace(co2_ppm=co2_ppm, tvoc=tvoc)


def test_stale_flush_repeats_every_8_hours_at_medium():
    thresholds = ThresholdsConfig(
        away_stale_flush_enabled=True,
        away_stale_flush_interval_hours=8,
        away_stale_flush_duration_minutes=30,
        away_stale_flush_speed="medium",
        tvoc_away_enabled=False,
    )
    orchestrator = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.AWAY
    orchestrator.qingping.latest_reading = _reading(co2_ppm=450, tvoc=20)

    # First flush due now.
    now = datetime.now()
    orchestrator._room_closed_since = now - timedelta(hours=9)
    orchestrator._away_stale_flush_next_due_at = now - timedelta(minutes=1)
    orchestrator._evaluate_erv_state()

    assert orchestrator._erv_running is True
    assert orchestrator._erv_speed == "medium"

    # Flush window ended; next schedule not due yet -> ERV turns off.
    orchestrator._away_stale_flush_active_until = datetime.now() - timedelta(seconds=1)
    orchestrator._away_stale_flush_next_due_at = datetime.now() + timedelta(hours=1)
    orchestrator._evaluate_erv_state()
    assert orchestrator._erv_running is False
    assert orchestrator._erv_speed == "off"

    # Next stale cycle due -> flush starts again.
    orchestrator._away_stale_flush_next_due_at = datetime.now() - timedelta(seconds=1)
    orchestrator._evaluate_erv_state()
    assert orchestrator._erv_running is True
    assert orchestrator._erv_speed == "medium"


def test_stale_flush_does_not_run_when_present():
    thresholds = ThresholdsConfig(
        away_stale_flush_enabled=True,
        away_stale_flush_interval_hours=8,
        away_stale_flush_duration_minutes=30,
        away_stale_flush_speed="medium",
        tvoc_away_enabled=False,
    )
    orchestrator = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.PRESENT
    orchestrator.qingping.latest_reading = _reading(co2_ppm=450, tvoc=20)
    orchestrator._room_closed_since = datetime.now() - timedelta(hours=9)
    orchestrator._away_stale_flush_next_due_at = datetime.now() - timedelta(minutes=1)

    orchestrator._evaluate_erv_state()

    assert orchestrator._erv_running is False
    assert orchestrator._erv_speed == "off"


def test_stale_flush_schedule_resets_when_room_opens():
    thresholds = ThresholdsConfig(away_stale_flush_enabled=True)
    orchestrator = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.AWAY
    orchestrator._room_closed_since = datetime.now() - timedelta(hours=9)
    orchestrator._away_stale_flush_active_until = datetime.now() + timedelta(minutes=10)
    orchestrator._away_stale_flush_next_due_at = datetime.now() + timedelta(hours=8)

    orchestrator.state_machine.sensors.door_open = True
    orchestrator._update_room_closed_tracking()

    assert orchestrator._room_closed_since is None
    assert orchestrator._away_stale_flush_active_until is None
    assert orchestrator._away_stale_flush_next_due_at is None


def test_stale_flush_never_overrides_more_aggressive_co2_speed():
    thresholds = ThresholdsConfig(
        away_stale_flush_enabled=True,
        away_stale_flush_interval_hours=8,
        away_stale_flush_duration_minutes=30,
        away_stale_flush_speed="medium",
        tvoc_away_enabled=False,
        co2_adaptive_speed_enabled=True,
        co2_turbo_duration_minutes=30,
    )
    orchestrator = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.AWAY
    orchestrator.qingping.latest_reading = _reading(co2_ppm=1200, tvoc=20)
    orchestrator._away_start_time = datetime.now()  # Forces CO2 turbo window.
    orchestrator._room_closed_since = datetime.now() - timedelta(hours=9)
    orchestrator._away_stale_flush_next_due_at = datetime.now() - timedelta(minutes=1)

    orchestrator._evaluate_erv_state()

    assert orchestrator._erv_running is True
    assert orchestrator._erv_speed == "turbo"
