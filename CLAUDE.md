# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Office Climate Automation system for a backyard shed office. The system coordinates multiple smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.

## Current Status (2026-01-09)

**✅ Deployed to Mac Mini (Always-On Production)**
- Mac Mini (macOS High Sierra 10.13.6) — hostname `bakasura4.local` (IP in config.yaml)
- All services auto-start on boot via Launch Agents
- MQTT broker (amqtt), Orchestrator, LocalTunnel all running
- Remote access via LocalTunnel at https://climate.loca.lt
- SSH enabled for remote management

**Working:**
- YoLink sensors (door, window, motion) via Cloud MQTT
- ERV control via Tuya local API (tinytuya)
- Qingping Air Monitor via local MQTT (Mac Mini — broker in config.yaml)
- State machine (PRESENT/AWAY) with door open mode for ventilation scenarios
- SQLite database for persistence and historical analysis (restores sensor states on startup)
- React dashboard with live data (WebSocket + polling fallback)
- Dashboard quick controls for manual ERV/HVAC override
- macOS occupancy detector on work Mac (sends to Mac Mini orchestrator)
- HVAC coordination (suspends heating when ERV venting)
- HVAC status polling (syncs dashboard with manual remote/app changes, pauses at night)
- tVOC-triggered ventilation (VOC index >250 triggers ERV)
- **Adaptive tVOC spike detection** - Catches sub-threshold meal/food events (45+ point spikes above baseline)
- **Two-phase AWAY mode ventilation** - 30 min forced TURBO, then adaptive speed based on CO2 fall rate
- **CO2 plateau detection** - Learns outdoor baseline (~490 ppm winter) and stops ERV when equilibrium reached
- PWA support for iOS home screen installation
- **Google OAuth 2.0 authentication** - Secure JWT-based auth for dashboard and API (ready for testing)

## Hardware Devices

| Device | Control Method | Status |
|--------|---------------|--------|
| **ERV (Pioneer Airlink)** | Tuya local (IP in config.yaml) | Working |
| **Qingping Air Monitor** | Local MQTT (broker in config.yaml) | Working |
| **YoLink Sensors** | Cloud MQTT (`api.yosmart.com:8003`) | Working |
| **Mitsubishi Split AC** | pykumo (Kumo Cloud) | Working |
| **Mac Keyboard/Mouse** | HTTP POST to orchestrator (server in config.yaml) | Working |
| **Mac Mini (bakasura4)** | SSH (`bakasura4.local`) | Production host |

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
- **AWAY**: Adaptive ventilation mode - ERV speed adjusts based on CO2 fall rate, stops at outdoor baseline plateau

### Operating Modes

The state machine has two operating modes that handle different door scenarios:

**Normal Mode** (Door recently changed or closed):
- Transitions require door events
- AWAY → PRESENT: Mac OR motion activity AFTER door event
- PRESENT → AWAY: Door close + 10s verification with no activity

**Door Open Mode** (Door open for 5+ minutes):
- Enables when door stays open (for ventilation)
- AWAY → PRESENT: Immediately on any Mac OR motion activity
- PRESENT → AWAY: After 5 minutes of no activity
- Free transitions based on activity alone
- Door close exits door open mode → back to normal

This prevents false departures when leaving the door open for ventilation.

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

**PRESENT Mode:**
| Mode | Fan Speed | Trigger |
|------|-----------|---------|
| **OFF** | 0/0 | Default when air quality OK |
| **QUIET** | 1/1 | CO2 > 2000 ppm (hysteresis: off at 1800 ppm) |
| **ELEVATED** | 3/2 | tVOC index > 250 OR adaptive spike detected (positive pressure) |

**AWAY Mode (Two-Phase Ventilation):**

*Phase 1: Initial Purge (first 30 minutes)*
| Mode | Fan Speed | Notes |
|------|-----------|-------|
| **TURBO** | 8/8 | Forced for first 30 min regardless of CO2 level or rate |

*Phase 2: Adaptive Control (after 30 minutes)*
| Mode | Fan Speed | CO2 Fall Rate | Notes |
|------|-----------|---------------|-------|
| **TURBO** | 8/8 | > 8 ppm/min | Fast CO2 drop, stay aggressive |
| **MEDIUM** | 3/2 | 2-8 ppm/min | Moderate drop, stepping down |
| **QUIET** | 1/1 | 0.5-2 ppm/min | Slow drop, approaching equilibrium |
| **OFF** | 0/0 | < 0.5 ppm/min for 10 min | Plateau detected, outdoor baseline reached |

