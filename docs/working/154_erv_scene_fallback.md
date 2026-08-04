# ERV: Smart Life Scene Fallback and Polling Removal

Issue: #154

## Summary

ERV automation was dead from 2026-07-02 to 2026-08-03 (last successful write: `2026-07-02 15:58`, while HVAC kept writing normally). Local Tuya control failed with Err 914 and there was no fallback path, so a single credential failure took out all ERV control for a month.

This ticket moves ERV control onto Smart Life tap-to-run scenes and removes the status polling loop that is the documented aggravator of the failure mode.

**The default is cloud-primary, with local Tuya used for speed readback only:**

- **Writes go through scenes.** The cloud path has no local key to go stale, and — critically — issuing no local *commands* removes the documented cause of the Err 914 lockout entirely.
- **Reads go through local Tuya, read-only.** Local is the only path that can see fan speed, so it is used for read-after-write verification and nothing else. It never issues a command in the default configuration.

This inverts the usual primary/fallback framing: the two paths are split by *direction*, not by preference order. Local writes remain reachable via `control_mode: "local"` but are off by default.

## Background

The ERV is a Pioneer ECOasis 150 (`ERVQ-H-F-BM`, device `ebfb18b2fc8f6dc63eqvcw`) on Tuya protocol 3.4 at `192.168.4.59`.

The 2026-07 outage was a local-key desync: `config.yaml`'s key matched Smart Life's cached copy *exactly*, and the protocol version matched the device's own broadcast, yet the handshake still returned Err 914. Per `docs/tuya-local-key.md:187` this is the "both copies stale" case, and only a hardware re-pair fixes it — `scripts/refresh-erv-key.py` cannot, because it only helps when the cloud has a *newer* key than config.

The re-pair was done on 2026-08-03 and local control is working again. But the same failure will recur, and `docs/tuya-local-key.md:20` names the aggravator: rapid local command bursts that the device treats as adversarial. Today the server issues 288–1440 local status reads/day from `start_erv_status_poll` plus a blocking read before every write, against only ~30 actual writes/day. That ratio is the thing to fix.

## Hard Constraints (measured 2026-08-03 — do not re-derive)

These were established empirically. Several are counter-intuitive; trust them over assumptions.

### Tuya Smart Life sharing API

App key `HA_3y9q4ak7g4ephrvke`; refreshable token cache at `~/.office-automate/tuya-sharing-auth.json`. Already implemented in Rust at `blinds.rs:161-441`.

| Capability | Endpoint | Result |
|---|---|---|
| Read device detail | `GET /v1.0/m/life/ha/devices/detail?devIds=` | works, ~420 ms |
| List scenes | `GET /v1.0/m/scene/ha/home/scenes?homeId=` | works, ~100 ms |
| Trigger scene | `POST /v1.0/m/scene/ha/trigger` `{homeId, sceneId}` | works |
| Create/modify/delete scene | `…/save`, `…/add`, `…/create`, `…/modify`, `…/delete` | **all return `1108 uri path invalid`** |

- **The cloud cannot read fan speed.** Device status exposes exactly 9 standard codes — `switch`, `mode`, `pm25`, `eco2`, `tvoc`, `humidity_indoor`, `temp_indoor`, `filter_reset`, `fault`. The speed DPs (101/102) are private and absent.
- **The cloud cannot write fan speed directly.** Writable `function` set is only `switch`, `mode`, `filter_reset`.
- **Scenes are not creatable via API.** They must be hand-built in the Smart Life app. Budget for this if scene definitions ever need to change.

### ⚠ The scene read view is lossy — do not be misled by it

`GET /v1.0/m/scene/ha/home/scenes` renders all four ERV scenes' actions as the *same* JSON:

```json
{"action_executor": "dpIssue", "entity_id": "ebfb18b2fc8f6dc63eqvcw",
 "executor_property": {"switch": true}}
```

**This is a rendering artifact, not the stored scene.** The sharing API hides private DPs, and it hides them in scene actions exactly as it hides them in device status — so the part that distinguishes HighERV from LowERV is precisely the part the view drops. The scenes were verified physically (display readout and audible fan speed) to set distinct SA/EA values. Scene execution happens server-side against the real stored definition; `/trigger` only names the scene and is not constrained by what the read endpoint can display.

Do not "fix" the scenes based on this JSON. Do not assert scene equivalence from it.

### Tuya IoT Cloud developer API — dead end

`config.yaml`'s `tuya_cloud` block (`access_id`/`access_secret`) authenticates successfully but every device call returns `28841002: IoT Core service subscription has expired`. This API *would* expose arbitrary DPs including speed, but it is unavailable. **No Rust code reads the `tuya_cloud` block today** — it is dead config.

