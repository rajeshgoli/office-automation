# Dashboard Handoff

> **Status: COMPLETED** - Dashboard is wired to live data. See Session 3 in roadmap.md.

## What Was Done

Wired the dashboard template to live data from the orchestrator backend.

## Template Location

```
/Users/rajesh/Desktop/office-automate/tmp/office-climate-dashboard/
```

This is a working React + Vite + TypeScript app with:
- Dark theme, responsive layout
- Hero section with CO2 as primary metric
- Vital tiles (temp, humidity, door, window, HVAC, vent status)
- CO2 timeline chart (recharts)
- Activity log
- Ambient mode (full-screen display)
- System health footer

**To run the template:**
```bash
cd tmp/office-climate-dashboard
npm install
npm run dev
```

## What Needs To Be Done

### 1. Move Template to `frontend/`

Move `tmp/office-climate-dashboard/*` to `frontend/` and integrate with the backend.

### 2. Replace Mock Data with Live API

The template uses `mockData.ts`. Replace with real data from the orchestrator.

**Backend endpoint (already exists):**
```
GET http://localhost:8080/status
```

**Returns:**
```json
{
  "state": "present",
  "is_present": true,
  "safety_interlock": false,
  "erv_should_run": false,
  "sensors": {
    "mac_last_active": 1704567890.123,
    "external_monitor": true,
    "motion_detected": false,
    "door_open": false,
    "window_open": false,
    "co2_ppm": 650
  },
  "air_quality": {
    "co2_ppm": 650,
    "temp_c": 22.5,
    "humidity": 48,
    "pm25": 12,
    "last_update": "2026-01-05T16:30:00"
  },
  "erv": {
    "running": false
  }
}
```

### 3. Add WebSocket for Real-Time Updates

Currently the backend only has REST. Add WebSocket support to push updates:

**In `src/orchestrator.py`:**
- Add `aiohttp` WebSocket endpoint at `/ws`
- Broadcast state changes to connected clients
- Send updates when: CO2 changes, state changes, door/window events

**In frontend:**
- Connect to `ws://localhost:8080/ws`
- Update state on message received
- Show connection status in footer

### 4. Wire Up Activity Log

The backend logs events but doesn't expose them. Add:
- Event history endpoint: `GET /events?limit=50`
- Store events in memory (last 100)
- Event types: state changes, ERV on/off, door/window, CO2 thresholds

### 5. Convert Temperature

Backend returns Celsius, template shows Fahrenheit. Add conversion:
```typescript
const toFahrenheit = (c: number) => Math.round(c * 9/5 + 32);
```

## Key Files in Template

| File | Purpose |
|------|---------|
| `App.tsx` | Main dashboard layout, state management |
| `types.ts` | TypeScript interfaces (OfficeState, etc.) |
| `constants.tsx` | Status configs (colors, labels, icons) |
| `mockData.ts` | **Replace this with API calls** |
| `components/VitalTile.tsx` | Individual metric card |
| `components/CO2Chart.tsx` | Timeline chart using recharts |

## Backend Reference

| Component | Status | Notes |
|-----------|--------|-------|
| Orchestrator | Running on `:8080` | REST API ready |
| Qingping MQTT | Connected | Data every 15 min |
| ERV (Tuya) | Working | Local control |
| YoLink | Connected | Real-time events |
| Mitsubishi | Not wired | Client exists |

## Thresholds (from config.yaml)

```yaml
co2_critical_ppm: 2000    # ERV on while present
co2_refresh_target_ppm: 500   # Target when away
motion_timeout_seconds: 60
```

## Success Criteria

1. Dashboard loads and shows live CO2, temp, humidity
2. State changes reflected in real-time (PRESENT/AWAY)
3. Door/window status updates immediately
4. ERV status shows on/off
5. CO2 chart shows last 24 hours of data
6. Activity log shows recent events
7. System health footer shows device connectivity

## Optional Enhancements

- Add manual ERV override button
- Show Qingping data age ("Updated 3 min ago")
- Add PM2.5 tile
- Historical data beyond 24h (would need persistence)
- Mobile-optimized layout

## Starting the System

```bash
# Terminal 1: Start MQTT broker
brew services start mosquitto

# Terminal 2: Start orchestrator
cd /Users/rajesh/Desktop/office-automate
source venv/bin/activate
python run.py

# Terminal 3: Start dashboard
cd frontend
npm run dev
```

## Questions?

Check these files for context:
- `CLAUDE.md` - System overview and current status
- `roadmap.md` - Full session history
- `src/orchestrator.py` - Backend implementation
- `src/state_machine.py` - State logic