**Why 30 minutes?** Historical data analysis showed:
- 0-10 min: Air mixing/ramping (minimal CO2 drop)
- 10-30 min: High-efficiency window (~10 ppm/min drop)
- 30+ min: Diminishing returns, adaptive can optimize

**Outdoor Baseline Detection:** System learns outdoor CO2 level (e.g., ~490 ppm in winter) and stops ventilating when equilibrium is reached, preventing infinite runtime.

### Safety Interlocks
- Window OR door open → ERV OFF
- ANY presence signal → immediate PRESENT transition

### Key Thresholds
| Parameter | Value | Notes |
|-----------|-------|-------|
| CO2 critical (ERV on while present) | 2000 ppm | Turn ON threshold |
| CO2 critical hysteresis | 200 ppm | Turn OFF at (2000-200) = 1800 ppm |
| CO2 refresh target (away mode) | 500 ppm | Start target (adaptive control takes over) |
| tVOC index threshold (triggers ELEVATED) | 250 | |
| **CO2 plateau rate threshold** | **0.5 ppm/min** | Slower than this = outdoor baseline reached |
| **CO2 plateau window** | **10 minutes** | Sustained slow rate duration |
| **CO2 plateau min safety** | **600 ppm** | Don't plateau above this (safety margin for winter ~490 ppm) |
| **TURBO duration** | **30 minutes** | Force TURBO for this long after departure, then adaptive |
| **Adaptive TURBO threshold** | **> 8 ppm/min** | CO2 falling fast (after initial 30 min) |
| **Adaptive MEDIUM threshold** | **2-8 ppm/min** | CO2 falling moderately |
| **Adaptive QUIET threshold** | **0.5-2 ppm/min** | CO2 falling slowly |
| Motion timeout | 60 seconds | |
| Departure verification | 10 seconds | |
| Door open mode threshold | 5 minutes | Door open this long → activity-based transitions |
| Door open mode away timeout | 5 minutes | No activity for this long → AWAY |
| Manual override timeout | 30 minutes | |
| HVAC polling interval | 10 minutes | Sync dashboard with manual changes |
| HVAC night pause | 11 PM - 6 AM | Skip polling during sleep hours |

**Hysteresis explained:** ERV turns ON when CO2 ≥ 2000 ppm, but stays ON until CO2 < 1800 ppm. This prevents rapid on/off cycling when CO2 hovers around the threshold.

**Plateau detection explained:** When AWAY, the system tracks CO2 fall rate. When the rate drops below 0.5 ppm/min for 10 consecutive minutes, it means outdoor air equilibrium has been reached (e.g., ~490 ppm in winter). The ERV stops to save energy instead of running indefinitely trying to reach an impossible target.

## Code Structure

```
office-automate/
├── config.yaml            # Credentials and device config (includes OAuth)
├── requirements.txt       # Python dependencies
├── occupancy_detector.py  # macOS presence detection (OAuth device flow)
├── oauth_device_client.py # OAuth device flow client (for detector)
├── run.py                 # Main entry point (--port arg supported)
├── roadmap.md             # Progress tracking (update as you work!)
├── OAUTH_HANDOFF.md       # OAuth implementation documentation
├── data/                  # SQLite database (gitignored)
│   └── office_climate.db
├── frontend/              # React + Vite dashboard
│   ├── App.tsx            # Main dashboard with auth state + quick controls
│   ├── Login.tsx          # OAuth login screen
│   ├── api.ts             # API client + WebSocket + token management
│   ├── types.ts           # TypeScript types (ERVSpeed, etc.)
│   └── ...
└── src/
    ├── config.py          # Config loader (includes GoogleOAuthConfig)
    ├── database.py        # SQLite persistence + analysis
    ├── oauth_service.py   # OAuth 2.0 service (JWT tokens, device flow)
    ├── yolink_client.py   # YoLink cloud API (HTTP + MQTT)
    ├── qingping_client.py # Qingping MQTT client (local broker)
    ├── erv_client.py      # ERV control via Tuya local
    ├── kumo_client.py     # Mitsubishi Kumo Cloud
    ├── state_machine.py   # PRESENT/AWAY state logic
    └── orchestrator.py    # Main coordinator + HTTP/WS server + OAuth endpoints
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

**State Restoration:** On startup, the orchestrator restores last known sensor states from the database:
- Qingping: Latest CO2/temp/humidity reading (prevents showing 0 until first MQTT update)
- YoLink: Latest door/window/motion states (YoLink API doesn't support state queries with UAC)

**Querying Examples:**
```bash
# Recent door events (timestamps already in PST)
sqlite3 data/office_climate.db "SELECT timestamp, event FROM device_events WHERE device_type='door' ORDER BY timestamp DESC LIMIT 10"

