# Renovation Mode for Climate and Presence Automation

Issue: #147

## Summary

The office is under renovation. The ERV, HVAC-relevant equipment, Mac activity source, useful motion context, and Qingping air-quality monitor have moved to another room, while the door and window contact sensors remain in the physical office under construction. During this period, the automation should keep collecting productivity-relevant signals and raw sensor telemetry, but it must not use the renovated office's contact sensors or moved air-quality readings to infer presence or to drive climate behavior.

This is a temporary operating mode, but it should be represented explicitly in config and status so future incidents are explainable from logs and dashboards.

## Current Behavior

The Rust controller currently treats the office as a two-state room model:

- Mac activity with an external monitor can transition AWAY -> PRESENT.
- Motion can count as presence, but normally only when it is inside the current door boundary.
- Door and window contact sensors update the state machine and safety interlocks.
- Qingping readings update status, CO2 state, ERV policy inputs, HVAC temperature policy inputs, and history charts as current office climate readings.
- ERV and HVAC automation are gated independently by `erv.active_control_enabled` and `mitsubishi.active_control_enabled`.

Relevant code paths:

- `StateMachine::presence_signal_active_at()` uses door state to decide whether motion counts as inside-office presence.
- `StateMachine::update_door()` starts departure verification and door-open timers.
- `StateMachine::update_window()` sets window state used by safety interlocks.
- `StateMachine::safety_interlock_active()` treats door/window open as an ERV safety interlock.
- `evaluate_and_apply_hvac_policy()` treats window open and sustained door open as HVAC safety interlocks.
- `QingpingState::overlay_status()` publishes CO2/temp/tVOC/etc. as `air_quality` and updates `status.sensors.co2_ppm`.
- Qingping MQTT ingress triggers ERV/HVAC policy evaluation after readings are applied.
- `YoLinkState::apply_event()` logs contact events and applies door/window updates to the state machine.
- `apply_occupancy_signal()` and internal presence polling should continue to work for productivity monitoring.

The key mismatch during renovation: door/window sensors still describe the under-renovation room, and Qingping readings describe the temporary equipment room, not the renovated office. These readings are raw telemetry only; they are not valid office presence or office climate-control inputs.

## Goals

1. Disable automated ERV and HVAC writes for the renovation period.
2. Continue monitoring productivity signals.
3. Treat Mac activity and motion as the only automatic presence signals.
4. Continue logging door/window YoLink events as raw telemetry.
5. Continue ingesting Qingping readings as raw telemetry while marking them as non-office/ignored for office climate behavior.
6. Prevent door/window state from affecting:
   - occupancy transitions,
   - departure verification,
   - door-open mode,
   - ERV safety/interlock decisions,
   - HVAC safety/interlock decisions,
   - productivity summaries.
7. Prevent moved Qingping readings from affecting:
   - ERV air-quality policy inputs,
   - HVAC temperature policy inputs,
   - status fields presented as current office conditions,
   - office climate history without source/trust context.
8. Make the mode visible in status/logs so a future RCA does not misinterpret ignored contact sensors or moved air-quality readings as broken sensors or office conditions.

## Non-Goals

- Do not delete or disable YoLink device ingestion entirely.
- Do not delete or disable Qingping ingestion entirely.
- Do not stop productivity telemetry collection.
- Do not change manual present/away behavior.
- Do not permanently remove door/window semantics; this must be reversible when equipment returns.
- Do not use door/window events from the renovated space to estimate productivity during the renovation window.

## Proposed Configuration

Add a top-level config block:

```yaml
room_mode:
  renovation: false
  climate_automation_enabled: true
  contact_sensors_enabled: true
  air_quality_sensors_enabled: true
```

For the current renovation:

```yaml
room_mode:
  renovation: true
  climate_automation_enabled: false
  contact_sensors_enabled: false
  air_quality_sensors_enabled: false

erv:
  active_control_enabled: false

mitsubishi:
  active_control_enabled: false
```

`erv.active_control_enabled` and `mitsubishi.active_control_enabled` remain the immediate kill switches and should still be honored. `room_mode.climate_automation_enabled=false` is an explanatory umbrella gate that prevents future automation paths from accidentally writing while the room is marked as under renovation.

`room_mode.contact_sensors_enabled=false` means door/window events remain logged but are ignored by state and policy logic.

`room_mode.air_quality_sensors_enabled=false` means Qingping readings remain ingested and stored as raw telemetry but are not trusted as current office air-quality, temperature, humidity, noise, CO2, or tVOC readings.

