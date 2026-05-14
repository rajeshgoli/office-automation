"""Unit tests for manual presence correction."""

import asyncio
import time

from src.state_machine import OccupancyState, StateConfig, StateMachine


def test_manual_present_behaves_like_motion_after_door_event():
    state_machine = StateMachine(StateConfig())
    state_machine.sensors.door_last_changed = time.time()

    asyncio.run(state_machine.set_manual_presence(True))

    assert state_machine.state == OccupancyState.PRESENT
    assert state_machine.sensors.motion_detected is True
    assert state_machine.sensors.motion_last_seen > state_machine.sensors.door_last_changed


def test_manual_away_resets_stale_activity_boundary():
    state_machine = StateMachine(StateConfig())
    now = time.time()
    state_machine.state = OccupancyState.PRESENT
    state_machine.sensors.external_monitor = True
    state_machine.sensors.mac_last_active = now
    state_machine.sensors.door_last_changed = now - 60
    state_machine.sensors.motion_detected = True
    state_machine.sensors.motion_last_seen = now

    asyncio.run(state_machine.set_manual_presence(False))

    assert state_machine.state == OccupancyState.AWAY
    assert state_machine.sensors.motion_detected is False
    assert state_machine.sensors.motion_last_seen == 0
    assert state_machine.sensors.door_last_changed > now
    assert state_machine.is_present is False