### Local Tuya — the only speed-capable path

Working post-re-pair. Observed DP map:

| DP | Meaning | DP | Meaning |
|---|---|---|---|
| 1 | power (bool) | 9 | temp_indoor (°F) |
| 2 | mode (`"manua"`) | 13 | filter_reset |
| 3 | pm25 | 18 | fault |
| 6 | eco2 | **101** | **supply speed (SA)** |
| 7 | tvoc | **102** | **exhaust speed (EA)** |
| 8 | humidity | 105 | unknown, observed `"3"` |

Speed DPs persist across power-off (device read `101=2, 102=3` while `1=false`), so turning the unit on via a switch-only path resumes its last speed.

## Already Done (do not redo)

- ERV re-paired 2026-08-03. **Device id did not change** (`ebfb18b2fc8f6dc63eqvcw`), so scene bindings and `device_id` config survived.
- New local key written to `config.yaml:31`. Verified: local read at protocol 3.4 returns DP 101/102.
- Five scenes exist and are bound to the correct device; their ids are in `config.yaml`.
- New config keys added to `config.yaml` (inert — `ErvConfig` uses `#[serde(default)]`, so unknown keys are ignored until this ticket lands).

**The running server has not restarted since 2026-07-10 and is still using the old key.** It must be restarted to pick up `config.yaml`.

## Design

Three independent changes; they can land separately.

### 1. Split the writer by direction: scenes write, local reads

`ErvSpeedWriter` (`erv.rs:94-103`) is already a trait behind `Arc<dyn>`, injected at `http.rs:303` and `http.rs:554` and consumed by the coordinator at `automation.rs:21`. **Keep the trait signature unchanged** — that keeps policy, dwell, manual override, burst guard, DB logging, the `/erv` endpoint, the frontend, and Android all untouched, and keeps every existing `FakeErvWriter` test compiling.

The trait has two methods, and in the default configuration they are served by *different transports*:

| Trait method | Default transport | Notes |
|---|---|---|
| `set_speed` | Smart Life scene trigger | no local command is ever issued |
| `smoke_status` | local Tuya read | read-only; the only way to see DP 101/102 |

Add:

- `SceneErvSpeedWriter` — maps `ErvFanSpeed` (+ negative-pressure flag) to a scene id and POSTs `/v1.0/m/scene/ha/trigger`.
- `SplitErvWriter { writer, reader }` — `set_speed` delegates to the scene writer, then (when `local_readback_enabled`) waits `verify_delay_seconds` and confirms via a local read; `smoke_status` delegates to the local reader.

This combination is strictly better than either path alone: scenes can reach every speed, and the local read confirms the scene actually landed — so `verify_speed` (`erv.rs:1152`) keeps working unchanged against the real SA/EA values.

If the local read fails, the write is **not** failed: fall back to the synthesized expected status marked assumed (see §3). A dead local key must degrade observability, never control.

Optional and off by default: `local_write_fallback_enabled`, which tries a local write when a scene trigger fails. This is the only thing that keeps the ERV controllable during an internet outage, but it reintroduces local commands, so it is opt-in. See open question below.

### 2. Remove the polling loop

Delete `start_erv_status_poll` (`erv.rs:1059-1079`) and its call site at `http.rs:592`. Replace with:

- **one read at boot** to establish true state;
- **read-after-write** when `local_readback_enabled` — write, wait `verify_delay_seconds`, read, verify against `verify_speed` (`erv.rs:1152`).

This drops local traffic from 288–1440 reads/day to ~30. Retain `status_poll_delay` only if something still needs it after the loop is gone; otherwise remove it too.

Keep the burst guard (`erv.rs:34-35`, 3 writes / 5 min) unchanged — it protects the device regardless of path.

### 3. Status truth and its source

`ErvState.latest_status` becomes either observed or assumed. Add a source marker (e.g. `status_source: Local | Assumed`) surfaced in `ErvControlStatus` (`status.rs:207-216`) so the dashboard can distinguish "the ERV is at turbo" from "we told it turbo and could not confirm".

Assumed state is set from the last successful command. It is used when local readback is unavailable or disabled. Cloud status *can* still confirm the `switch` bit — but note that verifying a speed-only change (quiet→medium) carries **zero information**, since `switch` is `true` before and after. Only power transitions are worth a cloud check. Measured split: 65% power transitions, 35% speed-only.

Boot sequence: attempt a local read first; only if that fails, trigger `ERVOff` to force reality into a known state.

### 4. Stop letting a broken local key block control