`room_mode.renovation` is descriptive and observability-facing. The behavioral gates are `room_mode.climate_automation_enabled`, `room_mode.contact_sensors_enabled`, and `room_mode.air_quality_sensors_enabled`. Mixed states are valid:

- `renovation=false`, `climate_automation_enabled=false`: non-renovation climate kill switch.
- `renovation=false`, `contact_sensors_enabled=false`: contact sensor maintenance or replacement.
- `renovation=false`, `air_quality_sensors_enabled=false`: Qingping maintenance, relocation, or calibration.
- `renovation=true` with all behavioral gates enabled: allowed but should emit a validation warning, because the mode is visible but not protective.

## Runtime Semantics

### Presence

When `contact_sensors_enabled=true`, preserve the current room-boundary model.

When `contact_sensors_enabled=false`:

- `in_door_open_mode_at()` returns false.
- Contact-sensor driven departure verification is disabled.
- Door-open away timers are disabled.
- `safety_interlock_active()` ignores door/window state.
- Motion presence is direct:

```text
motion_detected || now - motion_last_seen < motion_timeout_seconds
```

- Mac presence remains:

```text
external_monitor
&& mac_last_active > 0
&& mac_last_active is newer than the last manual-away timestamp, if any
```

The important differences:

- Motion should no longer require `!door_open` or `motion_last_seen > door_last_changed`, because the door/window boundary no longer describes the active workspace.
- Mac presence must not reassert stale presence after manual-away. Track a freshness anchor such as `last_manual_away_at`, and require Mac activity to be newer than that anchor when contact sensors are disabled. Add an explicit test where manual-away is set while an external monitor remains connected; the next unchanged Mac poll must not immediately transition AWAY -> PRESENT.

Manual present/away should keep working and should continue logging to `occupancy_log`.

### YoLink Contact Events

Door/window events should still be persisted to `device_events`. This preserves raw sensor history and makes it obvious construction activity occurred.

When `contact_sensors_enabled=false`, YoLink contact events must not call:

- `StateMachine::update_door()`
- `StateMachine::restore_door_state()`
- `StateMachine::update_window()`

The existing status fields `sensors.door_open` and `sensors.window_open` must remain trusted logical state-machine fields. When contact sensors are disabled, raw YoLink door/window events must not update those fields. Add explicit mode/status fields such as `contact_sensors_enabled=false` and `contact_sensors_ignored=true`. A separate raw contact telemetry section can be added later, but do not overload existing logical sensor booleans with ignored construction telemetry.

### Startup Restore

Startup restore must honor contact sensor trust mode. `build_app_state()` currently calls `yolink.restore_from_database(...)` before serving, and the restore path reads latest door/window rows from `device_events`. When `contact_sensors_enabled=false`:

- Do not restore latest door/window state into `StateMachine`.
- Do restore motion state if it is still useful and trustworthy.
- Do not start door-open timers, departure verification, or safety interlocks from restored contact rows.
- Add tests that a database with latest `door=open` and `window=open` still starts with contact-sensor logical state ignored when renovation/contact-disabled mode is configured.

### Trusted vs Ignored Contact Events

Ignored renovation contact events must be distinguishable from trusted contact events before contact sensors are re-enabled. Otherwise a later normal-mode restart could restore construction noise as trusted room-boundary state.

Implement one of these contracts:

1. Mark contact events in `device_events.details` with a trust field, for example:

```json
{
  "contact_sensor_trusted": false,
  "ignored_reason": "renovation_contact_sensors_disabled"
}
```

Then update restore/history queries to include only trusted contact events where trusted state is required.

2. Or require a fresh trusted contact snapshot/event after `contact_sensors_enabled=true` before restoring or relying on contact state.

Preferred contract: mark ignored events in `details` and filter trusted-state restore from those details. Raw history can still show all events.

### Qingping Air Quality Events

Qingping ingestion should continue during renovation. The moved monitor can still be useful for temporary-room telemetry and device health, but it is not an office climate sensor while `air_quality_sensors_enabled=false`.

When `air_quality_sensors_enabled=false`:

- Continue accepting MQTT readings.
- Continue storing raw rows in `sensor_readings`.
- Mark readings as non-office or ignored in persisted metadata where possible. If the current schema cannot store trust metadata directly, add enough source/trust context through a schema change or parallel details field before relying on these rows for office climate history.
- Do not update state-machine office CO2 from the reading.
- Do not update `status.sensors.co2_ppm` as if it were trusted office CO2.
- Do not feed CO2/tVOC into ERV policy decisions.
- Do not feed temperature into HVAC policy decisions, including critical heat logic.
- Do not present `air_quality` as current office conditions without an explicit ignored/non-office marker.

