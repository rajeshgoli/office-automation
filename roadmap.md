# Office Climate Automation - Roadmap

A living document tracking progress and next steps.

---

## Completed

### Phase 0: Foundation
- [x] **Vision document** - Core requirements and state machine defined
- [x] **macOS occupancy detector** - External monitor + idle detection (`occupancy_detector.py`)
  - Detects external monitors via system_profiler
  - Tracks idle time via IOKit
  - Watch mode with state change events
  - JSON output for integration

---

## In Progress

### Qingping Data Collection ✅ Complete
- Device bound to local MQTT broker (Mosquitto on Mac)
- Upload interval: **Configurable** (default 60s, set in `config.yaml`)
- Configured automatically on startup via MQTT `qingping/{MAC}/down` topic
- API endpoint: POST `/qingping/interval` for runtime changes

---

## API Research Findings

### YoLink (Door, Window, Motion Sensors) ✅ Researched

**Hub Requirements:**
- **Cloud API**: No hub required, works with any YoLink setup
- **Local API**: Requires "Local Hub" (YS1603-UC or YS1604-UC SpeakerHub)

**API Options:**
| Method | Endpoint | Auth | Real-time |
|--------|----------|------|-----------|
| Cloud API | api.yosmart.com | UAC token | MQTT port 8003 |
| Local API | http://{hub-ip}:1080 | Client ID/Secret | MQTT port 18080 |
| Home Assistant | Built-in integration | OAuth via Nabu Casa | Cloud Push |

**Getting Credentials:**
- UAC: YoLink App → Account → Advanced Settings → Personal Access Credentials → [+]
- Local: YoLink App → Local Hub → Local Network → Integrations → Local API

**Sensor Behavior:**
- Door/Window: Binary open/closed, event sent immediately on change
- Motion: Binary motion/no-motion, event sent immediately on detection
- All sensors: Class A devices - event-driven + heartbeat every 4 hours

**Protocol:** HTTP JSON-RPC (not REST) + MQTT for events

**Recommendation:** Use Local API (hub confirmed available) - faster, no cloud dependency.

**Your Setup:** YS1603-UC (basic hub) → Using Cloud API instead of local