`local_key_invalid` currently hard-bails every write path at `erv.rs:278-280`, `erv.rs:333-335`, `erv.rs:454-456`, and `set_speed_with` additionally requires a *local read* to succeed before writing (`erv.rs:295-298`). Together these are why one credential failure killed all control for a month.

Under the default config these gates are simply wrong: the local key has no bearing on whether a scene trigger will work. A stale local key must cost us **speed readback only** — never control. Scope every one of these checks to the local path.

Rework the `erv_local_key_invalid` notification (`erv.rs:1293-1306`) from "control is dead, run the runbook" to "speed readback unavailable, control unaffected" — it is a degraded-observability warning, not a critical outage.

## Config Schema

Already present in `config.yaml:27-50`. Mirror the dual-mode pattern from `BlindsConfig` (`config.rs:352-378`), including a `scene_configured()` predicate alongside `is_configured()`.

```yaml
erv:
  device_id: "ebfb18b2fc8f6dc63eqvcw"
  local_key: "…"                 # rotated 2026-08-03
  ip: "192.168.4.59"

  smart_life_home_id: "8171319"
  off_scene_id: "NVxK3R8JtFuLHS3c"      # ERVOff
  quiet_scene_id: "vFWqz0OGpMXMf3Bu"    # LowERV        SA/EA 1/1
  medium_scene_id: "cSFjNL8GvkwLqAAW"   # MidERV        SA/EA 3/2
  turbo_scene_id: "A8TmAJfbOIqWkhPO"    # HighERV       SA/EA 8/8
  quiet_negative_pressure_scene_id: "UU21bwc3Y0MgxwuJ"   # LowERVNP      SA/EA 1/2
  medium_negative_pressure_scene_id: "pQApieM2dUo6PR5r"  # MidERVNegPres SA/EA 2/3
  turbo_negative_pressure_scene_id: "qaeDnnwLNnaQXT54"   # HiERVNP       SA/EA 7/8

  control_mode: "scene"                # scene | local -- which transport issues WRITES
  local_readback_enabled: true         # local Tuya read-after-write for speed truth (read-only)
  local_write_fallback_enabled: false  # opt-in: local write when a scene trigger fails
```

**These are the intended defaults, not placeholders.** `control_mode: "scene"` and `local_write_fallback_enabled: false` together mean the server issues **zero local Tuya commands** in normal operation — only local reads. That is the point of the design, so `ErvConfig::default()` in `config.rs:315-330` should encode it too, not just `config.yaml`.

`smart_life_auth_file` should default to `~/.office-automate/tuya-sharing-auth.json`, matching `blinds.rs:128-135`.

### Open question: internet-outage behaviour

With `local_write_fallback_enabled: false`, an internet or Tuya-cloud outage means **no ERV control at all**, even though the unit is sitting on the same LAN and reachable. Local writes are the only thing that would keep it controllable, but enabling them reintroduces exactly the local command traffic this design removes.

Ship the flag defaulting to `false` as specified. Flag the trade-off to the owner rather than deciding it in code; it is a genuine choice between "never risk the lockout" and "stay controllable when the WAN drops".

### Negative-pressure coverage — complete

`speed_preset` (`erv.rs:66-76`) defines negative-pressure variants for all three speeds — Quiet `(1,2)`, Medium `(2,3)`, Turbo `(7,8)`. **All three now have scenes** (built by hand 2026-08-03), so the scene path covers the full `(speed × pressure)` matrix with no gaps:

| Speed | Normal | SA/EA | Negative pressure | SA/EA |
|---|---|---|---|---|
| Off | `ERVOff` | — | — | — |
| Quiet | `LowERV` | 1/1 | `LowERVNP` | 1/2 |
| Medium | `MidERV` | 3/2 | `MidERVNegPres` | 2/3 |
| Turbo | `HighERV` | 8/8 | `HiERVNP` | 7/8 |

Every state the policy can request is reachable over the scene path. Negative pressure is dormant right now — `post_renovation_expires_at` is `2026-07-11`, already past, so `post_renovation_negative_pressure_active()` (`automation.rs:261-265`) returns false — but re-arming renovation mode now needs only a date change, no new scenes.

Still implement the missing-scene case defensively: if a `(speed, pressure)` pair ever resolves to no configured scene id, **fail loudly and surface it in status** rather than silently substituting the normal-pressure variant. A system that reports a mode it is not delivering is worse than one that reports an error.

(Scene naming is inconsistent — `MidERVNegPres` vs `LowERVNP`/`HiERVNP`. Cosmetic; the ids are what bind.)

## Implementation Steps