Preferred status contract:

```json
{
  "air_quality": {
    "trusted_office_reading": false,
    "ignored_reason": "renovation_air_quality_sensor_moved",
    "co2_ppm": 640,
    "temp_c": 22.5
  },
  "sensors": {
    "co2_ppm": 400,
    "air_quality_sensors_enabled": false,
    "air_quality_sensors_ignored": true
  }
}
```

In this contract, `air_quality.*` may expose the latest raw Qingping reading as long as the trust marker is adjacent and unambiguous. `sensors.co2_ppm` remains trusted logical office CO2 and should not be overwritten by ignored readings.

### ERV

Automated ERV policy evaluation should be a no-op when either:

- `erv.active_control_enabled=false`, or
- `room_mode.climate_automation_enabled=false`.

Manual ERV writes should continue to require the existing active-control gate. During renovation, keeping `erv.active_control_enabled=false` should reject manual writes unless the user deliberately re-enables it.

Read-only ERV status polling can continue if configured. It does not drive climate control and is useful for diagnosing stale local connectivity.

This umbrella gate must cover every automated ERV trigger path, including:

- Qingping sensor update hooks,
- YoLink device update hooks,
- internal Mac presence polling,
- `/occupancy` updates,
- manual present/away follow-up policy evaluation,
- door-grace background timers.

If `room_mode.air_quality_sensors_enabled=false`, ERV policy input should treat office CO2 and tVOC as unavailable even if `room_mode.climate_automation_enabled=true`. This prevents accidental future reuse of moved Qingping data if climate automation is temporarily re-enabled before the sensor returns.

### HVAC

Automated HVAC policy evaluation should be a no-op when either:

- `mitsubishi.active_control_enabled=false`, or
- `room_mode.climate_automation_enabled=false`.

HVAC safety interlocks should ignore door/window while `contact_sensors_enabled=false`, but this should be mostly redundant while climate automation is disabled. Implement both so the behavior is correct if climate automation is temporarily re-enabled while contact sensors are still untrusted.

Manual HVAC writes should continue to require the existing active-control gate.

This umbrella gate must cover every automated HVAC trigger path, including:

- Qingping sensor update hooks,
- YoLink device update hooks,
- internal Mac presence polling,
- `/occupancy` updates,
- manual present/away follow-up policy evaluation,
- door-grace background timers.

If `room_mode.air_quality_sensors_enabled=false`, HVAC policy input should treat office temperature and humidity as unavailable even if `room_mode.climate_automation_enabled=true`. In particular, critical heat/cool and temperature-band decisions must not use moved Qingping readings as office temperature.

### Door-Grace Scheduler

When `contact_sensors_enabled=false`, `door_grace_policy_delay()` should return `None`. Door-grace scheduling is based entirely on door timestamps, so it should be inert while contact sensors are untrusted even if climate writer gates would also prevent writes. This avoids stale restored door timestamps causing repeated background evaluation and misleading logs.

### Productivity and History

Productivity monitoring should continue to ingest:

- Mac/internal presence snapshots,
- motion-driven presence,
- session/tool telemetry,
- raw climate and air-quality readings, clearly marked as non-office/ignored when `air_quality_sensors_enabled=false`.

Door/window event handling should be endpoint-specific:

- Raw `/history` should still include all door/window `device_events`, including ignored renovation events, because this is operational telemetry.
- Raw `/history` should still include Qingping `sensor_readings`, including renovation-period readings, because this is operational telemetry.
- `/history/daily-stats` should exclude ignored contact events from `door_events`, or add a separate `ignored_contact_events` count and keep trusted `door_events` separate.
- `/history/openings` should exclude ignored contact events from trusted door/window open intervals, or label ignored intervals separately.
- CO2 and temperature history endpoints should either exclude non-office Qingping readings by default or label them explicitly with source/trust context so charts cannot be mistaken for renovated-office conditions.
- Leverage/session productivity endpoints that do not consume contact events do not need changes beyond avoiding broad claims that contact events affect leverage.

The current physical office contact events are construction/context noise, not actual office entry/exit behavior.

## Status and Observability

Expose the active mode in `/status`:

```json
{
  "room_mode": {
      "renovation": true,
      "climate_automation_enabled": false,
      "contact_sensors_enabled": false,
      "air_quality_sensors_enabled": false
  }
}
```

Also expose whether contact sensors are ignored in the existing sensor status area, for example:

```json
{
  "sensors": {
    "door_open": false,
    "window_open": false,
    "contact_sensors_enabled": false,
    "contact_sensors_ignored": true,
    "air_quality_sensors_enabled": false,
    "air_quality_sensors_ignored": true
  }
}
```

