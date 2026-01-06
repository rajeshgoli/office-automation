# Office Climate Automation - North Star Spec

## Vision
Transform your backyard shed office into an intelligently climate-controlled space that optimizes air quality silently while you work, and aggressively refreshes air the moment you step away.

---

## Current State

### Hardware Inventory
| Device | Function | Connectivity |
|--------|----------|--------------|
| Mitsubishi Split AC | Heating/Cooling | Kumo Cloud / Comfort app |
| Airlink ERV | Ventilation (noisy) | Airlink app |
| Qingping Air Monitor | CO2, temp, humidity sensing | Qingping+ app |
| YoLink Door Sensor | Sliding door state | YoLink app |
| YoLink Window Sensor | Window state | YoLink app |
| YoLink Motion Sensor | Occupancy detection | YoLink app |
| Mac + External Monitor | High-confidence occupancy signal | macOS |

### Current Pain Points
- Manual control across 4 separate apps
- ERV noise disrupts focus when running
- No coordination between devices
- Air quality degrades during long focus sessions

---

## Occupancy Detection

### Philosophy
- **False positive ventilation is acceptable** — worst case is unnecessary air refresh
- **False negative (noise while present) is not** — disrupts focus
- **Trigger ventilation eagerly, stop immediately on presence**

### Occupancy Signals

| Signal | Indicates | Confidence | Latency |
|--------|-----------|------------|---------|
| Mac active + external monitor connected | Present | Very High | Real-time |
| Motion sensor active | Present | High | Real-time |
| Door open → close sequence | Likely departed | Medium | Immediate |
| Door open (sustained) | Uncertain | Low | — |
| No motion for N seconds | Likely away | Medium | Delayed |

### State Machine
```
PRESENT (quiet mode):
    Entry: Any presence signal
    Exit: Departure signal + no presence signals
    
AWAY (ventilation mode):
    Entry: Departure detected, no contradicting presence signals
    Exit: ANY presence signal (immediate)
```

### Presence Detection Logic
```
mac_presence = (
    external_monitor_connected AND
    mac_last_active > door_last_changed
)

motion_presence = (
    motion_recent AND
    door_closed AND
    motion_last_seen > door_last_changed
)

is_present = mac_presence OR motion_presence
```

**Key insight:** Door event timestamp is the source of truth. Only activity AFTER the last door event counts as presence, automatically filtering out pre-departure activity.

### Departure Detection Logic
```
departure_triggered = (
    door_opened_then_closed
    OR motion_timeout_exceeded
)

is_away = departure_triggered AND NOT is_present
```

### Transition Rules
```
TO AWAY:
    Trigger on FIRST reliable departure signal:
        - Door open → close sequence (immediate)
        - Motion sensor timeout (configurable, e.g., 60 seconds)
    Confirm: Mac not active on external monitor
    
TO PRESENT:
    Trigger on ANY presence signal (immediate, no delay):
        - Motion detected
        - Mac becomes active on external monitor
        - Door opens (conservative: assume returning)
```

---

## Core Automation Rules

### Rule 1: Present Mode — Quiet Priority
```
IF is_present:
    IF CO2 >= 2000 ppm:
        → ERV ON (necessary evil - air quality critical)
    ELSE IF ERV is running AND CO2 < 1800 ppm:
        → ERV OFF (hysteresis: 200 ppm band prevents cycling)
    ELSE:
        → Maintain current state (in hysteresis band 1800-2000 ppm)
```

### Rule 2: Away Mode — Aggressive Refresh
```
IF is_away:
    IF CO2 > 500 ppm:
        → ERV FULL BLAST
    ELSE:
        → ERV OFF (target achieved)
        
ON first presence signal:
    → ERV OFF (immediate, no delay)
```

### Rule 3: HVAC Control
```
IF window OPEN OR door OPEN:
    → AC/Heat OFF
    → ERV OFF
    
IF window CLOSED AND door CLOSED:
    → Use Qingping temp as authoritative reading
    → Override Mitsubishi internal sensor
    → Maintain target temp via Mitsubishi
```

### Rule 4: Safety Interlocks
```
ALWAYS:
    → Window open OR door open = all climate systems OFF
    → Qingping readings override device internal sensors
    → Any presence signal = immediate ERV shutoff
```

---

