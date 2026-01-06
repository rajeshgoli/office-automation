# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Git Commits

**Do NOT add Claude attributions to commits.** No "Generated with Claude Code" or "Co-Authored-By" lines. Single developer repo - Claude authorship is assumed.

## Project Overview

Office Climate Automation system for a backyard shed office. The system coordinates multiple smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.

## Current Status (2026-01-05)

**Working:**
- YoLink sensors (door, window, motion) via Cloud MQTT
- ERV control via Tuya local API (tinytuya)
- Qingping Air Monitor bound to local MQTT (Mosquitto)
- State machine (PRESENT/AWAY) with all inputs
- Orchestrator wired up and ready to run

**Pending:**
- First Qingping sensor data via MQTT (device uploads every 15 min)
- Mitsubishi Kumo not wired into orchestrator (client exists in `src/kumo_client.py`)

## Hardware Devices

| Device | Control Method | Status |
|--------|---------------|--------|
| **ERV (Pioneer Airlink)** | Tuya local (`192.168.5.119`) | Working |
| **Qingping Air Monitor** | Local MQTT (`127.0.0.1:1883`) | Configured, waiting for data |
| **YoLink Sensors** | Cloud MQTT (`api.yosmart.com:8003`) | Working |
| **Mitsubishi Split AC** | pykumo (Kumo Cloud) | Client exists, not wired |
| **Mac + External Monitor** | HTTP POST to orchestrator | Working |

## Core Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  Mac Occupancy  │────▶│                 │     │  Qingping MQTT  │
│   (HTTP POST)   │     │                 │◀────│  (CO2/Temp/RH)  │
└─────────────────┘     │                 │     └─────────────────┘
                        │   Orchestrator  │              │
┌─────────────────┐     │                 │     ┌────────▼────────┐
│  YoLink Sensors │────▶│   State Machine │────▶│   ERV Control   │
│ (Door/Window/   │     │  (PRESENT/AWAY) │     │  (Tuya Local)   │
│   Motion)       │     │                 │     └─────────────────┘
└─────────────────┘     └─────────────────┘
```

### Two-State System
- **PRESENT**: Quiet mode - ERV off unless CO2 > 2000 ppm
- **AWAY**: Ventilation mode - ERV TURBO until CO2 < 500 ppm

### Safety Interlocks
- Window OR door open → ERV OFF
- ANY presence signal → immediate ERV shutoff

### Key Thresholds
| Parameter | Value |
|-----------|-------|
| CO2 critical (ERV on while present) | 2000 ppm |
| CO2 refresh target (away mode) | 500 ppm |
| Motion timeout | 60 seconds |

## Code Structure

```
office-automate/
├── config.yaml            # Credentials and device config
├── requirements.txt       # Python dependencies
├── occupancy_detector.py  # macOS presence detection
├── run.py                 # Main entry point
├── roadmap.md             # Progress tracking (update as you work!)
└── src/
    ├── config.py          # Config loader
    ├── yolink_client.py   # YoLink cloud API (HTTP + MQTT)
    ├── qingping_client.py # Qingping MQTT client (local broker)
    ├── erv_client.py      # ERV control via Tuya local
    ├── kumo_client.py     # Mitsubishi Kumo Cloud (NOT WIRED YET)
    ├── state_machine.py   # PRESENT/AWAY state logic
    └── orchestrator.py    # Main coordinator
```

## Running

```bash
# Activate venv
source venv/bin/activate

# Start Mosquitto (required for Qingping)
brew services start mosquitto

# Run the orchestrator
python run.py

# Check status
curl http://localhost:8080/status
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
- JSON format: `{"type":17,"sensor_data":{"co2":{"value":800},"temperature":{"value":22},...}}`

### YoLink (Cloud)
- Using Cloud API (not local hub)
- MQTT broker: `api.yosmart.com:8003`
- Credentials: in config.yaml (uaid, secret_key)

## Next Steps for Future Agent

1. **Verify Qingping data** - Run `mosquitto_sub -h 127.0.0.1 -t "qingping/#" -v` and wait for data
2. **Test automation** - Start orchestrator, leave office, verify ERV turns on
3. **Wire Mitsubishi** - Add kumo_client to orchestrator for HVAC control
4. **Reduce upload interval** - Consider changing to 10 min in developer portal
5. **Deploy to Pi** - Move orchestrator to always-on Raspberry Pi

## Deployment

- **Development**: Run on Mac for testing
- **Production**: Deploy orchestrator to Raspberry Pi (always-on)
- **macOS detector**: Stays on Mac, POSTs to orchestrator
