# Fix orchestrator thrashing ERV → triggers device local-key drift

## Summary

The orchestrator's AWAY-mode adaptive speed logic toggles the ERV between QUIET and OFF every 15–60 seconds when CO2 hovers near the plateau threshold. Each toggle issues 2–4 local Tuya commands. Sustained over ~5 minutes, this command burst is followed by the ERV beginning to reject the previously valid local key, breaking subsequent local control with Err 914. (Background section discusses the inferred device-side mechanism.)

Today's incident: ~10 toggles between 07:51 and 07:59 → first 914 at 08:03:24 → orchestrator notification fired at the 5th consecutive failure threshold.

## Background — what's known

- The local key drift is **not** caused by Tuya firmware OTA. Confirmed via:
  - User has Smart Life "auto update" disabled for this device.
  - Smart Life MMKV cache and Tuya cloud HA-sharing API both hold the **same** localKey as the orchestrator (redacted); the device rejects all three. So the rotation is device-side, not cloud-side.
  - Smart Life's `localConnectLastUpdateTime` for this device is from Dec 2025 (~5 months ago). Smart Life has been entirely cloud-relayed for fan control since then. During those 5 months, drift has continued — and the orchestrator was the only thing doing local Tuya protocol against the device.
- The MMKV dump confirms `wifi.verSw=2.7.7`, `mcu.verSw=1.0.0` — no in-flight OTA at the time of today's failure.
- The 4-minute gap between last successful local op (07:59:17) and first 914 (08:03:24) is inconsistent with OTA (which would involve a reboot and version bump).

The most defensible explanation: the ERV's local Tuya stack began rejecting the previously valid local key after the command burst. The exact device-side mechanism (defensive rotation, session-state corruption, watchdog reset, etc.) is inferred, not directly observed.

## Root cause — three bugs that compound

All in `src/orchestrator.py`.

### Bug 1: `_plateau_detected` is set but never consulted (lines 538–574, 586–588)

`_detect_co2_plateau()` re-evaluates from scratch each tick. It returns `True` only when the current 10-min rolling rate is `< 0.5 ppm/min`. The `self._plateau_detected = True` assignment on line 588 has no effect on subsequent ticks — the function never reads it. Today it is consulted only for log-string formatting (line 887) and cleared on PRESENT (lines 1365-1368).

Result: plateau "detection" is purely instantaneous, not latching.

### Bug 2: Adaptive logic returns "quiet" when plateau-not-declared but rate-very-low (line 618)

`_get_adaptive_erv_speed_for_away()` returns `"quiet"` whenever rate < 0.5 but `_detect_co2_plateau()` returned False (e.g. due to insufficient readings, rate at exactly the threshold, or noise-floor jitter). Combined with Bug 1, the function alternates `off` / `quiet` on every sample as the rolling rate crosses the 0.5 ppm/min line.

### Aggravator: `erv_client.py` does multiple local ops per `set_speed` (`_set_speed_local`, lines 256–315)

- `set_speed(OFF)` = 2 local round-trips: `set_value(power=False)` + `get_status` verify
- `set_speed(QUIET / MEDIUM / TURBO)` = 4 local round-trips: `set_value(power=True)` + `set_value(supply)` + `set_value(exhaust)` + `get_status` verify

A single `quiet → off` toggle pair is ~6 round-trips. The observed 10 toggles in ~5 minutes amounts to roughly 30–40 local round-trips against a device that was sitting in steady state.

## Proposed fix

