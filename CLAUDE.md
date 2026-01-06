# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Git Commits

**Do NOT add Claude attributions to commits.** No "Generated with Claude Code" or "Co-Authored-By" lines. Single developer repo - Claude authorship is assumed.

## Project Overview

Office Climate Automation system for a backyard shed office. The system coordinates multiple smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.

## Current Status (2026-01-06)

**Working:**
- YoLink sensors (door, window, motion) via Cloud MQTT
- ERV control via Tuya local API (tinytuya)
- Qingping Air Monitor via local MQTT - CO2, temp, humidity, tVOC index, noise dB
- State machine (PRESENT/AWAY) with robust presence detection
- SQLite database for persistence and historical analysis
- React dashboard with live data (WebSocket + polling fallback)
- Dashboard quick controls for manual ERV/HVAC override
- macOS occupancy detector (keyboard/mouse activity)
- HVAC coordination (suspends heating when ERV venting)
- HVAC status polling (syncs dashboard with manual remote/app changes)
- tVOC-triggered ventilation (VOC index >250 triggers ERV)

**Pending:**
- Deploy to Raspberry Pi for always-on operation

## Hardware Devices

| Device | Control Method | Status |
|--------|---------------|--------|
| **ERV (Pioneer Airlink)** | Tuya local (`192.168.5.119`) | Working |
| **Qingping Air Monitor** | Local MQTT (`127.0.0.1:1883`) | Working |
| **YoLink Sensors** | Cloud MQTT (`api.yosmart.com:8003`) | Working |
| **Mitsubishi Split AC** | pykumo (Kumo Cloud) | Working |
| **Mac Keyboard/Mouse** | HTTP POST to orchestrator | Working |

## Core Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  Mac Occupancy  │────▶│                 │     │  Qingping MQTT  │
│  (kbd/mouse)    │     │                 │◀────│  (CO2/Temp/RH)  │
└─────────────────┘     │                 │     └─────────────────┘
                        │   Orchestrator  │              │
┌─────────────────┐     │                 │     ┌────────▼────────┐
│  YoLink Sensors │────▶│   State Machine │────▶│   ERV Control   │
│ (Door/Window/   │     │  (PRESENT/AWAY) │     │  (Tuya Local)   │
│   Motion)       │     │                 │     └─────────────────┘
└─────────────────┘     └────────┬────────┘
                                 │
                        ┌────────▼────────┐
                        │  HVAC Control   │
                        │  (Kumo Cloud)   │
                        └─────────────────┘
```

### Two-State System
- **PRESENT**: Quiet mode - ERV off unless CO2 > 2000 ppm or tVOC index > 250
- **AWAY**: Ventilation mode - ERV PURGE until CO2 < 500 ppm

### Presence Detection Logic

**Architecture:**
- Mac occupancy detector sends **raw timestamps** of last keyboard/mouse activity
- State machine uses timestamp comparisons - NO idle threshold needed!
- Door event timestamp is the source of truth for presence/departure

**AWAY → PRESENT triggers:**
- Mac keyboard/mouse activity AFTER last door event (strongest signal)
- Motion detected AFTER last door event while door is closed

**NOT triggers for PRESENT:**
- Door opening alone (might just grab something)
- External monitor connected alone (just means Mac is there)

**PRESENT → AWAY triggers:**
- Door closes + 10s verification with no activity

**Timestamp-based detection:**
- `mac_presence = external_monitor AND (mac_last_active > door_last_changed)`
- Only activity AFTER last door event counts as presence
- Pre-departure activity (walking to door) automatically ignored
- Watching a movie (no keyboard): door hasn't closed → still PRESENT
- Walk away: door closes → departure verification → AWAY

### ERV Speed Modes
| Mode | Fan Speed | Trigger |
|------|-----------|---------|
| **OFF** | 0/0 | Default when present, air quality OK |
| **QUIET** | 1/1 | CO2 > 2000 ppm while present |
| **ELEVATED** | 3/2 | tVOC index > 250 (positive pressure) |
| **PURGE** | 8/8 | Away mode CO2 refresh |

### Safety Interlocks
- Window OR door open → ERV OFF
- ANY presence signal → immediate PRESENT transition

### Key Thresholds
| Parameter | Value | Notes |
|-----------|-------|-------|
| CO2 critical (ERV on while present) | 2000 ppm | Turn ON threshold |
| CO2 critical hysteresis | 200 ppm | Turn OFF at (2000-200) = 1800 ppm |
| CO2 refresh target (away mode) | 500 ppm | |
| tVOC index threshold (triggers ELEVATED) | 250 | |
| Motion timeout | 60 seconds | |
| Departure verification | 10 seconds | |
| Manual override timeout | 30 minutes | |

**Hysteresis explained:** ERV turns ON when CO2 ≥ 2000 ppm, but stays ON until CO2 < 1800 ppm. This prevents rapid on/off cycling when CO2 hovers around the threshold.

## Code Structure

```
office-automate/
├── config.yaml            # Credentials and device config
├── requirements.txt       # Python dependencies
├── occupancy_detector.py  # macOS presence detection (keyboard/mouse)
├── run.py                 # Main entry point (--port arg supported)
├── roadmap.md             # Progress tracking (update as you work!)
├── data/                  # SQLite database (gitignored)
│   └── office_climate.db
├── frontend/              # React + Vite dashboard
│   ├── App.tsx            # Main dashboard with quick controls
│   ├── api.ts             # API client + WebSocket
│   ├── types.ts           # TypeScript types (ERVSpeed, etc.)
│   └── ...
└── src/
    ├── config.py          # Config loader
    ├── database.py        # SQLite persistence + analysis
    ├── yolink_client.py   # YoLink cloud API (HTTP + MQTT)
    ├── qingping_client.py # Qingping MQTT client (local broker)
    ├── erv_client.py      # ERV control via Tuya local
    ├── kumo_client.py     # Mitsubishi Kumo Cloud
    ├── state_machine.py   # PRESENT/AWAY state logic
    └── orchestrator.py    # Main coordinator + HTTP/WS server