# CO2 trend (last 24 hours)
sqlite3 data/office_climate.db "SELECT timestamp, co2_ppm FROM sensor_readings WHERE timestamp > datetime('now', '-24 hours') ORDER BY timestamp"
```

## Authentication

**Google OAuth 2.0 (Primary)** - Secure JWT-based authentication for dashboard and API access.

### Setup Google OAuth

1. **Google Cloud Console** (https://console.cloud.google.com):
   - Create project: "Office Climate Automation"
   - Enable "Google+ API" or "Google Identity Services"
   - Create OAuth 2.0 Client ID (Web application)
   - Add authorized redirect URIs:
     - `http://localhost:8080/auth/callback` (development)
     - `http://bakasura4.local:8080/auth/callback` (local network)
     - `https://climate.loca.lt/auth/callback` (remote access)

2. **Update config.yaml**:
```yaml
orchestrator:
  google_oauth:
    client_id: "YOUR_CLIENT_ID.apps.googleusercontent.com"
    client_secret: "YOUR_CLIENT_SECRET"
    allowed_emails:
      - "your_email@gmail.com"
```

### OAuth Features

**Authorization Code Flow (Browser/Dashboard):**
- Login screen with "Sign in with Google" button
- PKCE for secure authorization without exposing client secret
- JWT tokens (7-day expiry) stored in localStorage
- User email display and logout button in dashboard

**Device Flow (Occupancy Detector):**
- Headless authentication for background services
- Shows device code and verification URL
- Automatic token refresh
- Token stored in `~/.office-automate/auth_token.json`

**API Security:**
- All endpoints require `Authorization: Bearer <JWT>` header
- WebSocket requires auth token as first message
- Email allowlist enforcement (only authorized emails can access)
- Automatic 401 handling and token refresh

### HTTP Basic Auth (Fallback)

OAuth is optional. If not configured, the system falls back to HTTP Basic Auth:

```yaml
orchestrator:
  auth_username: "admin"
  auth_password: "your_secure_password"
```

**Note:** Basic Auth is deprecated. Use OAuth for production deployments.

## API Endpoints

### OAuth Endpoints
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/auth/login` | Initiate OAuth flow, returns Google authorization URL |
| GET | `/auth/callback` | OAuth callback, exchanges code for JWT token |
| POST | `/auth/logout` | Invalidate user session |
| POST | `/auth/device/start` | Start device flow (for occupancy detector) |
| POST | `/auth/device/poll` | Poll device authorization status |

### System Endpoints (Require Authentication)
| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/status` | Current system status (JSON) |
| GET | `/ws` | WebSocket for real-time updates (auth token required) |
| POST | `/occupancy` | Mac keyboard/mouse activity `{"last_active_timestamp": 1704567890.123, "external_monitor": true}` |
| POST | `/erv` | Manual ERV control `{"speed": "off\|quiet\|medium\|turbo"}` |
| POST | `/hvac` | Manual HVAC control `{"mode": "off\|heat", "setpoint_f": 70}` |
| POST | `/qingping/interval` | Configure sensor report interval `{"interval": 60}` (seconds, min 15) |
| GET | `/localtunnel/password` | Fetch LocalTunnel tunnel password from loca.lt API |

## Running

```bash
# Terminal 1: Backend
source venv/bin/activate
brew services start mosquitto  # Required for Qingping
python run.py --port 9001

# Terminal 2: Frontend
cd frontend
VITE_API_PORT=9001 npm run dev -- --port 9002

# Terminal 3: Mac occupancy detector (OAuth)
python3 occupancy_detector.py --watch --url http://localhost:9001 --reauth
# First run: Follow device code flow to authenticate
# Subsequent runs: Omit --reauth flag, uses saved token

# Check status (with OAuth)
TOKEN="your_jwt_token_here"
curl -H "Authorization: Bearer $TOKEN" http://localhost:9001/status

# Manual ERV control (with OAuth)
curl -X POST http://localhost:9001/erv \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"speed":"turbo"}'
```

## Device Details