Status source of truth:

- `sensors.door_open` and `sensors.window_open` are trusted logical state-machine values.
- When `contact_sensors_enabled=false`, raw ignored YoLink contact events do not update those fields.
- `contact_sensors_enabled=false` and `contact_sensors_ignored=true` explain why raw contact telemetry may differ from logical sensor state.
- `air_quality.trusted_office_reading=false` explains why raw Qingping values may differ from logical office climate state.
- `sensors.co2_ppm` remains trusted logical office CO2 and is not overwritten by ignored Qingping readings.

Add log details when skipping contact-sensor state application:

- device type,
- raw event state,
- timestamp,
- reason: `renovation_contact_sensors_disabled`.

Add log details when skipping climate automation:

- subsystem: `erv` or `hvac`,
- reason: `renovation_climate_automation_disabled`.

Add log details when applying a raw-but-ignored Qingping reading:

- source: `qingping`,
- reading timestamp,
- reason: `renovation_air_quality_sensor_moved`.

## Implementation Plan

1. Add `RoomModeConfig` in `src/config.rs`.
   - Defaults should preserve current behavior:
     - `renovation=false`
     - `climate_automation_enabled=true`
     - `contact_sensors_enabled=true`
     - `air_quality_sensors_enabled=true`
   - Add environment overrides only if consistent with nearby config patterns.
   - Add config parser tests.

2. Thread room mode into app status.
   - Add fields to `Status` and serialization tests.
   - Update dashboard consumers only if they require explicit model changes.
   - Preserve `sensors.door_open` and `sensors.window_open` as trusted logical state fields; do not repurpose them as raw ignored telemetry.
   - Preserve `sensors.co2_ppm` as trusted logical office CO2; do not overwrite it with ignored Qingping readings.
   - Add explicit air-quality trust fields to status serialization.

3. Update `StateMachine` to support contact-sensor-disabled semantics.
   - Prefer adding `contact_sensors_enabled` to `StateConfig`, sourced from `AppConfig`.
   - Update `presence_signal_active_at()`, `in_door_open_mode_at()`, `safety_interlock_active()`, and relevant timer paths.
   - Add a manual-away freshness anchor for Mac presence when contact sensors are disabled.
   - Add state-machine tests for direct motion, Mac presence, and manual-away with external monitor still connected.

4. Update YoLink event application.
   - Continue logging door/window `device_events`.
   - Mark ignored contact events in `details`, including `contact_sensor_trusted=false` and an ignored reason.
   - Skip state-machine door/window updates when contact sensors are disabled.
   - Make startup restore skip untrusted/ignored contact rows and skip all contact restore when contact sensors are disabled.
   - Add tests that door/window events are logged but do not mutate state or trigger occupancy transitions.

5. Update Qingping ingestion and trust handling.
   - Continue applying/storing raw Qingping readings.
   - Mark readings as non-office/ignored when `air_quality_sensors_enabled=false`.
   - Prevent ignored readings from updating state-machine office CO2 and trusted status sensor CO2.
   - Add tests that ignored readings appear in raw history/status with trust markers but are not presented as trusted office conditions.

6. Gate ERV automation with room mode.
   - Keep existing `erv.active_control_enabled` behavior.
   - Add an umbrella no-op when `room_mode.climate_automation_enabled=false`.
   - Treat CO2/tVOC as unavailable when `room_mode.air_quality_sensors_enabled=false`, even if climate automation is enabled.
   - Add tests that policy updates produce no automated ERV writes from Qingping, YoLink, Mac/presence, manual present/away follow-up, and door-grace triggers in renovation mode.
   - Add tests that ERV policy does not consume Qingping CO2/tVOC when air-quality sensors are disabled.

7. Gate HVAC automation with room mode.
   - Keep existing `mitsubishi.active_control_enabled` behavior.
   - Add an umbrella no-op when `room_mode.climate_automation_enabled=false`.
   - Update `hvac_safety_interlock_active()` or its caller so contact sensors are ignored when disabled.
   - Treat temperature/humidity as unavailable when `room_mode.air_quality_sensors_enabled=false`, even if climate automation is enabled.
   - Make `door_grace_policy_delay()` inert when contact sensors are disabled.
   - Add tests for sustained door-open and window-open not driving HVAC behavior in renovation mode.
   - Add tests that HVAC policy does not consume Qingping temperature/humidity when air-quality sensors are disabled.

