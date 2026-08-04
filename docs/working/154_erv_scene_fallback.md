# ERV: Smart Life Scene Control with Local Read-Only Verification

Issue: #154

## Summary

ERV automation was dead for roughly a month. Local Tuya control failed with Err 914 and there was no fallback path, so a single credential failure took out all ERV control while every other subsystem kept running.

Move ERV writes onto Smart Life tap-to-run scenes, make local Tuya read-only, and remove the status polling loop that aggravates the failure mode.

**The two transports are split by direction, not by preference order:**

| Trait method | Default transport | Effect |
|---|---|---|
| `set_speed` | Smart Life scene trigger | no local command is ever issued |
| `smoke_status` | local Tuya read | read-only; the only view of the speed DPs |

Issuing no local *commands* removes the documented cause of the Err 914 lockout (`docs/tuya-local-key.md`), while local reads remain the only way to observe fan speed.

## Why local can't just be fixed

The recurring failure is a local-key desync: the configured key can match the cloud's copy *exactly*, and the protocol version can match the device's own broadcast, and the handshake still returns Err 914. `scripts/refresh-erv-key.py` cannot repair this — it only helps when the cloud holds a *newer* key than config. The only fix is a hardware re-pair, which is manual and has happened more than once.

So local control cannot be the sole path. But it also cannot be dropped, because of the constraint below.

## Hard Constraints (measured — do not re-derive)

These were established empirically against the live device. Several are counter-intuitive.

### The cloud cannot see or set fan speed

Fan speed lives in private data points (`101` = supply, `102` = exhaust; see `DP_SUPPLY_SPEED` / `DP_EXHAUST_SPEED` in `erv.rs`). The Smart Life sharing API does not expose them:

- **Status** returns only standard codes — `switch`, `mode`, `pm25`, `eco2`, `tvoc`, `humidity_indoor`, `temp_indoor`, `filter_reset`, `fault`. The speed DPs are absent.
- **Writable functions** are only `switch`, `mode`, `filter_reset`. There is no cloud path to set speed directly.

Scenes are the *only* cloud mechanism that reaches speed, because the scene replays private DPs server-side.

### ⚠ The scene read view is lossy — do not be misled by it

Listing scenes renders every ERV speed scene's action as the same JSON:

```json
{"action_executor": "dpIssue", "entity_id": "<device>",
 "executor_property": {"switch": true}}
```

**This is a rendering artifact, not the stored scene.** The sharing API hides private DPs, and it hides them in scene actions exactly as it hides them in device status — so the part that distinguishes a high-speed scene from a low-speed one is precisely the part the view drops. The scenes were verified physically (display readout and audible fan speed) to set distinct supply/exhaust values. Scene execution happens server-side against the real stored definition; the trigger endpoint only names the scene and is not constrained by what the read endpoint can display.

Do not "fix" the scenes based on this JSON. Do not assert scene equivalence from it.

### Scenes cannot be created or edited via API

All create/modify/delete paths return `1108 uri path invalid`; the sharing credential is read + trigger only. Scenes are hand-built in the Smart Life app. Budget for manual work if scene definitions ever need to change, and never assume a missing scene can be provisioned in code.

## Design

Four independent changes; they can land separately.

### 1. Split the writer by direction

`ErvSpeedWriter` (`erv.rs`) is already a trait behind `Arc<dyn>`, injected in `http.rs` and consumed by the coordinator in `automation.rs`. **Keep the trait signature unchanged** — that keeps policy, dwell, manual override, burst guard, DB logging, the `/erv` endpoint, and both clients untouched, and keeps every existing `FakeErvWriter` test compiling.

Add:

- `SceneErvSpeedWriter` — maps `ErvFanSpeed` + negative-pressure flag to a configured scene id and triggers it.
- `SplitErvWriter { writer, reader }` — `set_speed` delegates to the scene writer, then verifies; `smoke_status` delegates to the local reader.

