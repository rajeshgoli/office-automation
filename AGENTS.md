Read .agent-os/agents.md for workflow instructions and persona definitions.

# Office Climate Automation

Office climate automation system for a backyard shed office. Coordinates smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.

## Key Components

- `run.py` - Main entry point
- `src/orchestrator.py` - Main coordinator + HTTP/WS server + OAuth endpoints
- `src/state_machine.py` - PRESENT/AWAY state logic
- `src/erv_client.py` - ERV control via Tuya local
- `src/kumo_client.py` - Mitsubishi Kumo Cloud (HVAC)
- `src/yolink_client.py` - YoLink cloud API (sensors)
- `src/qingping_client.py` - Qingping MQTT client (air quality)
- `frontend/` - React + Vite dashboard

## Development

```bash
# Backend
source venv/bin/activate
python run.py --port 9001

# Frontend
cd frontend
VITE_API_PORT=9001 npm run dev -- --port 9002
```

## Database

SQLite at `data/office_climate.db`. Tables: `sensor_readings`, `occupancy_log`, `device_events`, `climate_actions`. All timestamps in local time (PST).

## Two-State System

- **PRESENT**: Quiet mode - ERV off unless CO2 > 2000 ppm or tVOC > 250
- **AWAY**: Adaptive ventilation - 30 min TURBO then adaptive based on CO2 fall rate
