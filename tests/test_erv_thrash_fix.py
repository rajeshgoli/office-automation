"""Tests for ERV plateau latching and dwell protection."""

from __future__ import annotations

from collections import deque
from datetime import datetime, timedelta
from types import ModuleType, SimpleNamespace
import sys


def _install_dependency_stubs():
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
from src.erv_client import FanSpeed
from src.orchestrator import Orchestrator
from src.state_machine import OccupancyState


class FakeClock:
    def __init__(self, now: datetime):
        self.now = now

    def advance(self, **kwargs):
        self.now += timedelta(**kwargs)

    def set(self, value: datetime):
        self.now = value


class FakeERV:
    def __init__(self):
        self.calls: list[tuple[str, str | None]] = []

    def turn_on(self, fan_speed):
        self.calls.append(("on", fan_speed.value))
        return True

    def turn_off(self):
        self.calls.append(("off", None))
        return True


class FakeDB:
    def __init__(self):
        self.actions: list[tuple[str, str, str | None]] = []
        self.occupancy: list[tuple[str, int | None]] = []

    def log_climate_action(self, device, action, co2_ppm=None, reason=None, setpoint=None):
        self.actions.append((device, action, reason))

    def log_occupancy_change(self, state, trigger=None, co2_ppm=None, details=None):
        self.occupancy.append((state, co2_ppm))


def _reading(co2_ppm: int, tvoc: int = 44):
    return SimpleNamespace(co2_ppm=co2_ppm, tvoc=tvoc)


def _build_orchestrator(
    thresholds: ThresholdsConfig | None = None,
    now: datetime | None = None,
) -> tuple[Orchestrator, FakeClock]:
    thresholds = thresholds or ThresholdsConfig()
    clock = FakeClock(now or datetime(2026, 5, 21, 7, 45, 0))

    orchestrator = Orchestrator.__new__(Orchestrator)
    orchestrator.config = SimpleNamespace(thresholds=thresholds)
    orchestrator.state_machine = SimpleNamespace(
        state=OccupancyState.AWAY,
        sensors=SimpleNamespace(window_open=False, door_open=False),
    )
    orchestrator.qingping = SimpleNamespace(latest_reading=_reading(512))
    orchestrator.erv = FakeERV()
    orchestrator.db = FakeDB()
    orchestrator._now = lambda: clock.now

    orchestrator._erv_running = False
    orchestrator._erv_speed = "off"
    orchestrator._erv_speed_changed_at = None

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
    orchestrator._main_loop = None

    orchestrator._room_closed_since = clock.now - timedelta(hours=1)
    orchestrator._room_closed_state_known = True
    orchestrator._away_stale_flush_active_until = None
    orchestrator._away_stale_flush_next_due_at = None

    return orchestrator, clock


def _append_co2_history(orchestrator: Orchestrator, start: datetime, values: list[int]):
    for index, value in enumerate(values):
        orchestrator._co2_history.append((start + timedelta(seconds=index * 30), value))


def test_co2_plateau_latches_until_baseline_delta_release():
    thresholds = ThresholdsConfig(co2_plateau_release_delta_ppm=100)
    orchestrator, clock = _build_orchestrator(thresholds)
    start = clock.now - timedelta(minutes=10)
    _append_co2_history(orchestrator, start, [512, 513, 511, 512] * 6)

    orchestrator._plateau_detected = True
    orchestrator._outdoor_co2_baseline = 512

    for value in [510, 514, 513, 511, 512]:
        clock.advance(seconds=30)
        orchestrator._co2_history.append((clock.now, value))
        assert orchestrator._detect_co2_plateau() is True
        assert orchestrator._plateau_detected is True

    clock.advance(seconds=30)
    orchestrator._co2_history.append((clock.now, 612))

    assert orchestrator._detect_co2_plateau() is False
    assert orchestrator._plateau_detected is False
    assert orchestrator._outdoor_co2_baseline is None