**Sources:**
- [YoSmart API Docs](https://doc.yosmart.com/docs/overall/intro/)
- [Local API Setup](https://doc.yosmart.com/docs/overall/qsg_local/)
- [Home Assistant Integration](https://www.home-assistant.io/integrations/yolink/)
- [yolink-api GitHub](https://github.com/YoSmart-Inc/yolink-api)

### Qingping (CO2, Temp, Humidity) ✅ Researched

**Integration Options:**
| Method | Setup | Real-time | Cloud Dependency |
|--------|-------|-----------|------------------|
| Cloud API | developer.qingping.co OAuth | Polling (15min default) | Yes |
| Local MQTT (Official) | Dev console → Private Access Config | Configurable interval | Account needed for setup |
| Local MQTT (ADB Hack) | Modify /data/etc/settings.ini | Configurable | No |
| HomeKit Mode | Qingping+ app (Lite model) | Push | Apple ecosystem |

**Sensors Available:**
- CO2 (ppm) ← **critical for automation**
- Temperature, Humidity
- PM2.5, PM10
- TVOC (some models)

**Local MQTT Setup (Recommended):**
1. Create account in Qingping+ app
2. Go to developer.qingping.co → create "Private Access Config"
3. Assign config to device, set MQTT broker to local (e.g., Mosquitto)
4. Reset device to apply
5. Data arrives on topic `qingping/{MAC}/up` as JSON

**Default reporting interval:** 15 minutes (adjustable via API)

**Home Assistant:** HACS integration available ([mash2k3/qingping_cgs1](https://github.com/mash2k3/qingping_cgs1))

**Sources:**
- [Robert Ying's Guide](https://robertying.com/post/qingping-cgs1-home-assistant/)
- [HACS Integration](https://github.com/mash2k3/qingping_cgs1)
- [Hackaday - Local MQTT Mod](https://hackaday.com/2026/01/04/modifying-a-qingping-air-quality-monitor-for-local-mqtt-access/)
- [Qingping Developer Portal](https://developer.qingping.co/)

### Mitsubishi Kumo (HVAC) ✅ Researched

**⚠️ Current Status (2025):** Mitsubishi's March 2025 "Comfort" app rollout broke the cloud API used by pykumo to retrieve local credentials. Existing setups still work, but new setups or changes (IP, password) may fail.

**Control Options:**
| Method | Status | Pros | Cons |
|--------|--------|------|------|
| pykumo + hass-kumo | ⚠️ Fragile | Local control once configured | Needs cloud for initial setup, broken for new users |
| ESPHome (CN105) | ✅ Recommended | Fully local, no cloud ever | Requires ESP32 hardware (~$30-50) |
| New Cloud API | Works | Easy setup | Cloud dependent |

**ESPHome Route (Recommended):**
1. Purchase ESP32 board with CN105 connector (Etsy, ~$30-50)
2. Unplug Kumo Cloud adapter from indoor unit
3. Plug in ESPHome adapter (same CN105 port)
4. Flash with ESPHome config from [SwiCago/HeatPump](https://github.com/SwiCago/HeatPump)
5. Full climate entity + sensors in Home Assistant (filter status, outdoor temp, compressor freq)

**If Already Using Kumo Cloud:**
- DON'T change IP assignments or reset adapters
- DON'T change Mitsubishi password
- Stay out of the new Comfort app
- Existing pykumo/hass-kumo may continue working

**pykumo Local API:** Once credentials are obtained, communicates directly with indoor unit via WiFi adapter. No cloud needed for ongoing operation.

**Sources:**
- [pykumo GitHub](https://github.com/dlarrick/pykumo)
- [hass-kumo GitHub](https://github.com/dlarrick/hass-kumo)
- [ESPHome Heat Pump Guide](https://corb.co/posts/heatpump-esphome/)
- [Community Thread (500+ posts)](https://community.home-assistant.io/t/mitsubishi-kumo-cloud-integration/121508)

### Pioneer Airlink ERV (Ventilation) ✅ Researched

**Key Discovery:** Pioneer Airlink is built on **Tuya platform** under the hood.

**Control Options:**
| Method | Pros | Cons |
|--------|------|------|
| LocalTuya (HACS) | Local, no cloud | Requires device ID + local key setup |
| Tuya Cloud Integration | Easy setup | Cloud dependent, limited ventilation entity support |
| Shelly Relay | Simple, reliable, local | Requires wiring into control wires |
| Smart Plug | Simplest | ⚠️ NOT recommended - dampers stay open, freezing risk |

**LocalTuya Setup (Recommended for local control):**
1. Pair ERV with Tuya Smart Life app (instead of Pioneer Airlink app)
2. Create Tuya Developer account at iot.tuya.com
3. Get Device ID and Local Key from developer console
4. Install LocalTuya via HACS
5. Configure with device ID, local key, and data points (DPs)

**Relay Fallback (If Tuya doesn't work):**
- Wire Shelly relay (~$12) to control wires (typically 12V DC)
- Provides simple on/off control
- Reliable, fully local

**⚠️ Warning:** Don't use smart plug to cut power - ERV dampers stay open when unpowered, risking freezing.

**Sources:**
- [Pioneer Airlink App](https://apps.apple.com/us/app/pioneer-airlink/id1576310310)
- [LocalTuya Integration](https://community.home-assistant.io/t/local-tuya-fan-controller/306985)
- [Broan ERV Relay Integration](https://rowan.computer/blog/integrating-a-broan-ventilator-into-home-assistant/)

---

## Up Next

### Phase 1: Device Integration Research
Priority: Understand what APIs are available before choosing architecture.

- [x] **YoLink API** - Door, window, motion sensors ✅
- [x] **Qingping API** - CO2, temp, humidity ✅
- [x] **Airlink ERV API** - Ventilation control ✅
- [x] **Mitsubishi Kumo API** - HVAC control ✅

### Phase 2: Platform Decision ✅ Decided

**Decision: Custom Python**

Reasons:
- Full control over logic
- Understand every piece
- Lightweight, no external system to learn
- Can run directly on Mac

**Architecture:**
```
┌─────────────────────────────────────────────────────────┐
│                    Main Orchestrator                     │
│            (state machine + automation rules)            │
└─────────────────────────────────────────────────────────┘
        │              │              │              │
        ▼              ▼              ▼              ▼
┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
│   YoLink    │ │  Qingping   │ │   Airlink   │ │ Mitsubishi  │
│   Client    │ │   Client    │ │ ERV Client  │ │   Client    │
│ (HTTP+MQTT) │ │   (MQTT)    │ │(Tuya/Relay) │ │  (pykumo)   │
└─────────────┘ └─────────────┘ └─────────────┘ └─────────────┘
        │              │              │              │
        ▼              ▼              ▼              ▼
   Door/Window      CO2/Temp        ERV On/Off      HVAC Control
   Motion Sensor    Humidity        Fan Speed       Temp Override
```

**Shared Infrastructure:**
- MQTT broker (Mosquitto) for Qingping + YoLink local
- Async event loop (asyncio)
- Config file for thresholds, IPs, credentials

**Deployment:**
- Develop & test on Mac
- Deploy to Raspberry Pi (or similar always-on device) for production
- macOS occupancy detector stays on Mac, sends state to Pi over network

### Phase 3: Core State Machine ✅ Complete
Build the PRESENT/AWAY state machine with all inputs.

- [x] Unified state manager combining:
  - macOS occupancy (done)
  - YoLink motion sensor
  - YoLink door sensor
  - YoLink window sensor
- [x] Transition logic per vision.md
- [x] Event bus for state changes

### Phase 4: Automation Rules ✅ Complete
Implement the core rules from vision.md.

- [x] **Rule 1**: Present mode - quiet priority (ERV off unless CO2 > 2000)
- [x] **Rule 2**: Away mode - aggressive refresh (ERV on until CO2 < 500)
- [x] **Rule 3**: HVAC coordination - suspend heating when ERV venting (GH #2)
- [x] **Rule 4**: Safety interlocks (window/door open = systems off)
- [x] **Rule 5**: tVOC ventilation - ERV MEDIUM (3/2) when tVOC > 250 ppb (GH #1)

### Phase 5: Mobile & Remote Access ✅ Complete
- [x] PWA manifest and iOS meta tags
- [x] App icons generated (192x192, 512x512)
- [x] Backend serves frontend (single port deployment)
- [x] HTTP Basic Auth implemented (optional)
- [x] Tested PWA installation on iOS
- [x] Remote access solution selected (Tailscale Funnel)
- [ ] Deploy to Mac Mini / always-on device (ready to go, just waiting on hardware)

### Phase 6: Deployment
- [ ] Set up Mac Mini (or Pi) as always-on server
- [ ] Install dependencies (Python, Mosquitto, Tailscale)
- [ ] Deploy orchestrator to Mac Mini
- [ ] Run as launch agent (macOS) or systemd service (Linux)
- [ ] Enable Tailscale Funnel for remote access
- [ ] Add DNS CNAME for climate.rajeshgo.li
- [ ] Update Mac occupancy detector to POST to Mac Mini
- [ ] Test full end-to-end flow from anywhere
- [ ] Enable authentication (HTTP Basic Auth or Google OAuth)

---

## Future / P2

- [ ] **Historical Charts** - Time-series visualization for trends
  - CO2, temperature, humidity graphs (day/week/month views)
  - Occupancy pattern analysis (when are you typically in the office?)
  - ERV runtime statistics (total hours, mode distribution)
  - HVAC energy usage estimates
  - Export data as CSV for external analysis
- [ ] Predictive pre-conditioning based on arrival patterns
- [ ] Calendar integration for meeting awareness
- [ ] Seasonal threshold adjustments
- [x] Dashboard / visualization ✅ (Session 3)
- [ ] **Kumo Cloud WebSocket/Push Notifications** - Replace polling with real-time updates
  - Current: Polling every 5 minutes (wasteful, delayed updates)
  - Better: Investigate if Kumo Cloud API supports WebSockets or push notifications
  - Would eliminate API rate limit concerns and provide instant dashboard sync
  - Alternative: If Kumo doesn't support it, consider ESPHome route (fully local)

---

## Open Questions

1. ~~Is a hub required for YoLink sensors?~~ → **Answered**: No for cloud API, yes for local API
2. Can Airlink ERV be controlled via API or only smart plug?
3. Home Assistant vs custom Python - which fits better?
4. Where should the automation run? (Mac, Raspberry Pi, NAS?)
5. ~~Do you have a YoLink hub, or just the sensors?~~ → **Answered**: Yes, hub available - use Local API

---

## Session Log

**2025-01-05** (Session 1)
- Created vision.md with full spec
- Built macOS occupancy detector (occupancy_detector.py)
- Created roadmap.md
- Researched YoLink API: Cloud API works without hub, supports MQTT for real-time events
- Researched Qingping API: Local MQTT available, 15min default interval (adjustable)
- Researched Airlink ERV: Tuya-based, use LocalTuya or Shelly relay
- Researched Mitsubishi Kumo: Cloud API broken in 2025, ESPHome recommended for new setups
- **Phase 1 Complete** - All device APIs researched
- **Phase 2 Decision** - Custom Python (not Home Assistant)
- **YoLink client tested** - MQTT events working (door open/close confirmed)
- **State machine built** - PRESENT/AWAY transitions working with motion + door sensors

**2026-01-05** (Session 2)
- **ERV Client Complete** - Tuya local control working
  - Obtained local_key via tinytuya Cloud API (linked Smart Life to Tuya IoT project)
  - DPS mappings discovered: DP 1 (power), DP 101 (SA fan), DP 102 (EA fan)
  - Fan speeds: OFF, QUIET (1/1), MEDIUM (3/2), TURBO (8/8)
  - Device: `ebfb18b2fc8f6dc63eqvcw` @ `192.168.5.119`
- **Qingping Local MQTT Setup**
  - Logged into developer.qingping.co
  - Created "Private Access Config" → Self-built MQTT
  - Bound device MAC `582D3470765F` to config
  - Mosquitto broker installed on Mac (`192.168.5.47:1883`)
  - Upload interval: 15 minutes
  - Topic: `qingping/582D3470765F/up`
  - Created `QingpingMQTTClient` class to receive and parse sensor data
- **Mitsubishi Kumo Client** - Created but not wired into orchestrator
  - Uses pykumo library
  - Credentials in config.yaml
- **Orchestrator Wired Up**
  - Connects to: ERV (Tuya), Qingping (MQTT), YoLink (Cloud MQTT)
  - State machine controls ERV based on occupancy + CO2
  - HTTP endpoint for Mac occupancy updates (POST /occupancy)
  - Status endpoint (GET /status) shows all sensor data
- **Phase 3 & 4 Complete** - Core automation logic working
- **Waiting on**: First Qingping sensor data via MQTT (up to 15 min)

**2026-01-05** (Session 3)
- **Dashboard Wired to Live Data**
  - Moved template from `tmp/office-climate-dashboard/` to `frontend/`
  - Created `api.ts` with fetchStatus(), toFahrenheit(), StatusWebSocket class
  - Updated `App.tsx` to use live API instead of mock data
  - Temperature converted from Celsius to Fahrenheit
  - Events logged for state changes (arrival/away, ERV on/off, door/window)
  - CO2 history tracked in-memory (last 96 points)
  - Connection status indicators in footer (API, WebSocket, Qingping, YoLink)
- **WebSocket Support Added to Backend**
  - Added `/ws` endpoint to orchestrator
  - Broadcasts status on: YoLink events, Qingping readings, state changes, occupancy updates
  - CORS middleware for frontend access
- **Files Changed**:
  - `frontend/api.ts` - New API service
  - `frontend/App.tsx` - Wired to live data
  - `src/orchestrator.py` - WebSocket endpoint + CORS
- **To Run**:
  ```bash
  # Terminal 1: Backend
  cd /Users/rajesh/Desktop/office-automate
  source venv/bin/activate
  python run.py

  # Terminal 2: Frontend
  cd frontend
  npm run dev
  ```
- **Remaining Work**:
  - Wire Mitsubishi Kumo client (HVAC control)
  - Persist CO2 history to file for restart recovery
  - Add events endpoint (`GET /events`) for historical event log

**2026-01-05** (Session 4)
- **State Machine Bug Fixed**
  - PRESENT→AWAY now requires door open→close event
  - Motion timeout alone no longer triggers departure
  - "Can't leave without using the door"
- **Qingping MQTT Parser Fixed**
  - Supports local format (`sensorData` array, camelCase)
  - Was expecting cloud format (`type: 17`, `sensor_data` object)
  - Now receiving live CO2, temp, humidity data
- **SQLite Database Added** (`src/database.py`)
  - `sensor_readings` - CO2, temp, humidity, tVOC, PM2.5 history
  - `occupancy_log` - State changes with timestamps
  - `device_events` - Door, window, motion events
  - `climate_actions` - ERV/HVAC control actions
  - Restores last Qingping reading on startup (survives restarts)
  - Analysis helpers: `get_daily_co2_stats()`, `get_occupancy_patterns()`
- **Dashboard Fixes**
  - HVAC status from API (was hardcoded COOL 70°, now shows HEAT 72°)
  - Fixed spurious "Arrived" events on page load/reconnect
  - Added tVOC tile
  - Fixed humidity decimal precision (1 decimal place)
- **GitHub Issue #1 Filed**
  - tVOC > 250 ppb should trigger ERV at 3/2 speed
  - For clearing volatile chemicals (food odors)
- **Files Changed**:
  - `src/state_machine.py` - Door event required for departure
  - `src/qingping_client.py` - Local MQTT format support
  - `src/database.py` - New SQLite persistence layer
  - `src/orchestrator.py` - Database integration, HVAC status in API
  - `frontend/App.tsx` - HVAC from API, event deduplication, tVOC tile
  - `frontend/api.ts` - HVAC and tVOC types
  - `frontend/types.ts` - tVOC field
  - `.gitignore` - Ignore data/ directory

**2026-01-05** (Session 5)
- **GitHub Issue #1 Implemented** - tVOC-based ventilation control
  - Added `tvoc_threshold_ppb` config parameter (default: 250 ppb)
  - tVOC > threshold triggers ERV at MEDIUM (3/2) speed
  - Works in both PRESENT and AWAY states
  - PRESENT: tVOC triggers MEDIUM (louder than QUIET for CO2 critical)
  - AWAY: TURBO takes precedence for CO2 refresh, MEDIUM for tVOC-only
  - Logs tVOC-triggered actions to `climate_actions` table
  - ERV status API now includes `tvoc_ventilation` boolean
- **Files Changed**:
  - `src/config.py` - Added `tvoc_threshold_ppb` to ThresholdsConfig
  - `src/orchestrator.py` - tVOC evaluation logic in `_evaluate_erv_state()`
- **GitHub Issue #2 Implemented** - HVAC coordination with ERV
  - Wired Kumo client into orchestrator
  - Added HVAC config: `hvac_min_temp_f` (68), `hvac_critical_temp_f` (55), occupancy hours (7AM-10PM)
  - When AWAY + ERV running + temp > 68°F: suspend HVAC (don't heat vented air)
  - Resume HVAC when: return to PRESENT, or ERV stops within occupancy hours
  - Critical temp protection: always heat if < 55°F
  - Logs all HVAC actions to `climate_actions` table
  - Status API includes `hvac.suspended` boolean
- **Frontend Fix** - API now uses `window.location.hostname` for mobile access
- **Files Changed**:
  - `src/config.py` - Added HVAC threshold configs
  - `src/orchestrator.py` - Kumo client integration, `_evaluate_hvac_state()` method
  - `frontend/api.ts` - Dynamic hostname for mobile access

**2026-01-06** (Session 6)
- **GitHub Issue #5 Implemented** - Dashboard Quick Controls
  - POST `/erv` endpoint: `{"speed": "off|quiet|medium|turbo"}`
  - POST `/hvac` endpoint: `{"mode": "off|heat", "setpoint_f": 70}`
  - Manual override tracking with 30-minute auto-expiry
  - Dashboard "Quick Controls" section with ERV (4 buttons) + HVAC (2 buttons)
  - Visual feedback: amber highlight for active override, countdown timer
  - All manual actions logged to `climate_actions` table
- **ERV Speed Display** - Shows actual speed instead of just ON/OFF
  - Dashboard shows: OFF, QUIET, ELEVATED, PURGE
  - Maps to: off, 1/1, 3/2, 8/8 fan speeds
  - Backend tracks `_erv_speed` during automation
- **Presence Detection Overhaul** - Fixed false PRESENT issues
  - Problem: Mac "active" (monitor connected) kept triggering PRESENT after leaving
  - Solution: Door is now source of truth for departure
  - **AWAY → PRESENT triggers:**
    - Mac keyboard/mouse activity (strongest signal)
    - Motion detected while door is closed
  - **NOT triggers:**
    - Door opening alone (might just grab something)
    - External monitor connected (just means Mac is there)
  - **PRESENT → AWAY:** Door closes + 10s no activity
  - On departure verification: reset mac_active, motion_last_seen
- **Files Changed**:
  - `src/orchestrator.py` - Manual override endpoints, ERV speed tracking
  - `src/state_machine.py` - Presence detection logic overhaul
  - `frontend/App.tsx` - Quick controls UI, ERV speed display
  - `frontend/api.ts` - ERV/HVAC control functions, manual_override types
  - `frontend/types.ts` - ERVSpeed enum (OFF, QUIET, ELEVATED, PURGE)
  - `frontend/components/VitalTile.tsx` - Support ERVSpeed status

**2026-01-06** (Session 7)
- **GitHub Issue #6 Implemented** - Reduce Qingping polling interval
  - Research: Qingping devices accept config via MQTT `qingping/{MAC}/down` topic
  - Added `configure_interval()` method to `QingpingMQTTClient`
  - Config format: `{"type": "17", "setting": {"report_interval": N, ...}}`
  - New config option: `qingping.report_interval` (default: 60s)
  - Auto-configures device on startup
  - POST `/qingping/interval` endpoint for runtime changes
  - Status API shows `interval_configured` and `report_interval`
- **Note**: Device may need reset to apply new interval
- **Files Changed**:
  - `src/qingping_client.py` - `configure_interval()`, `report_interval` param
  - `src/config.py` - Added `report_interval` to QingpingConfig
  - `src/orchestrator.py` - Interval config on startup, `/qingping/interval` endpoint
  - `config.yaml` - Added `report_interval: 60`
  - `CLAUDE.md` - Updated API docs and Qingping section

**2026-01-06** (Session 8)
- **Fixed tVOC Reading** - Qingping sends `tvoc_index` not `tvoc`
  - Sensirion SGP40 VOC index (0-500 scale), not ppb
  - Updated parser to check `tvoc_index` first, fallback to `tvoc`
  - Dashboard now shows actual tVOC values
- **Added Noise Level** - New sensor reading from Qingping
  - `noise_db` field added to `QingpingReading` dataclass
  - Exposed in API at `air_quality.noise_db`
  - New "Noise" tile on dashboard with 🔊 icon
- **Report Interval** - Changed default from 60s to 30s
  - More responsive dashboard updates
  - No harm on USB-powered device
- **Dashboard Layout** - Changed vitals grid from 3 to 4 columns on desktop
- **Files Changed**:
  - `src/qingping_client.py` - Fixed `tvoc_index` field, added `noise_db`
  - `src/orchestrator.py` - Added `noise_db` to API
  - `frontend/api.ts` - Added `noise_db` type
  - `frontend/types.ts` - Added `noise` to OfficeState
  - `frontend/App.tsx` - Added noise tile, 4-column grid
  - `config.yaml` - `report_interval: 30`
  - `CLAUDE.md` - Updated Qingping sensor docs

**2026-01-06** (Session 9)
- **HVAC Status Polling** - Fixed dashboard showing incorrect HVAC state
  - Problem: Dashboard showed "off" when heater was actually running
  - Root cause: Orchestrator only checked HVAC status once at startup
  - Solution: Added background task to poll Kumo Cloud API periodically
  - Default poll interval: 300s (5 minutes) - configurable to respect API limits
  - Detects manual changes via remote/app and syncs local state
  - Broadcasts updates to dashboard via WebSocket
  - Config: `mitsubishi.poll_interval_seconds` (optional, defaults to 300)
- **Files Changed**:
  - `src/config.py` - Added `poll_interval_seconds` to MitsubishiConfig (default 300)
  - `src/orchestrator.py` - Added `_poll_hvac_status()` background task, graceful shutdown
  - `CLAUDE.md` - Documented HVAC polling feature
- **Note**: User needs to restart orchestrator for changes to take effect

**2026-01-06** (Session 10)
- **Timestamp-Based Presence Detection** - Simplified architecture, removed idle threshold
  - **Problem**: occupancy_detector was making policy decisions (30s idle = inactive)
  - **Solution**: Send raw timestamps, let state machine decide everything
  - **Key insight**: Door event timestamp IS the threshold - no arbitrary idle time needed!
  - **occupancy_detector.py changes**:
    - Now sends `{"last_active_timestamp": 1704567890.123, "external_monitor": true}`
    - Removed `idle_threshold` parameter from `send_to_orchestrator()`
    - Idle threshold kept only for local console display (informational)
  - **state_machine.py changes**:
    - Removed `mac_active` boolean from `SensorState`
    - Removed `mac_idle_threshold_seconds` from `StateConfig`
    - Simplified `is_present` logic: `mac_presence = external_monitor AND (mac_last_active > door_last_changed)`
    - No idle computations - just timestamp comparisons
  - **Why this works**:
    - Watching movie (no keyboard): door hasn't closed → `mac_last_active > door_last_changed` → PRESENT
    - Walk away: door closes → 10s verification → AWAY
    - Return: keyboard activity → `mac_last_active > door_last_changed` → PRESENT
    - External monitor check prevents false presence when Mac taken elsewhere
  - **orchestrator.py changes**:
    - Updated `/occupancy` endpoint to receive `last_active_timestamp`
    - Passes timestamp to state machine instead of boolean
  - **Removed everywhere**:
    - `mac_idle_threshold_seconds` config option
    - `mac_active` boolean state
    - All idle time computations in state machine
- **Files Changed**:
  - `occupancy_detector.py` - Send timestamp instead of boolean active
  - `src/state_machine.py` - Removed mac_active, simplified is_present logic
  - `src/orchestrator.py` - Accept timestamp from POST /occupancy
  - `src/config.py` - Removed mac_idle_threshold_seconds
  - `CLAUDE.md` - Updated architecture docs, removed idle threshold from thresholds table
- **CO2 Hysteresis Added** - Fixed rapid ERV on/off cycling at threshold
  - **Problem**: ERV oscillates when CO2 hovers around 2000 ppm
    - Turns ON at 2001 ppm, brings CO2 down to 2000 ppm, turns OFF
    - CO2 rises to 2001 ppm again, turns ON, repeat...
  - **Solution**: Different thresholds for ON vs OFF (hysteresis band)
    - Turn ON: CO2 ≥ 2000 ppm
    - Turn OFF: CO2 < 1800 ppm (200 ppm hysteresis)
    - In the 1800-2000 ppm band, ERV maintains current state
  - **Implementation**:
    - Added `co2_critical_hysteresis_ppm: int = 200` to ThresholdsConfig
    - Changed `_evaluate_erv_state()` PRESENT mode logic:
      - `co2_critical_on` checks CO2 ≥ 2000 (turn ON)
      - `co2_critical_off` checks CO2 < 1800 (turn OFF)
      - When running in QUIET mode, only turn OFF if below 1800 ppm
    - Prevents rapid cycling, extends ERV runtime to ensure proper ventilation
  - **Files Changed**:
    - `src/config.py` - Added co2_critical_hysteresis_ppm
    - `src/orchestrator.py` - Hysteresis logic in _evaluate_erv_state()
    - `CLAUDE.md` - Documented hysteresis in Key Thresholds table

**2026-01-08** (Session 12)
- **PWA Testing & Remote Access Planning**
  - Generated PNG icons from SVG (192x192, 512x512)
  - Tested PWA installation on iPhone via local network
  - Explored remote access options: Cloudflare Tunnel (temp URLs, auth issues)
  - **Decided on Tailscale Funnel** for remote access (simpler, free, stable)
- **Backend Serves Frontend** - Simplified deployment
  - Added static file serving to orchestrator
  - Frontend build served from `/frontend/dist`
  - Single port (8080) for both API and UI
  - WebSocket support maintained
- **Remote Access Documentation**
  - Created `tailscale.md` - Step-by-step Tailscale Funnel setup
  - Created `fly.md` - Fly.io alternative (deprecated in favor of Tailscale)
  - Created `fly-proxy/` - Caddy reverse proxy config (unused)
- **Deployment Target Decision**
  - Mac Mini ($40 used) chosen over Raspberry Pi
  - More reliable, more power, macOS compatibility
  - Always-on device for production deployment
- **Files Changed**:
  - `src/orchestrator.py` - Added static file serving for frontend
  - `frontend/public/icon-192.png` - Generated app icon
  - `frontend/public/icon-512.png` - Generated app icon
  - `tailscale.md` - Complete Tailscale setup guide
  - `fly.md` - Fly.io setup guide (deprecated)
  - `fly-proxy/*` - Fly.io proxy config files

**2026-01-08** (Session 13)
- **State Machine Door Open Mode** - Fixed false presence bug
  - **Problem**: Leaving door open and exiting → system thinks user is still present
  - **Solution**: Door open mode (activity-based transitions)
  - **Normal mode** (door recently changed/closed):
    - AWAY → PRESENT: Mac/motion activity AFTER door event
    - PRESENT → AWAY: Door close + 10s verification with no activity
  - **Door open mode** (door open 5+ minutes):
    - AWAY → PRESENT: Immediate on any Mac/motion activity
    - PRESENT → AWAY: After 5 min no activity
    - Door close exits mode → back to normal
  - Handles "door open for ventilation" without false departures
- **YoLink Sensor State Restoration** - Fixed window showing closed after restart
  - **Problem**: YoLink API doesn't support state queries with UAC ("Token is invalid")
  - **Solution**: Restore last known states from database on startup
  - Added `Database.get_latest_device_state()` method
  - Restores door/window/motion states from device_events table
  - Matches Qingping approach (cache restoration)
- **UI Improvements**
  - Open Air status now shows presence: "PRESENT · OPEN AIR" / "AWAY · OPEN AIR"
  - Changed from grey to cyan 🌬️ (positive vibe, not error state)
  - Added PM2.5 and PM10 tiles to vitals grid (µg/m³ units)
  - Backend now includes PM10 in air_quality response
- **HVAC Polling Optimization**
  - Reduced default interval from 5 min → 10 min (fewer API calls)
  - Added night pause (11 PM - 6 AM) - no polling during sleep hours
  - Respects Mitsubishi API while maintaining sync
- **Files Changed**:
  - `src/state_machine.py` - Door open mode implementation (renamed from greedy mode)
  - `src/database.py` - Added get_latest_device_state() method
  - `src/orchestrator.py` - YoLink state restoration, PM10 in API, HVAC night pause
  - `src/config.py` - HVAC poll interval changed to 600s default
  - `frontend/constants.tsx` - OPEN_AIR_PRESENT and OPEN_AIR_AWAY statuses
  - `frontend/App.tsx` - PM2.5/PM10 tiles, open air shows presence
  - `frontend/types.ts` - Added pm25/pm10 to OfficeState
  - `CLAUDE.md` - Updated Operating Modes section

**2026-01-08** (Session 14)
- **Adaptive tVOC Spike Detection** - Catches sub-threshold meal/food events
  - **Problem**: Breakfast/meal tVOC spikes (65-80 points above baseline) fall below 250 threshold
  - **Solution**: Three-phase detection (detect spike → track peak → trigger on decline)
  - **Implementation**:
    - Sliding window history buffer (15 readings, ~7.5 min)
    - Spike detection: tVOC rises 45+ points above 5-reading baseline
    - Peak tracking: Monitor highest value during spike
    - Decline trigger: 2 consecutive drops from peak
    - Ventilation: Run MEDIUM (3/2) until tVOC < 40 (nighttime baseline)
    - Cooldown: 2-hour window prevents re-triggering same event
  - **Configuration** (tuned to actual data):
    - `tvoc_spike_baseline_delta: 45` (observed 65-80 point spikes)
    - `tvoc_spike_min_trigger: 60` (baseline is 30-40)
    - `tvoc_spike_min_peak: 90` (typical peaks 100-117)
    - `tvoc_spike_target: 40` (clear to nighttime baseline)
  - **Dashboard Integration**:
    - New ERV status fields: `spike_ventilation`, `spike_peak`, `spike_cooldown_until`
    - Logs to database: `spike_decline_peak_NNN` reason codes
  - **State Management**:
    - Clears spike state on PRESENT → AWAY transition
    - Complements (doesn't replace) existing tVOC >250 logic
    - Priority: Spike ventilation > tVOC >250 > CO2 critical
- **Files Changed**:
  - `src/config.py` - Added 7 spike detection config parameters
  - `src/orchestrator.py` - Spike detection methods, integration in `_evaluate_erv_state()`
  - `config.yaml` - User-facing spike thresholds (gitignored)
  - `CLAUDE.md` - Updated Current Status, ERV Speed Modes

**2026-01-07** (Session 11)
- **PWA Implementation** - Progressive Web App for iOS
  - Added `manifest.json` with app metadata, icons, shortcuts
  - iOS-specific meta tags for native feel (status bar, home screen)
  - Created SVG icon (placeholder, needs PNG conversion)
  - Added `viewport-fit=cover` for notch/safe area support
- **HTTP Basic Auth** - Optional authentication for remote access
  - Added `auth_username` and `auth_password` to OrchestratorConfig
  - Middleware checks Authorization header, returns 401 if invalid
  - CORS updated to allow Authorization header
  - Disabled by default (no credentials = no auth required)
  - Updated config.example.yaml with auth fields
- **Cloudflare Tunnel + Access Documentation** - Comprehensive deployment guide
  - Step-by-step Pi deployment (systemd service)
  - Cloudflare Tunnel setup (cloudflared installation, config)
  - **Cloudflare Access** for email-based auth (recommended over Basic Auth)
    - One-time email codes (no passwords)
    - Whitelist rajeshgoli@gmail.com
    - 24-hour session persistence
  - PWA installation instructions for iPhone
  - Architecture diagrams for remote access flow
- **Files Changed**:
  - `frontend/public/manifest.json` - PWA manifest
  - `frontend/public/icon.svg` - App icon (SVG placeholder)
  - `frontend/public/ICONS_TODO.md` - Icon conversion instructions
  - `frontend/index.html` - PWA meta tags, manifest link
  - `src/config.py` - Added auth_username/auth_password to OrchestratorConfig
  - `src/orchestrator.py` - HTTP Basic Auth middleware
  - `config.example.yaml` - Auth configuration example
  - `CLAUDE.md` - Authentication section, Cloudflare Tunnel deployment guide
  - `roadmap.md` - Updated with Phase 5 (Mobile & Remote Access), historical charts in P2
