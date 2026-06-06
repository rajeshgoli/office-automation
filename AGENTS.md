Read .agent-os/agents.md for workflow instructions and persona definitions.

# Office Climate Automation

Office climate automation system for a backyard shed office. Coordinates smart devices to maintain air quality silently during occupancy and aggressively ventilate when away.

## Key Components

- `rust/office-automate-server/` - Rust server, collectors, device clients, and CLI
- `rust/office-automate-server/src/http.rs` - HTTP/WS server + OAuth endpoints
- `rust/office-automate-server/src/state.rs` - PRESENT/AWAY state logic
- `rust/office-automate-server/src/erv.rs` - ERV control via Tuya local
- `rust/office-automate-server/src/hvac.rs` - Mitsubishi Kumo Cloud (HVAC)
- `rust/office-automate-server/src/yolink.rs` - YoLink cloud API (sensors)
- `rust/office-automate-server/src/qingping.rs` - Qingping MQTT client (air quality)
- `frontend/` - React + Vite dashboard
- `android/` - Android client

## Development

```bash
# Backend
cargo run --manifest-path rust/office-automate-server/Cargo.toml -- serve --config config.yaml

# Frontend
cd frontend
VITE_API_PORT=8080 npm run dev -- --port 9002
```

## Database

SQLite at `data/office_climate.db`. Tables: `sensor_readings`, `occupancy_log`, `device_events`, `climate_actions`. All timestamps in local time (PST).

## Two-State System

- **PRESENT**: Quiet mode - ERV off unless CO2 > 2000 ppm or tVOC > 250
- **AWAY**: Adaptive ventilation - 30 min TURBO then adaptive based on CO2 fall rate
