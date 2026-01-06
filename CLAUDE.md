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
- Qingping Air Monitor via local MQTT - CO2, temp, humidity, tVOC
- State machine (PRESENT/AWAY) with robust presence detection
- SQLite database for persistence and historical analysis
- React dashboard with live data (WebSocket + polling fallback)
- Dashboard quick controls for manual ERV/HVAC override
- macOS occupancy detector (keyboard/mouse activity)
- HVAC coordination (suspends heating when ERV venting)
- tVOC-triggered ventilation (>250 ppb triggers ERV)

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
- **PRESENT**: Quiet mode - ERV off unless CO2 > 2000 ppm or tVOC > 250 ppb
- **AWAY**: Ventilation mode - ERV PURGE until CO2 < 500 ppm

### Presence Detection Logic

**AWAY → PRESENT triggers:**
- Mac keyboard/mouse activity (strongest signal)
- Motion detected while door is closed

**NOT triggers for PRESENT:**
- Door opening alone (might just grab something)
- External monitor connected (just means Mac is there)

**PRESENT → AWAY triggers:**
- Door closes + 10s no activity (departure verification)

### ERV Speed Modes
| Mode | Fan Speed | Trigger |
|------|-----------|---------|
| **OFF** | 0/0 | Default when present, air quality OK |
| **QUIET** | 1/1 | CO2 > 2000 ppm while present |
| **ELEVATED** | 3/2 | tVOC > 250 ppb (positive pressure) |
| **PURGE** | 8/8 | Away mode CO2 refresh |

### Safety Interlocks
- Window OR door open → ERV OFF
- ANY presence signal → immediate PRESENT transition

### Key Thresholds
| Parameter | Value |
|-----------|-------|
| CO2 critical (ERV on while present) | 2000 ppm |
| CO2 refresh target (away mode) | 500 ppm |
| tVOC threshold (triggers ELEVATED) | 250 ppb |
| Motion timeout | 60 seconds |
| Departure verification | 10 seconds |
| Manual override timeout | 30 minutes |

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

## API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/status` | Current system status (JSON) |
| GET | `/ws` | WebSocket for real-time updates |
| POST | `/occupancy` | Mac keyboard/mouse activity |
| POST | `/erv` | Manual ERV control `{"speed": "off\|quiet\|medium\|turbo"}` |
| POST | `/hvac` | Manual HVAC control `{"mode": "off\|heat", "setpoint_f": 70}` |

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
- Topic: `qingping/582D3470765F/up`
- Upload interval: 15 minutes (set in developer.qingping.co)

### YoLink (Cloud)
- Using Cloud API (not local hub)
- MQTT broker: `api.yosmart.com:8003`
- Credentials: in config.yaml (uaid, secret_key)

### Mitsubishi Kumo
- Control via pykumo library
- Suspends heating when ERV is venting (don't heat cold outside air)
- Resumes when ERV stops or user returns

## Deployment

- **Development**: Run on Mac for testing
- **Production**: Deploy orchestrator to Raspberry Pi (always-on)
- **macOS detector**: Stays on Mac, POSTs to orchestrator