8. Adjust productivity/history summaries.
   - Keep raw `/history` door/window events.
   - Keep raw `/history` Qingping sensor readings.
   - Exclude or label ignored door/window events in `/history/daily-stats`.
   - Exclude or label ignored door/window intervals in `/history/openings`.
   - Exclude or label non-office Qingping readings in CO2 and temperature history endpoints.
   - Add tests around those concrete endpoint payloads.

9. Update `config.example.yaml` and deployment notes.
   - Document the renovation configuration.
   - Include the restore checklist:
     - set `room_mode.renovation=false`,
     - set `room_mode.contact_sensors_enabled=true`,
     - set `room_mode.climate_automation_enabled=true`,
     - set `room_mode.air_quality_sensors_enabled=true`,
     - re-enable `erv.active_control_enabled` and `mitsubishi.active_control_enabled` when equipment is back.
     - require a fresh trusted door/window event or snapshot before relying on contact-sensor state.
     - require a fresh trusted Qingping reading from the office before relying on air-quality history/status or climate automation.

## Acceptance Criteria

1. With renovation mode enabled and contact sensors disabled, a door-open YoLink event is logged but does not set state-machine `door_open`, does not start door-open mode, and does not start departure verification.
2. With renovation mode enabled and contact sensors disabled, a window-open YoLink event is logged but does not trip ERV or HVAC safety interlocks.
3. With contact sensors disabled, startup/database restore does not apply latest trusted-looking or ignored door/window rows into the state machine; motion restore can still work.
4. Ignored contact events are marked in `device_events.details`, and trusted contact restore/history queries do not treat ignored rows as trusted room-boundary state.
5. Motion alone transitions AWAY -> PRESENT when contact sensors are disabled.
6. Fresh Mac activity with external monitor transitions AWAY -> PRESENT when contact sensors are disabled.
7. Manual away while an external monitor remains connected is not immediately undone by stale Mac activity on the next poll.
8. Manual present/away continues to work.
9. Automated ERV writes are not emitted from Qingping, YoLink, Mac/presence, manual present/away follow-up, or door-grace triggers when `room_mode.climate_automation_enabled=false`.
10. Automated HVAC writes are not emitted from Qingping, YoLink, Mac/presence, manual present/away follow-up, or door-grace triggers when `room_mode.climate_automation_enabled=false`.
11. If `air_quality_sensors_enabled=false`, ERV policy does not consume Qingping CO2/tVOC as office air-quality input, even if climate automation is enabled.
12. If `air_quality_sensors_enabled=false`, HVAC policy does not consume Qingping temperature/humidity as office climate input, even if climate automation is enabled.
13. `door_grace_policy_delay()` returns no scheduled work when contact sensors are disabled.
14. `/status` clearly reports renovation mode and contact sensors ignored, while `sensors.door_open/window_open` remain trusted logical state fields.
15. `/status` clearly reports air-quality readings as ignored/non-office when air-quality sensors are disabled, and `sensors.co2_ppm` is not overwritten by ignored Qingping readings.
16. Raw `/history` includes ignored door/window events and non-office Qingping readings, while `/history/daily-stats`, `/history/openings`, CO2 history, and temperature history exclude or explicitly label ignored/non-office data.
17. When defaults are used, all existing contact-sensor, air-quality, ERV, and HVAC behaviors are preserved.

## Rollout

1. Merge the code with defaults preserving current behavior.
2. Deploy with only:

```yaml
room_mode:
  renovation: true
  climate_automation_enabled: false
  contact_sensors_enabled: false
  air_quality_sensors_enabled: false

erv:
  active_control_enabled: false

mitsubishi:
  active_control_enabled: false
```

3. Verify:
   - `/status` shows renovation/contact-sensors-ignored.
   - `/status` shows Qingping readings ignored/non-office.
   - Motion or Mac activity can still mark present.
   - Door/window events appear in history but do not change occupancy.
   - Qingping readings appear in raw history but do not update trusted office CO2/temp.
   - ERV/HVAC writes are not emitted by automation.

4. When equipment returns:
   - Re-enable `contact_sensors_enabled`.
   - Move Qingping back into the office, then re-enable `air_quality_sensors_enabled`.
   - Require a fresh trusted door/window event or explicit contact-state refresh.
   - Require a fresh trusted Qingping reading from the office.
   - Only after trusted contact and air-quality state are fresh, re-enable `climate_automation_enabled`, `erv.active_control_enabled`, and `mitsubishi.active_control_enabled`.
   - Verify `/status` shows trusted logical door/window and air-quality state before relying on automation.

## Classification

Single ticket. The implementation touches several modules, but the behavioral surface is cohesive and bounded: add renovation/contact-sensor trust mode, gate climate automation, and preserve productivity monitoring.