```

## Database

**Location:** `data/office_climate.db` (SQLite)

**Tables:**
- `sensor_readings` - CO2, temp, humidity, tVOC, PM2.5, noise history
  - Columns: `timestamp`, `co2_ppm`, `temp_c`, `humidity`, `pm25`, `pm10`, `tvoc`, `noise_db`, `source`
- `occupancy_log` - State changes (PRESENT/AWAY) with timestamps
- `device_events` - Door, window, motion events
- `climate_actions` - ERV/HVAC control actions with reasons

**Important:** All timestamps are stored in **local time (PST)**, not UTC. This was changed in Session 6 to simplify debugging and analysis.

**Querying Examples:**
```bash
# Recent door events (timestamps already in PST)
sqlite3 data/office_climate.db "SELECT timestamp, event FROM device_events WHERE device_type='door' ORDER BY timestamp DESC LIMIT 10"

# CO2 trend (last 24 hours)
sqlite3 data/office_climate.db "SELECT timestamp, co2_ppm FROM sensor_readings WHERE timestamp > datetime('now', '-24 hours') ORDER BY timestamp"
```

## API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/status` | Current system status (JSON) |
| GET | `/ws` | WebSocket for real-time updates |
| POST | `/occupancy` | Mac keyboard/mouse activity `{"last_active_timestamp": 1704567890.123, "external_monitor": true}` |
| POST | `/erv` | Manual ERV control `{"speed": "off\|quiet\|medium\|turbo"}` |
| POST | `/hvac` | Manual HVAC control `{"mode": "off\|heat", "setpoint_f": 70}` |
| POST | `/qingping/interval` | Configure sensor report interval `{"interval": 60}` (seconds, min 15) |

## Running

```bash
# Terminal 1: Backend
source venv/bin/activate
brew services start mosquitto  # Required for Qingping
python run.py --port 9001

# Terminal 2: Frontend
cd frontend
VITE_API_PORT=9001 npm run dev -- --port 9002

# Terminal 3: Mac occupancy detector
python3 occupancy_detector.py --watch --url http://localhost:9001

# Check status
curl http://localhost:9001/status

# Manual ERV control
curl -X POST http://localhost:9001/erv -H "Content-Type: application/json" -d '{"speed":"turbo"}'
```

## Device Details

### ERV (Tuya Local)
- Device ID: `ebfb18b2fc8f6dc63eqvcw`
- IP: `192.168.5.119`
- Local Key: in config.yaml
- DPS: 1 (power), 101 (SA fan 1-8), 102 (EA fan 1-8)
- Presets: QUIET (1/1), MEDIUM (3/2), TURBO (8/8)

### Qingping (Local MQTT)
- MAC: `582D3470765F`
- Broker: Mosquitto on `127.0.0.1:1883`
- Topic (receive): `qingping/582D3470765F/up`
- Topic (configure): `qingping/582D3470765F/down`
- Report interval: 30s (configurable in config.yaml, min 15s)
- Sensors: CO2 (ppm), temp (°C), humidity (%), PM2.5, PM10, tVOC index (SGP40 0-500 scale), noise (dB)
- All sensor data saved to database for historical analysis
- Configured automatically on startup, or via POST `/qingping/interval`

### YoLink (Cloud)
- Using Cloud API (not local hub)
- MQTT broker: `api.yosmart.com:8003`
- Credentials: in config.yaml (uaid, secret_key)

### Mitsubishi Kumo
- Control via Kumo Cloud API (custom client)
- Suspends heating when ERV is venting (don't heat cold outside air)
- Resumes when ERV stops or user returns
- Status polling: Syncs dashboard with manual changes (remote/app)
  - Default: 300s (5 min) - configurable via `mitsubishi.poll_interval_seconds`
  - Updates local state and broadcasts to dashboard via WebSocket
  - Note: Cloud API polling only - no push notifications available

## Deployment

- **Development**: Run on Mac for testing
- **Production**: Deploy orchestrator to Raspberry Pi (always-on)
- **macOS detector**: Stays on Mac, POSTs to orchestrator