def test_half_latched_plateau_state_resets_and_recomputes():
    orchestrator, clock = _build_orchestrator()
    start = clock.now - timedelta(minutes=10)
    _append_co2_history(orchestrator, start, [512, 513, 511, 512] * 6)
    orchestrator._plateau_detected = True
    orchestrator._outdoor_co2_baseline = None

    assert orchestrator._detect_co2_plateau() is True
    assert orchestrator._outdoor_co2_baseline == 512


def test_apply_erv_speed_suppresses_within_dwell_without_db_row():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator._erv_speed_changed_at = clock.now

    ok = orchestrator._apply_erv_speed(
        "quiet",
        FanSpeed.QUIET,
        "away_adaptive_quiet_CO2=512ppm",
        512,
    )

    assert ok is False
    assert orchestrator.erv.calls == []
    assert orchestrator.db.actions == []

    clock.advance(seconds=181)
    ok = orchestrator._apply_erv_speed(
        "quiet",
        FanSpeed.QUIET,
        "away_adaptive_quiet_CO2=512ppm",
        512,
    )

    assert ok is True
    assert orchestrator.erv.calls == [("on", "quiet")]
    assert orchestrator.db.actions == [
        ("erv", "quiet", "away_adaptive_quiet_CO2=512ppm")
    ]


def test_apply_erv_speed_bypass_ignores_fresh_dwell_timer():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator._erv_running = True
    orchestrator._erv_speed = "quiet"
    orchestrator._erv_speed_changed_at = clock.now

    ok = orchestrator._apply_erv_speed(
        "off",
        None,
        "safety_interlock",
        512,
        bypass_dwell=True,
    )

    assert ok is True
    assert orchestrator.erv.calls == [("off", None)]
    assert orchestrator.db.actions == [("erv", "off", "safety_interlock")]


def test_safety_interlock_bypasses_dwell_in_evaluate():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator._erv_running = True
    orchestrator._erv_speed = "quiet"
    orchestrator._erv_speed_changed_at = clock.now
    orchestrator.state_machine.sensors.door_open = True

    orchestrator._evaluate_erv_state()

    assert orchestrator.erv.calls == [("off", None)]
    assert orchestrator.db.actions == [("erv", "off", "safety_interlock")]


def test_manual_erv_override_bypasses_dwell_in_evaluate():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator._erv_speed_changed_at = clock.now
    orchestrator._manual_erv_override = True
    orchestrator._manual_erv_speed = "turbo"
    orchestrator._manual_erv_override_at = clock.now

    orchestrator._evaluate_erv_state()

    assert orchestrator.erv.calls == [("on", "turbo")]
    assert orchestrator.db.actions == [("erv", "turbo", "manual_override")]


def test_present_critical_co2_bypasses_dwell_in_evaluate():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.PRESENT
    orchestrator.qingping.latest_reading = _reading(thresholds.co2_critical_ppm)
    orchestrator._erv_speed_changed_at = clock.now

    orchestrator._evaluate_erv_state()

    assert orchestrator.erv.calls == [("on", "quiet")]
    assert orchestrator.db.actions == [
        ("erv", "quiet", f"present_co2_critical_{thresholds.co2_critical_ppm}ppm")
    ]


def test_presence_transition_bypasses_dwell_for_present_cleanup():
    thresholds = ThresholdsConfig(erv_min_dwell_seconds=180)
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.PRESENT
    orchestrator.qingping.latest_reading = _reading(450)
    orchestrator._erv_running = True
    orchestrator._erv_speed = "quiet"
    orchestrator._erv_speed_changed_at = clock.now
    orchestrator._plateau_detected = True
    orchestrator._outdoor_co2_baseline = 512

    orchestrator._on_state_change(OccupancyState.AWAY, OccupancyState.PRESENT)

    assert orchestrator.erv.calls == [("off", None)]
    assert orchestrator.db.actions == [("erv", "off", "present_co2_hysteresis_450ppm")]
    assert orchestrator._plateau_detected is False
    assert orchestrator._outdoor_co2_baseline is None


