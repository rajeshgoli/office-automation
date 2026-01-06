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

### Qingping Data Collection
- Device bound to local MQTT broker (Mosquitto on Mac)
- Upload interval: 15 minutes
- Waiting for first sensor data to confirm end-to-end flow

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
- [ ] **Rule 3**: HVAC control based on Qingping readings (Mitsubishi client exists but not wired)
- [x] **Rule 4**: Safety interlocks (window/door open = systems off)

### Phase 5: Deployment
- [ ] Run as background service (launchd on macOS or systemd on Linux)
- [ ] Logging and monitoring
- [ ] Alerting for failures
- [ ] Test full end-to-end flow with real Qingping data

---

## Future / P2

- [ ] Predictive pre-conditioning based on arrival patterns
- [ ] Calendar integration for meeting awareness
- [ ] Seasonal threshold adjustments
- [x] Dashboard / visualization ✅ (Session 3)

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