### ERV (Tuya Local)
- Device ID: in config.yaml (`tuya.device_id`)
- IP: in config.yaml (`tuya.device_ip`)
- Local Key: in config.yaml (`tuya.local_key`)
- DPS: 1 (power), 101 (SA fan 1-8), 102 (EA fan 1-8)
- Presets: QUIET (1/1), MEDIUM (3/2), TURBO (8/8)

### Qingping (Local MQTT)
- MAC: in config.yaml (`qingping.mac`)
- Broker: amqtt on Mac Mini (configured in config.yaml)
- Topic (receive): `qingping/<mac>/up`
- Topic (configure): `qingping/<mac>/down`
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

### Local Development
- **Backend**: Run orchestrator on Mac (`python run.py`)
- **Frontend**: Run Vite dev server (`npm run dev`)
- **macOS detector**: Run locally (`python3 occupancy_detector.py --watch`)
- Access at `http://localhost:9001` (backend) and `http://localhost:9002` (frontend)

### Production: Mac Mini + LocalTunnel

**✅ Currently Deployed**

**Architecture:**
```
┌─────────────────┐
│   iPhone/iPad   │
│   (anywhere)    │
└────────┬────────┘
         │ HTTPS
         ▼
┌─────────────────────────────────┐
│  LocalTunnel                     │
│  https://climate.loca.lt        │
│  (HTTP Basic Auth)               │
└────────┬────────────────────────┘
         │ Encrypted tunnel
         ▼
┌─────────────────────────────────┐
│  Mac Mini (bakasura4.local)     │
│  orchestrator:8080 + amqtt:1883 │
└─────────────────────────────────┘
         ▲
         │ HTTP POST
┌────────┴────────┐
│ Work Mac (MBP)  │
│ occupancy_detector
└─────────────────┘
```

### Setup Instructions (Already Deployed)

**Mac Mini Setup:**

1. **✅ Set eero DHCP Reservation:**
   - Mac Mini (bakasura4) should have a static DHCP reservation (configure IP in config.yaml)

2. **✅ Install Python dependencies:**
   ```bash
   cd ~/office-automate
   python3 -m venv venv
   source venv/bin/activate
   pip install -r requirements.txt
   pip install --user amqtt  # MQTT broker (Homebrew too new for High Sierra)
   ```

3. **✅ Install Node.js for LocalTunnel:**
   ```bash
   # Node.js v14.21.3 (last version for High Sierra)
   wget https://nodejs.org/dist/v14.21.3/node-v14.21.3-darwin-x64.tar.gz
   tar -xzf node-v14.21.3-darwin-x64.tar.gz
   sudo mv node-v14.21.3-darwin-x64 /usr/local/node
   sudo ln -s /usr/local/node/bin/node /usr/local/bin/node
   sudo ln -s /usr/local/node/bin/npx /usr/local/bin/npx
   ```

4. **✅ Reconfigure Qingping to use Mac Mini IP:**
   - Via Qingping developer portal: Set MQTT broker to `bakasura4.local:1883`

5. **✅ Enable SSH:**
   ```bash
   sudo systemsetup -setremotelogin on
   # Set up SSH key from work Mac for passwordless login
   ```

6. **✅ Prevent Mac Mini sleep:**
   ```bash
   sudo pmset -c sleep 0
   sudo pmset -c disksleep 0
   sudo pmset -c displaysleep 10
   ```