Two layered changes in `src/orchestrator.py` plus a configurable catch-all. (A previously proposed "rate hysteresis" change was dropped — release semantics are handled by Change 1's baseline-delta release; layering a release-rate on first-entry detection was muddled.)

### Change 1: Use `_plateau_detected` as actual sticky state with a release condition

Add a config field in `src/config.py`:

```python
co2_plateau_release_delta_ppm: int = 100   # release plateau when CO2 climbs this far above learned baseline
```

In `_detect_co2_plateau()`:

```python
def _detect_co2_plateau(self) -> bool:
    if not self.config.thresholds.co2_plateau_enabled:
        return False

    # Inconsistent state guard: if detected is True but baseline is None,
    # something cleared baseline without clearing detected. Reset both and recompute.
    if self._plateau_detected and self._outdoor_co2_baseline is None:
        self._plateau_detected = False

    # Sticky: once latched, stay latched until CO2 climbs meaningfully above baseline.
    if self._plateau_detected and self._outdoor_co2_baseline is not None:
        current_co2 = self._co2_history[-1][1] if self._co2_history else None
        if current_co2 is not None and current_co2 < (
            self._outdoor_co2_baseline + self.config.thresholds.co2_plateau_release_delta_ppm
        ):
            return True
        # CO2 climbed past the release delta — break out and recompute fresh
        self._plateau_detected = False
        self._outdoor_co2_baseline = None

    # ... existing first-time detection logic unchanged ...
```

**State reset on presence transitions:** today, `_plateau_detected` and `_outdoor_co2_baseline` are cleared on PRESENT but not on AWAY entry. Add clearing of both on AWAY entry so a stale plateau from an earlier AWAY session doesn't carry over.

### Change 2 (was Change 3): Minimum dwell time on automated within-state transitions

Add to `ThresholdsConfig`:

```python
erv_min_dwell_seconds: int = 180   # minimum interval between automated ERV speed changes within a stable presence state
```

Add to orchestrator state:

```python
self._erv_speed_changed_at: Optional[datetime] = None
```

**Centralize all ERV speed changes through one helper** to avoid scattering dwell logic across call sites:

```python
def _apply_erv_speed(
    self,
    target_speed: str,                # "off"|"quiet"|"medium"|"turbo"
    fan_speed: Optional[FanSpeed],    # None when target_speed=="off"
    reason: str,
    co2: Optional[int],
    bypass_dwell: bool = False,
) -> bool:
    # Dwell check
    if not bypass_dwell and self._erv_speed_changed_at is not None:
        elapsed = (datetime.now() - self._erv_speed_changed_at).total_seconds()
        if elapsed < self.config.thresholds.erv_min_dwell_seconds:
            logger.info(
                f"ERV transition to {target_speed} suppressed by dwell "
                f"({elapsed:.0f}s < {self.config.thresholds.erv_min_dwell_seconds}s, reason={reason})"
            )
            return False  # do not write a climate_actions row — no device action occurred

    # Issue the device command via self.erv (turn_off / turn_on)
    if target_speed == "off":
        ok = self.erv.turn_off()
    else:
        ok = self.erv.turn_on(fan_speed)

    if ok:
        self._erv_running = target_speed != "off"
        self._erv_speed = target_speed
        self._erv_speed_changed_at = datetime.now()   # update only on success
        self.db.log_climate_action("erv", target_speed, co2_ppm=co2, reason=reason)
    return ok
```

Refactor all current call sites (`_evaluate_erv_state` and friends — see grep for `self.erv.turn_on` / `self.erv.turn_off`) to route through `_apply_erv_speed`, passing `bypass_dwell=True` from the appropriate paths.

**Dwell bypass list** (`bypass_dwell=True`):

1. **Safety interlocks** — window/door open → OFF
2. **Manual override** — explicit user action via API / dashboard
3. **Shutdown** — orchestrator clean exit
4. **PRESENT critical CO2** — entering ELEVATED at CO2 ≥ 2000 ppm. This is a safety-tier event; stale ventilation is worse than a brief command-rate concern.
5. **Presence state transitions** — any ERV change initiated by AWAY → PRESENT or PRESENT → AWAY (including the AWAY-mode initial 30-min TURBO at state entry, and PRESENT cleanup).

**Dwell-enforced paths** (`bypass_dwell=False`, the default):

- AWAY-mode adaptive speed changes within a stable AWAY state.
- PRESENT-mode hysteresis-driven on/off around the 1800–2000 CO2 band.
- tVOC adaptive within-state changes.
- Stale-air periodic flush.

**Rule of thumb:** dwell applies to automated transitions within a stable presence state. Any transition caused by — or coincident with — a presence event, safety event, or explicit user action bypasses dwell.

### (Dropped) Rate hysteresis

The previously sketched "release rate" threshold was conflating entry semantics with release semantics. Change 1's baseline-delta release is the correct latching mechanism. If post-fix observation shows entry chatter (rare oscillation right at first plateau detection), revisit with proper sticky-state-release semantics — but do not ship it in this PR.

### Optional follow-up: trim `erv_client.py` read-back verify

The per-`set_speed` command multiplier is real but secondary. Once Changes 1–2 stop the thrash, this is optimization. Defer unless local protocol abuse symptoms persist after the fix.

## Acceptance criteria

1. With CO2 history representing 510–519 ppm noise (matching today's 07:51–07:59 sensor sequence) and AWAY state post-30-min, the orchestrator emits **at most 1** ERV transition after plateau first latches.
2. AWAY mode active for >30 min with CO2 at outdoor baseline → no ERV toggles for at least 30 minutes under noisy-but-stable readings.
3. CO2 climbing by ≥ `co2_plateau_release_delta_ppm` (default 100) above the learned baseline must break plateau and allow ventilation to resume (regression check against locking OFF forever).
4. Safety interlocks (window/door open → OFF) fire immediately regardless of dwell timer state.
5. Walking into the room (AWAY → PRESENT) takes effect immediately regardless of dwell timer state.
6. Manual override commands take effect immediately regardless of dwell timer state.

## Test plan

- **Unit test `_detect_co2_plateau()` sticky behavior**: synthesize a `_co2_history` sequence at ~512 ppm with ±2 ppm noise, manually set `_plateau_detected = True` and `_outdoor_co2_baseline = 512`, assert function returns True across many ticks even as rate jitters. Then push CO2 to 612 and assert it transitions to False.
- **Unit test `_apply_erv_speed` dwell suppression**: with injected fake clock (see below), set `_erv_speed_changed_at = now()`, call helper with `bypass_dwell=False`, assert it returns False AND no `climate_actions` row was written. Advance fake clock past `erv_min_dwell_seconds`, call again, assert it proceeds.
- **Unit test dwell bypass**: with the same fresh dwell timer, call helper with `bypass_dwell=True`, assert it proceeds and writes a climate_actions row.
- **Integration test against today's sensor data**: replay the production `sensor_readings` sequence from 2026-05-21 07:45–08:08 (CO2 510–519 ppm range, ~30s cadence) through the fixed orchestrator in AWAY state, post-initial-TURBO window. Assert ≤1 ERV state transition emitted.
- **Test fake clock**: before testing dwell or the 30-minute turbo-window, introduce a `_now()` helper on the orchestrator (or accept an injectable clock) so tests don't depend on `datetime.now()` directly. Several existing tests will benefit from this too.
- **Audit existing stale-flush tests**: they currently perform repeated state changes in quick succession; with dwell central, some assertions may need to assert "suppressed by dwell" rather than "transition occurred."

## Out of scope

- The network/WAN block at the BGW (was previously proposed; no longer justified — see Background).
- Cloud-relay rewrite of `erv_client.py` to use `tuya_sharing.Manager.send_commands` (was previously proposed; not needed if local control isn't being abused).
- Daemon (`scripts/refresh-erv-key.py`) changes — already working; remains the recovery tool after a re-pair.
- The eero → BGW bridge cutover (independent project, on its own merits).
- `erv_client.py` read-back verify trim — deferred, see "Optional follow-up" above.

## Runbook docs cleanup

This PR also updates `docs/tuya-local-key.md`. Its previous "When To Run This" and "Prevention" sections attributed Err 914 almost entirely to firmware OTA. That phrasing is now stale and misleading. The runbook now notes:

- This incident's diagnosis showed orchestrator command-burst as a real cause distinct from OTA.
- Re-pair (Path B) is the only path to a working local key once the device starts rejecting it.
- The thrash-fix PR (this spec) eliminates the orchestrator-side trigger going forward.

## Ticket classification

**Single ticket.** One agent can complete both changes, the centralization refactor, and the tests without context compaction. Recommend filing as a GH issue with this spec linked, then implementing in one PR.