This combination is strictly better than either path alone: scenes reach every speed, and the local read confirms the scene actually landed, so `verify_speed` keeps working unchanged against real supply/exhaust values.

### 2. Verification ladder

Verification happens **only when we change device state** — never on a cadence. After a write, wait `verify_delay_seconds`, then confirm using the best available source:

1. **Local read** (default) — returns true supply/exhaust. Full verification against `verify_speed`.
2. **Cloud read** — used *only when the local key is dead*. Returns the `switch` bit and nothing else, so it can only confirm power state. Note this is worth doing on power transitions but is **uninformative on speed-only changes** (`switch` is `true` before and after); skip it there rather than spend a call for no signal.
3. **Assumed state** — if neither is available, trust the command and mark the status accordingly.

A failed verification must **not** fail the write. Degrade observability, never control.

### 3. Automatic local-write fallback

If a scene trigger fails (WAN down, Tuya cloud unreachable) **and** the local path is healthy, issue the write locally. Automatically, with no prompt and no toggle.

This is what "fallback" means: the system stays controllable on a LAN-only network without anyone being in the loop. An outage is precisely the moment the owner is least able to intervene, and a manual switch would be unreachable anyway — the phone app depends on the same connectivity. Do not surface this as a choice; decide it in code from observed health.

The local-write path stays rare by construction (it only runs when the cloud is down), so it does not reintroduce the command volume that causes the lockout.

### 4. Remove the polling loop

Delete `start_erv_status_poll` and its call site in `http.rs`. Replace with:

- **one read at boot** to establish true state; if it fails, trigger the off-scene to force a known state;
- **read-after-write** only, per the ladder above.

This drops local traffic from hundreds or thousands of reads/day to roughly the number of actual writes (~30/day measured). Keep the burst guard unchanged — it protects the device regardless of path.

### 5. Status truth and its source

`ErvState.latest_status` becomes either observed or assumed. Add a source marker (e.g. `Local | Cloud | Assumed`) surfaced in `ErvControlStatus` (`status.rs`) so clients can distinguish "the ERV is at turbo" from "we told it turbo and could not confirm".

### 6. Stop letting a broken local key block control

`local_key_invalid` currently hard-bails every write path in `erv.rs`, and `set_speed_with` additionally requires a *local read* to succeed before writing. Together these are why one credential failure killed all control.

Under this design those gates are simply wrong: the local key has no bearing on whether a scene trigger will work. A stale local key must cost **speed readback only** — never control. Scope every one of these checks to the local path.

Rework the `erv_local_key_invalid` notification from "control is dead, run the runbook" to "speed readback degraded, control unaffected". It is no longer a critical outage.

## Config

All values live in `config.yaml` (gitignored) under `erv:`. Mirror the dual-mode pattern from `BlindsConfig` in `config.rs`, including a `scene_configured()` predicate alongside `is_configured()`.

Keys to read (values are site-specific and already populated on the host):

- `smart_life_home_id`
- `off_scene_id`, `quiet_scene_id`, `medium_scene_id`, `turbo_scene_id`
- `quiet_negative_pressure_scene_id`, `medium_negative_pressure_scene_id`, `turbo_negative_pressure_scene_id`
- `smart_life_auth_file` — optional, defaults to the same path `blinds.rs` uses
- `control_mode` — `scene` | `local`, which transport issues writes
- `local_readback_enabled` — local read-after-write for true speed
- `local_write_fallback_enabled` — automatic local write when a scene trigger fails

**Intended defaults:** `control_mode: "scene"`, `local_readback_enabled: true`, `local_write_fallback_enabled: true`. Encode them in `ErvConfig::default()`, not just in the YAML.

### Negative-pressure coverage is complete

`speed_preset` in `erv.rs` defines negative-pressure variants for all three speeds. **All three now have scenes**, so the scene path covers the full `(speed × pressure)` matrix with no gaps. Negative pressure is dormant while `post_renovation_expires_at` is in the past; re-arming it needs only a date change, no new scenes.