def test_presence_transition_bypasses_dwell_for_away_turbo():
    thresholds = ThresholdsConfig(
        erv_min_dwell_seconds=180,
        min_away_seconds_before_erv=0,
    )
    orchestrator, clock = _build_orchestrator(thresholds)
    orchestrator.state_machine.state = OccupancyState.AWAY
    orchestrator.qingping.latest_reading = _reading(700)
    orchestrator._erv_speed_changed_at = clock.now

    orchestrator._on_state_change(OccupancyState.PRESENT, OccupancyState.AWAY)

    assert orchestrator.erv.calls == [("on", "turbo")]
    assert orchestrator.db.actions == [("erv", "turbo", "away_adaptive_turbo_CO2=700ppm")]


def test_may_21_sensor_replay_emits_at_most_one_transition_after_plateau():
    thresholds = ThresholdsConfig(
        co2_turbo_duration_minutes=30,
        min_away_seconds_before_erv=0,
        tvoc_away_enabled=False,
        away_stale_flush_enabled=False,
        erv_min_dwell_seconds=180,
    )
    start = datetime(2026, 5, 21, 7, 45, 20)
    orchestrator, clock = _build_orchestrator(thresholds, now=start)
    orchestrator.state_machine.state = OccupancyState.AWAY
    orchestrator._away_start_time = start - timedelta(minutes=31)
    orchestrator._erv_running = True
    orchestrator._erv_speed = "quiet"
    orchestrator._erv_speed_changed_at = start - timedelta(minutes=10)
    _append_co2_history(
        orchestrator,
        start - timedelta(minutes=10),
        [517, 516, 517, 518, 519, 518, 518, 517, 517, 518,
         519, 519, 516, 518, 516, 517, 515, 516, 515, 516],
    )

    samples = [
        (0, 517), (4, 516), (31, 517), (34, 518), (61, 519), (64, 518),
        (93, 518), (94, 517), (124, 517), (124, 518), (154, 519),
        (156, 519), (184, 516), (185, 518), (214, 516), (217, 517),
        (244, 515), (248, 516), (274, 515), (278, 516), (304, 513),
        (309, 514), (334, 514), (341, 513), (364, 513), (371, 513),
        (394, 513), (402, 512), (424, 512), (434, 512), (454, 512),
        (464, 511), (484, 513), (495, 513), (514, 514), (526, 514),
        (544, 513), (559, 513), (574, 513), (588, 512), (604, 514),
        (619, 514), (634, 514), (650, 513), (664, 513), (681, 513),
        (694, 512), (712, 511), (724, 512), (743, 512), (754, 510),
        (775, 511), (784, 510), (805, 511), (814, 511), (836, 512),
        (844, 512), (867, 512), (874, 512), (899, 512), (904, 512),
        (929, 512), (934, 511), (960, 511), (964, 510), (991, 510),
        (994, 511), (1022, 510), (1024, 510), (1053, 509), (1054, 506),
        (1084, 505), (1084, 506), (1114, 506), (1115, 505), (1144, 508),
        (1146, 506), (1174, 508), (1177, 508), (1204, 509), (1208, 508),
        (1234, 509), (1239, 508), (1264, 510), (1270, 510), (1294, 508),
        (1301, 509), (1324, 507), (1332, 508), (1354, 507),
    ]

    for offset_seconds, co2 in samples:
        clock.set(start + timedelta(seconds=offset_seconds))
        orchestrator.qingping.latest_reading = _reading(co2)
        orchestrator._co2_history.append((clock.now, co2))
        orchestrator._evaluate_erv_state()

    assert orchestrator.erv.calls == [("off", None)]
    assert orchestrator.db.actions == [("erv", "off", "co2_plateau")]
    assert orchestrator._plateau_detected is True