## State Diagram
```
                    ┌─────────────────────────────────┐
                    │                                 │
                    ▼                                 │
┌─────────────────────────────────┐                  │
│           PRESENT               │                  │
│  ─────────────────────────────  │                  │
│  ERV: OFF (unless CO2 > 2000)   │                  │
│  HVAC: Active per temp target   │                  │
│  Priority: QUIET                │                  │
└─────────────────────────────────┘                  │
        │                                            │
        │ door_close + 10s verification              │
        │ (no activity after door close)             │
        ▼                                            │
┌─────────────────────────────────┐                  │
│            AWAY                 │                  │
│  ─────────────────────────────  │                  │
│  ERV: FULL if CO2 > 500         │──────────────────┘
│  HVAC: Maintain or setback      │   ANY presence signal
│  Priority: AIR QUALITY          │   (activity after door event)
└─────────────────────────────────┘
```

---

## Priority Hierarchy

1. **P0**: Safety interlocks (open window/door → systems off)
2. **P0**: Immediate ERV shutoff on any presence signal
3. **P1**: Quiet while present (ERV only if CO2 critical >2000)
4. **P1**: Air quality while away (aggressive refresh >500)
5. **P2**: Temperature comfort (Qingping-driven HVAC)
6. **P3**: Predictive pre-conditioning (learn patterns)

---

## Configuration Parameters

| Parameter | Default | Notes |
|-----------|---------|-------|
| `motion_timeout_seconds` | 60 | Time before motion absence triggers away |
| `co2_critical_threshold` | 2000 ppm | ERV activates even when present (turn ON) |
| `co2_critical_hysteresis` | 200 ppm | Turn OFF at 1800 ppm (prevents cycling) |
| `co2_refresh_target` | 500 ppm | Target for away-mode ventilation |
| `mac_poll_interval` | 5 seconds | How often to check mac/monitor state |
| `erv_shutoff_delay` | 0 seconds | Immediate on presence detection |

---

## Edge Cases

| Scenario | Handling |
|----------|----------|
| Mac active on monitor, but user in house | Treated as present (acceptable — quiet mode is safe default) |
| Quick step outside (<60 sec) | Door sequence triggers away, motion on return triggers present |
| Door left open | HVAC and ERV disabled (safety interlock) |
| Window cracked for fresh air | All climate systems disabled |
| Power outage / sensor offline | Fail safe — manual control, no automation |
| Motion sensor false negative | Mac+monitor serves as backup presence signal |

---

## Technical Investigation Needed

### API/Integration Research
- [ ] **YoLink**: Hub required? IFTTT/Home Assistant integration?
- [ ] **Qingping**: API availability? HomeKit bridge? IFTTT?
- [ ] **Airlink ERV**: Any API? Smart plug as fallback control?
- [ ] **Mitsubishi Kumo**: Local API? Home Assistant integration?
- [ ] **macOS**: Script to detect external monitor + activity state

### Platform Options
| Option | Pros | Cons |
|--------|------|------|
| Home Assistant | Full local control, all devices likely supported | Setup complexity, needs always-on hub |
| IFTTT | Simple, no hardware | Latency, limited logic, subscription |
| Homebridge | Apple ecosystem integration | Limited automation logic |
| Custom (Node-RED) | Maximum flexibility | Development effort |

---

## Success Metrics

| Metric | Target |
|--------|--------|
| Occupied CO2 | Never exceeds 2000 ppm |
| Return-to-office CO2 | <800 ppm (pre-cleared by away ventilation) |
| ERV runtime while present | <5 min/day average |
| Latency: departure → ERV on | <30 seconds |
| Latency: presence → ERV off | <5 seconds |
| Manual interventions | Zero |

---

## P2: Future — Predictive Conditioning

- Learn arrival patterns (e.g., 8:45 AM weekdays)
- Pre-ventilate 15 min before typical arrival
- Pre-condition temperature based on outdoor forecast + calendar
- Adjust thresholds seasonally (humidity consideration)
- Integrate calendar for meeting awareness (pre-fresh before video calls)

---

## Open Questions

1. What's the optimal `motion_timeout_seconds`? Start with 60, tune based on false positive rate.
2. Should door-open (without close) trigger away? Currently conservative — treated as uncertain.
3. HVAC setback while away? Could save energy but adds complexity.
4. Should ERV have graduated speeds or just off/full? Spec assumes binary for simplicity.