7. **✅ Create Launch Agents (auto-start on boot):**

   **MQTT Broker (amqtt):**
   ```bash
   cat > ~/Library/LaunchAgents/com.office-automate.mqtt.plist << 'EOF'
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
   <plist version="1.0">
   <dict>
     <key>Label</key>
     <string>com.office-automate.mqtt</string>
     <key>ProgramArguments</key>
     <array>
       <string>/Users/rajesh/Library/Python/3.10/bin/amqtt</string>
     </array>
     <key>RunAtLoad</key>
     <true/>
     <key>KeepAlive</key>
     <true/>
   </dict>
   </plist>
   EOF

   launchctl load ~/Library/LaunchAgents/com.office-automate.mqtt.plist
   ```

   **Orchestrator:**
   ```bash
   cat > ~/Library/LaunchAgents/com.office-automate.orchestrator.plist << 'EOF'
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
   <plist version="1.0">
   <dict>
     <key>Label</key>
     <string>com.office-automate.orchestrator</string>
     <key>ProgramArguments</key>
     <array>
       <string>/Users/rajesh/office-automate/venv/bin/python</string>
       <string>/Users/rajesh/office-automate/run.py</string>
     </array>
     <key>WorkingDirectory</key>
     <string>/Users/rajesh/office-automate</string>
     <key>RunAtLoad</key>
     <true/>
     <key>KeepAlive</key>
     <true/>
   </dict>
   </plist>
   EOF

   launchctl load ~/Library/LaunchAgents/com.office-automate.orchestrator.plist
   ```

   **LocalTunnel:**
   ```bash
   cat > ~/Library/LaunchAgents/com.office-automate.localtunnel.plist << 'EOF'
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
   <plist version="1.0">
   <dict>
     <key>Label</key>
     <string>com.office-automate.localtunnel</string>
     <key>ProgramArguments</key>
     <array>
       <string>/usr/local/bin/npx</string>
       <string>localtunnel</string>
       <string>--port</string>
       <string>8080</string>
       <string>--subdomain</string>
       <string>climate</string>
     </array>
     <key>RunAtLoad</key>
     <true/>
     <key>KeepAlive</key>
     <true/>
   </dict>
   </plist>
   EOF

   launchctl load ~/Library/LaunchAgents/com.office-automate.localtunnel.plist
   ```

8. **✅ Configure HTTP Basic Auth in config.yaml:**
   ```yaml
   orchestrator:
     host: "0.0.0.0"
     port: 8080
     auth_username: "admin"
     auth_password: "your_password"
   ```

**Work Mac Setup:**

9. **✅ Create occupancy detector Launch Agent:**
   ```bash
   cat > ~/Library/LaunchAgents/com.office-automate.occupancy.plist << 'EOF'
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
   <plist version="1.0">
   <dict>
     <key>Label</key>
     <string>com.office-automate.occupancy</string>
     <key>ProgramArguments</key>
     <array>
       <string>/opt/homebrew/bin/python3</string>
       <string>/Users/rajesh/Desktop/office-automate/occupancy_detector.py</string>
       <string>--watch</string>
       <string>--url</string>
       <string>http://bakasura4.local:8080</string>
       <string>--auth-username</string>
       <string>admin</string>
       <string>--auth-password</string>
       <string>your_password</string>
     </array>
     <key>RunAtLoad</key>
     <true/>
     <key>KeepAlive</key>
     <true/>
   </dict>
   </plist>
   EOF

   launchctl load ~/Library/LaunchAgents/com.office-automate.occupancy.plist
   ```

### Usage & Remote Access

**Local Network Access:**
- Dashboard: `http://bakasura4.local:8080`
- Direct access from any device on home network

**Remote Access (via LocalTunnel):**
- URL: `https://climate.loca.lt`
- First access: Enter tunnel password (get from Mac Mini: `curl https://loca.lt/mytunnelpassword`)
- After IP whitelisting: HTTP Basic Auth (username: `rajesh`, password in config.yaml)
- **Note:** LocalTunnel subdomain is first-come-first-served. If lost, check logs: `ssh rajesh@bakasura4.local "tail /tmp/localtunnel.log"`

**Install PWA on iPhone:**
1. Visit `https://climate.loca.lt` in Safari
2. Enter LocalTunnel password (one-time per IP)
3. Login with HTTP Basic Auth
4. Tap Share button → "Add to Home Screen"
5. Name it "Climate"

**Remote Management:**
```bash
# SSH into Mac Mini
ssh rajesh@bakasura4.local

# Check services status
launchctl list | grep office-automate

# View logs
tail -f /tmp/office-automate.error.log
tail -f /tmp/mqtt.log
tail -f /tmp/localtunnel.log

# Restart services
launchctl unload ~/Library/LaunchAgents/com.office-automate.orchestrator.plist
launchctl load ~/Library/LaunchAgents/com.office-automate.orchestrator.plist

# Deploy code updates from work Mac
scp ~/Desktop/office-automate/src/orchestrator.py rajesh@bakasura4.local:~/office-automate/src/
ssh rajesh@bakasura4.local "launchctl unload ~/Library/LaunchAgents/com.office-automate.orchestrator.plist && launchctl load ~/Library/LaunchAgents/com.office-automate.orchestrator.plist"

# Database queries
ssh rajesh@bakasura4.local "sqlite3 ~/office-automate/data/office_climate.db 'SELECT * FROM occupancy_log ORDER BY timestamp DESC LIMIT 10'"
```
