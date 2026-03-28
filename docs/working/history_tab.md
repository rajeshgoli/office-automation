# History Tab — Spec

## Context

The app is being redesigned from a single-page dashboard to a two-tab app: **Dashboard** and **History**. The History tab surfaces patterns and trends from data already collected in SQLite — occupancy sessions, CO2 levels, door events, and climate actions. Think "sleep tracking but for office productivity."

## Sections

### 1. Office Sessions Timeline (hero section)

Horizontal bar chart showing arrival/departure times per day, like Fitbit's sleep timeline.

```
THIS WEEK
Mon  ·········████████████···  8:42a – 5:15p  (8.5h)
Tue  ··········███████████····  9:10a – 5:45p  (8.6h)
Wed  ·········████████·······  8:30a – 3:20p  (6.8h)
Thu  ···········██████████···  9:45a – 5:30p  (7.7h)
Fri  ·········████████████··  8:15a – 5:00p  (8.7h)

AVG ARRIVAL    8:52a  (± 34min)
AVG DEPARTURE  5:05p  (± 42min)
AVG SESSION    8.1h
```

- **Time axis:** Fixed 6:00a–10:00p so bars are visually comparable across days
- **Gaps:** Mid-day departures (lunch, errands) show as breaks in the bar
- **Summary stats below:** Average arrival ± std dev, average departure ± std dev, average session duration, total hours this week
- **Data source:** `occupancy_log` table — first PRESENT per day = arrival, last PRESENT→AWAY = departure

### 2. CO2 OHLC Chart

Candlestick chart for CO2 with adaptive resolution based on selected time range.

| Time Range | Candle Width | ~Candle Count |
|-----------|-------------|---------------|
| 1 hour | 5 min | 12 |
| 6 hours | 15 min | 24 |
| 1 day | 1 hour | 24 |
| 1 week | 4 hours | 42 |

Each candle: Open (first reading in bucket), High (max), Low (min), Close (last reading in bucket).

- **Green candle:** Close < Open (CO2 dropped during period)
- **Red candle:** Close > Open (CO2 rose during period)
- **Reference lines:** 800 ppm target (green dashed), 2000 ppm critical (red dashed)
- **Time range selector:** Buttons for 1h, 6h, 1d, 1w
- **Summary row:** Average, Peak, Low for selected period
- **Data source:** `sensor_readings` grouped into time buckets

### 3. Daily Stats

Stat cards for the current day / selected range:

- **Presence hours:** Total hours PRESENT today
- **Door events:** Count of open/close events (from `device_events` where device_type='door')
- **Vent runtime:** Total ERV on-time (from `climate_actions` where system='erv')
- **HVAC runtime:** Total heating/cooling time (from `climate_actions` where system='hvac')

### 4. Activity Log (Extended)

Same format as the dashboard activity log but with more entries and scrollable. Occupancy changes, ERV actions, door events. No cap on visible entries.

---

## Backend API

### `GET /history/sessions?days=7`

```json
{
  "ok": true,
  "sessions": [
    {
      "date": "2026-03-27",
      "arrival": "08:42:00",
      "departure": "17:15:00",
      "duration_hours": 8.55,
      "gaps": [{"left": "12:15:00", "returned": "12:48:00", "duration_min": 33}]
    }
  ],
  "summary": {
    "avg_arrival": "08:52:00",
    "avg_departure": "17:05:00",
    "avg_duration_hours": 8.1,
    "std_arrival_min": 34,
    "std_departure_min": 42,
    "total_hours_week": 28.4
  }
}
```

### `GET /history/co2-ohlc?hours=24`

Bucket size auto-calculated from hours (5min/15min/1hr/4hr).

```json
{
  "ok": true,
  "bucket_minutes": 60,
  "candles": [
    {
      "timestamp": "2026-03-27T08:00:00",
      "open": 485, "high": 612, "low": 470, "close": 590,
      "avg": 540, "readings": 120
    }
  ]
}
```

### `GET /history/daily-stats?days=7`

```json
{
  "ok": true,
  "stats": [
    {
      "date": "2026-03-27",
      "door_events": 8,
      "erv_runtime_min": 134,
      "hvac_runtime_min": 270,
      "presence_hours": 8.5
    }
  ]
}
```

---

## Files to Modify

| File | Change |
|------|--------|
| `src/database.py` | Add `get_office_sessions()`, `get_co2_ohlc()`, `get_daily_stats()` |
| `src/orchestrator.py` | Add 3 new `/history/*` endpoints |
| `frontend/api.ts` | Add `fetchSessions()`, `fetchCO2OHLC()`, `fetchDailyStats()` |
| `frontend/History.tsx` | **New** — History tab with all 4 sections |
| `frontend/App.tsx` | Add tab navigation, render History when on History tab |
| `frontend/types.ts` | Add TypeScript types for new API responses |

## Verification

1. Hit each new endpoint with curl, verify response shape
2. Spot-check a day's sessions against raw `occupancy_log` in SQLite
3. Visual check — session bars, OHLC candles, daily stats all render correctly
4. Change OHLC time range → verify bucket size adapts