Still implement the missing-scene case defensively: if a `(speed, pressure)` pair ever resolves to no configured scene id, **fail loudly and surface it in status** rather than silently substituting the normal-pressure variant. A system that reports a mode it is not delivering is worse than one that reports an error.

## Implementation Steps

1. **Extract a shared `smart_life` module** from `blinds.rs` — the auth cache, token refresh, client, and AES-GCM/HMAC helpers are all device-agnostic. `blinds.rs` keeps only its scene trigger. Pure refactor; existing `blinds.rs` tests cover it.

   **Move the hardcoded Smart Life client id into config as part of this step.** It is currently a literal in both `blinds.rs` and `scripts/refresh-erv-key.py`, where the two copies can silently drift. It is not a secret — it is the shared Home Assistant Tuya integration identifier — but it is a third-party constant that has changed before, and needing a Rust rebuild to track it is wrong. Give it a config key with the current value as the default so existing deployments are unaffected.

2. **Add an auth-cache mutex.** Token refresh *rotates the refresh token*. Once blinds and ERV share one cache file, two concurrent refreshes can clobber each other and lose cloud auth for both devices. This bug does not exist today only because blinds is the sole caller — adding the ERV is what introduces it. Guard load/refresh/save with a process-wide async mutex.

3. **Extend `ErvConfig`** with the scene fields and mode flags; add `scene_configured()`. Add matching env overrides if following the existing pattern in `config.rs`.

4. **Add `SceneErvSpeedWriter` and `SplitErvWriter`.**

5. **Wire writer selection** in `http.rs` from the mode flags. Default assembly is `SplitErvWriter { writer: SceneErvSpeedWriter, reader: RustuyaErvStatusReader }`.

6. **Implement the verification ladder** (§2) and the automatic local-write fallback (§3), driven by observed path health rather than configuration alone.

7. **Remove the polling loop**; add the boot read.

8. **Re-scope the gates** — the `local_key_invalid` bails and the pre-write local smoke read in `erv.rs`.

9. **Fix the read-only smoke paths.** `smoke_erv` is called from `cli.rs` and `validation.rs`; `validation.rs` hard-bails when local ERV credentials are absent, which breaks shadow validation in scene-only mode.

10. **Update status and notifications** — `status.rs`, and the ERV notification constructors in `erv.rs`.

11. **Docs** — update `docs/tuya-local-key.md` to describe re-pair as the path for restoring *speed readback* rather than restoring control, and note that the refresh script cannot fix a both-copies-stale desync.

## Testing

- Existing `FakeErvWriter` suites in `automation.rs`, `http.rs`, and `yolink.rs` must keep passing unchanged — if they don't, the trait was altered and shouldn't have been.
- In the default config, a `set_speed` issues a scene trigger and **no local command** — assert the local writer is never invoked. This is the property the whole design rests on; test it directly.
- Scene write succeeds + local read succeeds → status source `Local`, real supply/exhaust values.
- Scene write succeeds + local read fails → write still succeeds, status source `Assumed`, control not blocked.
- `local_key_invalid` does not block a scene write.
- Scene trigger fails + local healthy → automatic local write, no prompt.
- Scene trigger fails + local unhealthy → error surfaced.
- Cloud verification is skipped on speed-only changes and used on power transitions.
- Missing scene for a `(speed, pressure)` pair fails loudly rather than substituting.
- Auth-cache refresh under concurrent blinds + ERV access does not lose the refresh token.
- Manual: trigger each speed and confirm supply/exhaust on the unit's display, since no automated check can observe speed through the cloud.

## Out of Scope

- **Local/cloud toggle in the Android app** — tracked separately. The Android client is the primary control surface; the web dashboard is rarely used.
- Reviving the Tuya IoT Cloud developer API. It is unavailable (expired subscription) and no code reads its config block today.
- Using the ERV's own air-quality readings — the existing sensor already covers this.