1. **Extract `smart_life.rs`** from `blinds.rs:161-441` — `SmartLifeAuthCache`, `SmartLifeTokenInfo`, `SmartLifeClient`, and the AES-GCM/HMAC helpers are all device-agnostic. `blinds.rs` keeps only `trigger_smart_life_scene`. Pure refactor; `blinds.rs:443-499` tests cover it.

2. **Add an auth-cache mutex.** `refresh_auth_if_needed` (`blinds.rs:213-248`) *rotates the refresh token* on every refresh. Once blinds and ERV share one cache file, two concurrent refreshes can clobber each other and lose cloud auth for both devices. This bug does not exist today only because blinds is the sole caller. Guard load/refresh/save with a process-wide async mutex.

3. **Extend `ErvConfig`** (`config.rs:288-330`) with the scene fields and mode flags; add `scene_configured()`. Add matching env overrides if following the pattern at `config.rs:720-726`.

4. **Add `SceneErvSpeedWriter`** and `SplitErvWriter` (scene writes + local reads).

5. **Wire writer selection** at `http.rs:303` and `http.rs:554` from `control_mode` / `local_readback_enabled` / `local_write_fallback_enabled`. Default assembly is `SplitErvWriter { writer: SceneErvSpeedWriter, reader: RustuyaErvStatusReader }`.

6. **Remove the polling loop** (`erv.rs:1059`, `http.rs:592`); add the boot read.

7. **Make the gates mode-aware** — `erv.rs:273-280`, `erv.rs:327-335`, `erv.rs:451-456`, and the pre-write smoke read at `erv.rs:295-298`.

8. **Fix the read-only smoke paths.** `smoke_erv` (`erv.rs:1081`) is called from `cli.rs:618` and `validation.rs:1475`; `validation.rs:1472-1474` hard-bails when local ERV creds are absent, which breaks shadow validation in scene-only mode.

9. **Update status/notifications** — `status.rs:207-216`, `erv.rs:1293-1319`.

10. **Docs** — mark `docs/tuya-local-key.md` as the recovery path for restoring *speed readback* rather than restoring control; note that `scripts/refresh-erv-key.py` cannot fix a both-copies-stale desync and that Path B (re-pair) is required, as happened on 2026-04-30 and again on 2026-08-03.

## Testing

- Existing `FakeErvWriter` suites in `automation.rs:427+`, `http.rs:2939+`, `yolink.rs:921+` must keep passing unchanged — if they don't, the trait was altered and shouldn't have been.
- New: in the default config, a `set_speed` issues a scene trigger and **no local command** — assert the local writer is never invoked. This is the property the whole design rests on; test it directly.
- New: scene write succeeds + local read succeeds → status source is `Local` with real SA/EA.
- New: scene write succeeds + local read fails → write still succeeds, status source is `Assumed`, control is not blocked.
- New: `local_key_invalid` does not block a scene write.
- New: scene trigger failure surfaces an error (and, with `local_write_fallback_enabled: true`, retries locally).
- New: negative-pressure request with no matching scene degrades loudly, not silently.
- New: auth-cache refresh under concurrent blinds + ERV access does not lose the refresh token.
- Manual: trigger each of the four speeds and confirm SA/EA on the unit's display, since no automated check can observe speed through the cloud.

## Out of Scope

- **Settings UI.** There is no settings surface in the app today (`frontend/components/` is `CO2Chart.tsx` and `VitalTile.tsx` only); a runtime toggle needs new UI, an endpoint, and mutable-config persistence. Config-file driven for now; separate ticket if it's wanted.
- Reviving the Tuya IoT Cloud subscription.
- Using the ERV's own `tvoc`/`humidity_indoor`/`temp_indoor` readings — Qingping already covers this.

## Appendix: Reproducing the Diagnostics

All probes ran through the existing sharing-API auth cache, non-interactively — no QR re-authorization needed (that is only `refresh-erv-key.py --init-auth`).

```python
# Manager built from ~/.office-automate/tuya-sharing-auth.json, as in
# scripts/refresh-erv-key.py:386-400
api = manager.customer_api
api.get("/v1.0/m/life/ha/devices/detail", {"devIds": DEVICE_ID})   # status, local_key
api.get("/v1.0/m/scene/ha/home/scenes", {"homeId": HOME_ID})       # scene list (lossy actions)
manager.query_scenes(); manager.update_device_cache()
```

```python
import tinytuya                       # local, the only speed-capable path
d = tinytuya.Device(DEVICE_ID, IP, LOCAL_KEY, version=3.4)
d.status()                            # -> dps incl. "101"/"102"
tinytuya.deviceScan(False, 8)         # device advertises its own protocol version
```

Err 914 means "key **or version**" — check both. The device broadcasts its true version, so compare against that before concluding the key is stale.
