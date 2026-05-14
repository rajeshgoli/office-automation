
import React, { useState, useEffect, useCallback, useRef } from 'react';
import {
  OfficeState,
  OccupancyState,
  DeviceStatus,
  ERVSpeed,
  ActivityEvent,
  ClimateDataPoint,
  Threshold
} from './types';
import { STATUS_CONFIG } from './constants';
import VitalTile from './components/VitalTile';
import CO2Chart from './components/CO2Chart';
import { fetchStatus, ApiStatus, toFahrenheit, StatusWebSocket, setERVSpeed, setHVACMode, setPresence, setHVACTemperatureBands, ERVSpeed as ApiERVSpeed, HVACMode, HVACTemperatureBands, isAuthenticated, logout, getUserEmail, checkTrustedNetwork } from './api';
import Login from './Login';
import HistoricalCharts from './HistoricalCharts';
import OfficeReplay from './OfficeReplay';

// Default state when no data available
const DEFAULT_STATE: OfficeState = {
  co2: 0,
  temperature: 0,
  tempTrend: 'stable',
  humidity: 0,
  tvoc: 0,
  noise: 0,
  pm25: 0,
  pm10: 0,
  door: DeviceStatus.CLOSED,
  window: DeviceStatus.CLOSED,
  hvacMode: DeviceStatus.OFF,
  hvacTarget: 70,
  ventMode: ERVSpeed.OFF,
  occupancy: OccupancyState.AWAY,
  lastUpdated: new Date(),
  isSystemError: true
};

const DEFAULT_TEMPERATURE_BANDS: HVACTemperatureBands = {
  heat_on_temp_f: 71,
  heat_off_temp_f: 75,
  cool_off_temp_f: 78,
  cool_on_temp_f: 81,
};

const TEMPERATURE_BAND_LIMITS: Record<keyof HVACTemperatureBands, { min: number; max: number }> = {
  heat_on_temp_f: { min: 45, max: 85 },
  heat_off_temp_f: { min: 46, max: 90 },
  cool_off_temp_f: { min: 55, max: 95 },
  cool_on_temp_f: { min: 56, max: 100 },
};

// Map API ERV speed to display speed
function mapERVSpeed(apiSpeed: string | undefined): ERVSpeed {
  switch (apiSpeed) {
    case 'quiet': return ERVSpeed.QUIET;
    case 'medium': return ERVSpeed.ELEVATED;
    case 'turbo': return ERVSpeed.PURGE;
    case 'off': return ERVSpeed.OFF;
    default: return ERVSpeed.OFF;
  }
}

// Convert API response to OfficeState
function apiToState(api: ApiStatus): OfficeState {
  return {
    co2: api.air_quality.co2_ppm ?? api.sensors.co2_ppm ?? 0,
    temperature: api.air_quality.temp_c ? toFahrenheit(api.air_quality.temp_c) : 0,
    tempTrend: 'stable',
    humidity: api.air_quality.humidity ? Math.round(api.air_quality.humidity * 10) / 10 : 0,
    tvoc: api.air_quality.tvoc ?? 0,
    noise: api.air_quality.noise_db ?? 0,
    pm25: api.air_quality.pm25 ?? 0,
    pm10: api.air_quality.pm10 ?? 0,
    door: api.sensors.door_open ? DeviceStatus.OPEN : DeviceStatus.CLOSED,
    window: api.sensors.window_open ? DeviceStatus.OPEN : DeviceStatus.CLOSED,
    hvacMode: api.hvac?.mode === 'heat' ? DeviceStatus.HEAT :
              api.hvac?.mode === 'cool' ? DeviceStatus.COOL :
              api.hvac?.mode === 'off' ? DeviceStatus.OFF : DeviceStatus.OFF,
    hvacTarget: api.hvac?.setpoint_c ? toFahrenheit(api.hvac.setpoint_c) : 70,
    ventMode: mapERVSpeed(api.erv.speed),
    occupancy: api.is_present ? OccupancyState.PRESENT : OccupancyState.AWAY,
    lastUpdated: api.air_quality.last_update ? new Date(api.air_quality.last_update) : new Date(),
    isSystemError: false
  };
}

const App: React.FC = () => {
  const [state, setState] = useState<OfficeState>(DEFAULT_STATE);
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [history, setHistory] = useState<ClimateDataPoint[]>([]);
  const [currentTime, setCurrentTime] = useState(new Date());
  const [isAmbient, setIsAmbient] = useState(false);
  const [wsConnected, setWsConnected] = useState(false);
  const [apiConnected, setApiConnected] = useState(false);
  const [temperatureBands, setTemperatureBands] = useState<HVACTemperatureBands>(DEFAULT_TEMPERATURE_BANDS);
  const [manualOverride, setManualOverride] = useState<{
    erv: boolean;
    erv_speed: string | null;
    erv_expires_in: number | null;
    hvac: boolean;
    hvac_mode: string | null;
    hvac_setpoint_f: number | null;
    hvac_expires_in: number | null;
  } | null>(null);
  const [controlLoading, setControlLoading] = useState<string | null>(null);
  const [authenticated, setAuthenticated] = useState(false); // Start false, check on load
  const [authChecking, setAuthChecking] = useState(true); // Always check on load
  const [userEmail, setUserEmail] = useState<string | null>(null);

  const wsRef = useRef<StatusWebSocket | null>(null);
  const historyRef = useRef<ClimateDataPoint[]>([]);
  const initialLoadRef = useRef(true); // Track first load to skip spurious events
  const lastOccupancyRef = useRef<OccupancyState | null>(null); // Track real occupancy changes

  // Update history with new CO2 reading
  const addHistoryPoint = useCallback((co2: number, occupancy: OccupancyState, venting: boolean) => {
    const now = new Date();
    const point: ClimateDataPoint = {
      time: now.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
      co2,
      occupancy,
      venting
    };

    historyRef.current = [...historyRef.current, point].slice(-96); // Keep last 48 hours at 30min intervals
    setHistory(historyRef.current);
  }, []);

  // Add event to log
  const addEvent = useCallback((type: string, message: string, co2?: number) => {
    const event: ActivityEvent = {
      id: Date.now().toString(),
      timestamp: new Date(),
      type,
      message,
      co2
    };
    setEvents(prev => [...prev.slice(-19), event]); // Keep last 20 events
  }, []);

  // Handle logout
  const handleLogout = async () => {
    await logout();
    setAuthenticated(false);
    setUserEmail(null);
  };

  // Check authentication on load - try trusted network first, then token
  useEffect(() => {
    if (!authChecking) return;

    checkTrustedNetwork().then((isTrusted) => {
      if (isTrusted) {
        // Trusted network - no token needed
        setAuthenticated(true);
        setUserEmail('Local Network');
        setAuthChecking(false);
      } else if (isAuthenticated()) {
        // Not trusted network, but have a token - use it
        setAuthenticated(true);
        setUserEmail(getUserEmail());
        setAuthChecking(false);
      } else {
        // No trusted network, no token - need login
        setAuthChecking(false);
      }
    });
  }, [authChecking]);

  // Fetch initial status
  useEffect(() => {
    // Skip if not authenticated
    if (!authenticated) return;

    let mounted = true;
    let pollInterval: number | null = null;

    const loadStatus = async () => {
      try {
        const status = await fetchStatus();
        if (mounted) {
          const newState = apiToState(status);
          // Update manual override state
          if (status.manual_override) {
            setManualOverride(status.manual_override);
          }
          if (status.hvac?.temperature_bands) {
            setTemperatureBands(status.hvac.temperature_bands);
          }
          setState(prev => {
            // Only log events after initial load (avoid spurious "Arrived" on page load)
            if (!initialLoadRef.current) {
              // Only log occupancy if it actually changed from last known state
              if (lastOccupancyRef.current !== null && lastOccupancyRef.current !== newState.occupancy) {
                addEvent('state', `${newState.occupancy === OccupancyState.PRESENT ? 'Arrived' : 'Away'} - CO2 ${newState.co2} ppm`, newState.co2);
              }
              if (prev.ventMode !== newState.ventMode) {
                addEvent('erv', `ERV ${newState.ventMode}`, newState.co2);
              }
              if (prev.door !== newState.door) {
                addEvent('door', `Door ${newState.door === DeviceStatus.OPEN ? 'opened' : 'closed'}`);
              }
              if (prev.window !== newState.window) {
                addEvent('window', `Window ${newState.window === DeviceStatus.OPEN ? 'opened' : 'closed'}`);
              }
            }
            initialLoadRef.current = false;
            lastOccupancyRef.current = newState.occupancy;
            return newState;
          });
          setApiConnected(true);

          // Add to history every update
          addHistoryPoint(
            status.air_quality.co2_ppm ?? status.sensors.co2_ppm ?? 0,
            status.is_present ? OccupancyState.PRESENT : OccupancyState.AWAY,
            status.erv.running
          );
        }
      } catch (e) {
        console.error('Failed to fetch status:', e);
        if (mounted) {
          if (e instanceof Error && e.message === 'Authentication required') {
            setAuthenticated(false);
            setUserEmail(null);
            return;
          }
          setApiConnected(false);
          setState(prev => ({ ...prev, isSystemError: true }));
        }
      }
    };

    // Initial load
    loadStatus();

    // Poll every 5 seconds as fallback (WebSocket is primary)
    pollInterval = window.setInterval(loadStatus, 5000);

    return () => {
      mounted = false;
      if (pollInterval) clearInterval(pollInterval);
    };
  }, [authenticated, addHistoryPoint, addEvent]);

  // WebSocket connection
  useEffect(() => {
    // Skip if not authenticated
    if (!authenticated) return;

    const ws = new StatusWebSocket();
    wsRef.current = ws;

    ws.onConnection(setWsConnected);
    ws.onStatus((status) => {
      const newState = apiToState(status);
      // Update manual override state
      if (status.manual_override) {
        setManualOverride(status.manual_override);
      }
      if (status.hvac?.temperature_bands) {
        setTemperatureBands(status.hvac.temperature_bands);
      }
      setState(prev => {
        // Only log real changes (compare against last known occupancy, not stale state)
        if (lastOccupancyRef.current !== null && lastOccupancyRef.current !== newState.occupancy) {
          addEvent('state', `${newState.occupancy === OccupancyState.PRESENT ? 'Arrived' : 'Away'} - CO2 ${newState.co2} ppm`, newState.co2);
        }
        if (prev.ventMode !== newState.ventMode && !initialLoadRef.current) {
          addEvent('erv', `ERV ${newState.ventMode}`, newState.co2);
        }
        lastOccupancyRef.current = newState.occupancy;
        return newState;
      });
      setApiConnected(true);

      addHistoryPoint(
        status.air_quality.co2_ppm ?? status.sensors.co2_ppm ?? 0,
        status.is_present ? OccupancyState.PRESENT : OccupancyState.AWAY,
        status.erv.running
      );
    });

    ws.connect();

    return () => {
      ws.disconnect();
    };
  }, [authenticated, addHistoryPoint, addEvent]);

  // Clock update
  useEffect(() => {
    const timer = setInterval(() => {
      setCurrentTime(new Date());
    }, 1000);
    return () => clearInterval(timer);
  }, []);

  const getPrimaryStatus = useCallback(() => {
    if (state.isSystemError) return STATUS_CONFIG.ERROR;

    // Open air shows presence status too
    if (state.window === DeviceStatus.OPEN || state.door === DeviceStatus.OPEN) {
      return state.occupancy === OccupancyState.PRESENT
        ? STATUS_CONFIG.OPEN_AIR_PRESENT
        : STATUS_CONFIG.OPEN_AIR_AWAY;
    }

    if (state.occupancy === OccupancyState.PRESENT) {
      if (state.co2 > Threshold.CRITICAL) return STATUS_CONFIG.PRESENT_VENTING;
      if (state.co2 > Threshold.ELEVATED) return STATUS_CONFIG.PRESENT_ELEVATED;
      return STATUS_CONFIG.PRESENT_QUIET;
    } else {
      if (state.ventMode !== ERVSpeed.OFF) return STATUS_CONFIG.AWAY_CLEARING;
      return STATUS_CONFIG.AWAY_CLEAR;
    }
  }, [state]);

  // Control button handlers (must be before early returns for hook consistency)
  const handleERVControl = useCallback(async (speed: ApiERVSpeed) => {
    setControlLoading(`erv-${speed}`);
    try {
      const result = await setERVSpeed(speed);
      if (!result.ok) {
        console.error('ERV control failed:', result.error);
      } else {
        addEvent('erv', `Manual: ERV ${speed.toUpperCase()}`, state.co2);
      }
    } catch (e) {
      console.error('ERV control error:', e);
    } finally {
      setControlLoading(null);
    }
  }, [addEvent, state.co2]);

  const handleHVACControl = useCallback(async (mode: HVACMode, setpoint_f: number = 70) => {
    setControlLoading(`hvac-${mode}`);
    try {
      const result = await setHVACMode(mode, setpoint_f);
      if (!result.ok) {
        console.error('HVAC control failed:', result.error);
      } else {
        const label = mode === 'off' ? 'OFF' : `${mode.toUpperCase()} ${setpoint_f}°F`;
        addEvent('hvac', `Manual: HVAC ${label}`);
      }
    } catch (e) {
      console.error('HVAC control error:', e);
    } finally {
      setControlLoading(null);
    }
  }, [addEvent]);

  const handlePresenceToggle = useCallback(async () => {
    const requestedState = state.occupancy === OccupancyState.PRESENT ? 'away' : 'present';
    setControlLoading('presence');
    try {
      const result = await setPresence(requestedState);
      if (!result.ok) {
        console.error('Presence control failed:', result.error);
      } else {
        addEvent('state', `Manual: ${requestedState === 'present' ? "I'm here" : "I'm away"}`, state.co2);
      }
    } catch (e) {
      console.error('Presence control error:', e);
    } finally {
      setControlLoading(null);
    }
  }, [addEvent, state.co2, state.occupancy]);

  const clampTemperatureBand = useCallback((key: keyof HVACTemperatureBands, value: number) => {
    const limit = TEMPERATURE_BAND_LIMITS[key];
    return Math.max(limit.min, Math.min(limit.max, value));
  }, []);

  const updateTemperatureBand = useCallback((band: 'heat' | 'cool', action: 'shift' | 'spread', delta: number) => {
    const previousBands = temperatureBands;
    const nextBands: HVACTemperatureBands = { ...temperatureBands };
    const lowerKey: keyof HVACTemperatureBands = band === 'heat' ? 'heat_on_temp_f' : 'cool_off_temp_f';
    const upperKey: keyof HVACTemperatureBands = band === 'heat' ? 'heat_off_temp_f' : 'cool_on_temp_f';

    if (action === 'shift') {
      nextBands[lowerKey] += delta;
      nextBands[upperKey] += delta;
    } else {
      nextBands[lowerKey] -= delta;
      nextBands[upperKey] += delta;
    }

    nextBands[lowerKey] = clampTemperatureBand(lowerKey, nextBands[lowerKey]);
    nextBands[upperKey] = clampTemperatureBand(upperKey, nextBands[upperKey]);

    if (nextBands[upperKey] <= nextBands[lowerKey]) {
      nextBands[upperKey] = clampTemperatureBand(upperKey, nextBands[lowerKey] + 1);
      nextBands[lowerKey] = clampTemperatureBand(lowerKey, nextBands[upperKey] - 1);
    }

    setTemperatureBands(nextBands);
    setControlLoading(`bands-${band}`);

    setHVACTemperatureBands(nextBands).then((result) => {
      if (!result.ok || !result.temperature_bands) {
        console.error('HVAC temperature band update failed:', result.error);
        setTemperatureBands(previousBands);
      } else {
        setTemperatureBands(result.temperature_bands);
        addEvent(
          'hvac',
          `Bands: heat ${result.temperature_bands.heat_on_temp_f}-${result.temperature_bands.heat_off_temp_f}°F, cool ${result.temperature_bands.cool_off_temp_f}-${result.temperature_bands.cool_on_temp_f}°F`
        );
      }
    }).catch((e) => {
      console.error('HVAC temperature band update error:', e);
      setTemperatureBands(previousBands);
    }).finally(() => {
      setControlLoading(null);
    });
  }, [addEvent, clampTemperatureBand, temperatureBands]);

  const resetTemperatureBands = useCallback(() => {
    const previousBands = temperatureBands;
    setTemperatureBands(DEFAULT_TEMPERATURE_BANDS);
    setControlLoading('bands-reset');

    setHVACTemperatureBands(DEFAULT_TEMPERATURE_BANDS).then((result) => {
      if (!result.ok || !result.temperature_bands) {
        console.error('HVAC temperature band reset failed:', result.error);
        setTemperatureBands(previousBands);
      } else {
        setTemperatureBands(result.temperature_bands);
        addEvent('hvac', 'Bands reset to defaults');
      }
    }).catch((e) => {
      console.error('HVAC temperature band reset error:', e);
      setTemperatureBands(previousBands);
    }).finally(() => {
      setControlLoading(null);
    });
  }, [addEvent, temperatureBands]);

  const currentStatus = getPrimaryStatus();

  // Show loading while checking trusted network
  if (authChecking) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-black">
        <div className="text-zinc-500">Checking connection...</div>
      </div>
    );
  }

  // Show login screen if not authenticated
  if (!authenticated) {
    return <Login />;
  }

  const getCo2Color = (ppm: number) => {
    if (ppm < Threshold.NORMAL) return 'text-emerald-400';
    if (ppm < Threshold.ELEVATED) return 'text-yellow-400';
    if (ppm < Threshold.CRITICAL) return 'text-orange-400';
    return 'text-red-500';
  };

  // Connection status indicator component
  const ConnectionDot: React.FC<{ connected: boolean; label: string }> = ({ connected, label }) => (
    <div className="flex items-center gap-2">
      <div className={`w-1.5 h-1.5 rounded-full ${connected ? 'bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.5)]' : 'bg-red-500 shadow-[0_0_8px_rgba(239,68,68,0.5)]'}`} />
      <span className="text-[10px] font-bold text-zinc-500 uppercase tracking-widest">{label}</span>
    </div>
  );

  if (isAmbient) {
    return (
      <div
        className="fixed inset-0 bg-black flex flex-col items-center justify-center cursor-pointer select-none transition-all duration-1000"
        onClick={() => setIsAmbient(false)}
      >
        <div className={`text-4xl font-bold mb-4 ${currentStatus.text}`}>
          {currentStatus.icon} {currentStatus.label}
        </div>
        <div className={`text-9xl font-black mb-8 ${getCo2Color(state.co2)}`}>
          {state.co2} <span className="text-3xl text-zinc-700 font-medium">ppm</span>
        </div>
        <div className="flex gap-12 text-zinc-400">
          <div className="flex flex-col items-center">
            <span className="text-4xl font-bold text-zinc-200">{state.temperature}°F</span>
            <span className="text-sm uppercase tracking-widest text-zinc-600">Temp</span>
          </div>
          <div className="flex flex-col items-center">
            <span className="text-4xl font-bold text-zinc-200">{state.humidity}%</span>
            <span className="text-sm uppercase tracking-widest text-zinc-600">Humidity</span>
          </div>
          <div className="flex flex-col items-center">
            <span className="text-4xl font-bold text-zinc-200">{state.ventMode !== ERVSpeed.OFF ? '🌀' : '❄️'}</span>
            <span className="text-sm uppercase tracking-widest text-zinc-600">{state.ventMode !== ERVSpeed.OFF ? state.ventMode : 'Cool'}</span>
          </div>
        </div>
        <div className="absolute bottom-8 text-zinc-800 font-mono">
          {currentTime.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen p-4 md:p-8 max-w-5xl mx-auto flex flex-col gap-6 select-none">
      {/* Header */}
      <header className="flex justify-between items-center mb-2">
        <h1 className="text-lg font-bold tracking-tighter text-zinc-400 uppercase">Office Climate</h1>
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-3">
            <span className="text-sm text-zinc-400">{userEmail}</span>
            <button
              onClick={handleLogout}
              className="text-xs bg-zinc-800 hover:bg-zinc-700 text-zinc-300 px-3 py-1 rounded transition-colors"
            >
              Logout
            </button>
          </div>
          <button
            onClick={() => setIsAmbient(true)}
            className="text-[10px] border border-zinc-800 px-2 py-1 rounded text-zinc-600 hover:text-zinc-400 transition-colors uppercase font-bold"
          >
            Ambient Mode
          </button>
          <div className="text-2xl font-mono font-medium text-zinc-200">
            {currentTime.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
          </div>
        </div>
      </header>

      {/* Hero Section */}
      <section
        className={`relative bg-zinc-900/50 rounded-3xl border-2 p-10 flex flex-col items-center justify-center transition-all duration-500 overflow-hidden ${currentStatus.border}`}
      >
        <div className={`text-sm font-bold tracking-[0.2em] uppercase mb-4 ${currentStatus.text}`}>
          Primary Status
        </div>
        <div className={`text-4xl md:text-5xl font-black mb-6 flex items-center gap-4 ${currentStatus.text}`}>
          <span>{currentStatus.icon}</span>
          <span>{currentStatus.label}</span>
        </div>
        <div className="flex flex-col items-center">
          <div className={`text-8xl md:text-[10rem] leading-none font-black tracking-tighter transition-colors duration-500 ${getCo2Color(state.co2)}`}>
            {state.co2 || '---'}
          </div>
          <div className="text-xl md:text-2xl font-bold text-zinc-600 tracking-[0.3em] uppercase -mt-4">
            parts per million
          </div>
        </div>

        {/* Pulsing effect for critical status */}
        {state.co2 > Threshold.CRITICAL && (
          <div className="absolute inset-0 border-4 border-red-500/20 animate-pulse pointer-events-none rounded-3xl" />
        )}
      </section>

      {/* Vitals Grid */}
      <section className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <VitalTile
          label="Temperature"
          value={state.temperature || '---'}
          unit="°F"
          icon="🌡️"
        />
        <VitalTile
          label="Humidity"
          value={state.humidity || '---'}
          unit="%"
          icon="💧"
        />
        <VitalTile
          label="tVOC"
          value={state.tvoc || '---'}
          unit=""
          icon="🌫️"
        />
        <VitalTile
          label="Noise"
          value={state.noise || '---'}
          unit="dB"
          icon="🔊"
        />
        <VitalTile
          label="PM2.5"
          value={state.pm25 ?? '---'}
          unit="µg/m³"
          icon="💨"
        />
        <VitalTile
          label="PM10"
          value={state.pm10 ?? '---'}
          unit="µg/m³"
          icon="💨"
        />
        <VitalTile
          label="Door"
          value={state.door}
          icon="🚪"
          status={state.door}
          attention={state.door === DeviceStatus.OPEN}
        />
        <VitalTile
          label="Window"
          value={state.window}
          icon="🪟"
          status={state.window}
          attention={state.window === DeviceStatus.OPEN}
        />
        <VitalTile
          label="HVAC"
          value={`${state.hvacMode} ${state.hvacTarget}°`}
          icon={state.hvacMode === DeviceStatus.COOL ? '❄️' : state.hvacMode === DeviceStatus.HEAT ? '🔥' : '⏸️'}
          status={state.hvacMode}
        />
        <VitalTile
          label="Vent"
          value={state.ventMode}
          icon="🌀"
          status={state.ventMode}
        />
      </section>

      {/* Quick Controls */}
      <section className="bg-zinc-900/30 border border-zinc-800/50 rounded-3xl p-6">
        <div className="flex justify-between items-center mb-4">
          <h2 className="text-xs font-bold uppercase tracking-widest text-zinc-500">Quick Controls</h2>
        </div>

        <div className="space-y-4">
          {/* Presence Correction */}
          <div className="flex flex-col sm:flex-row sm:items-center gap-3 rounded-2xl border border-emerald-500/20 bg-emerald-500/5 p-4">
            <div className="flex items-center gap-2 shrink-0">
              <span className="text-sm font-bold text-zinc-400">Detected presence:</span>
              <span className={`text-[10px] font-bold uppercase px-2 py-1 rounded-full ${
                state.occupancy === OccupancyState.PRESENT
                  ? 'text-emerald-300 bg-emerald-500/10'
                  : 'text-blue-300 bg-blue-500/10'
              }`}>
                {state.occupancy === OccupancyState.PRESENT ? 'Here' : 'Away'}
              </span>
            </div>
            <button
              onClick={handlePresenceToggle}
              disabled={controlLoading !== null}
              className={`flex-1 min-h-[38px] px-4 py-2 text-xs font-black uppercase rounded-lg transition-all
                ${state.occupancy === OccupancyState.PRESENT
                  ? 'bg-blue-500/15 text-blue-200 hover:bg-blue-500/25'
                  : 'bg-emerald-500 text-black hover:bg-emerald-400'
                }
                ${controlLoading === 'presence' ? 'opacity-50 cursor-wait' : ''}
                disabled:opacity-50 disabled:cursor-not-allowed
              `}
            >
              {controlLoading === 'presence'
                ? '...'
                : state.occupancy === OccupancyState.PRESENT ? "I'm away" : "I'm here"}
            </button>
          </div>

          {/* ERV Controls */}
          <div className="rounded-2xl border border-zinc-800/70 bg-black/20 p-4 space-y-3">
            <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2">
              <div>
                <div className="text-sm font-bold text-zinc-400 uppercase tracking-wide">🌀 Ventilation</div>
                {manualOverride?.erv && manualOverride.erv_expires_in && (
                  <div className="text-xs font-semibold text-zinc-500 mt-1">
                    Manual ERV control expires in {Math.round(manualOverride.erv_expires_in / 60)}m
                  </div>
                )}
              </div>
              {manualOverride?.erv && (
                <span className="self-start sm:self-auto text-[10px] font-bold uppercase text-amber-500 bg-amber-500/10 px-2 py-1 rounded animate-pulse">
                  Manual Override Active
                </span>
              )}
            </div>
            <div className="grid grid-cols-4 gap-2">
              {(['off', 'quiet', 'medium', 'turbo'] as const).map((speed) => (
                <button
                  key={speed}
                  onClick={() => handleERVControl(speed)}
                  disabled={controlLoading !== null}
                  className={`px-3 py-2 text-xs font-bold uppercase rounded-lg transition-all
                    ${manualOverride?.erv && manualOverride.erv_speed === speed
                      ? 'bg-amber-500 text-black ring-2 ring-amber-400'
                      : 'bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200'
                    }
                    ${controlLoading === `erv-${speed}` ? 'opacity-50 cursor-wait' : ''}
                    disabled:opacity-50 disabled:cursor-not-allowed
                  `}
                >
                  {controlLoading === `erv-${speed}` ? '...' : speed}
                </button>
              ))}
            </div>
          </div>

          {/* HVAC Controls */}
          <div className="rounded-2xl border border-zinc-800/70 bg-black/20 p-4 space-y-3">
            <div className="flex flex-col sm:flex-row sm:items-center justify-between gap-2">
              <div>
                <div className="text-sm font-bold text-zinc-400 uppercase tracking-wide">🔥 HVAC</div>
                {manualOverride?.hvac && manualOverride.hvac_expires_in && (
                  <div className="text-xs font-semibold text-zinc-500 mt-1">
                    Manual HVAC control expires in {Math.round(manualOverride.hvac_expires_in / 60)}m
                  </div>
                )}
              </div>
              {manualOverride?.hvac && (
                <span className="self-start sm:self-auto text-[10px] font-bold uppercase text-amber-500 bg-amber-500/10 px-2 py-1 rounded animate-pulse">
                  Manual Override Active
                </span>
              )}
            </div>
            <div className="grid grid-cols-3 gap-2">
              <button
                onClick={() => handleHVACControl('off')}
                disabled={controlLoading !== null}
                className={`px-3 py-2 text-xs font-bold uppercase rounded-lg transition-all
                  ${manualOverride?.hvac && manualOverride.hvac_mode === 'off'
                    ? 'bg-amber-500 text-black ring-2 ring-amber-400'
                    : 'bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200'
                  }
                  ${controlLoading === 'hvac-off' ? 'opacity-50 cursor-wait' : ''}
                  disabled:opacity-50 disabled:cursor-not-allowed
                `}
              >
                {controlLoading === 'hvac-off' ? '...' : 'Off'}
              </button>
              <button
                onClick={() => handleHVACControl('heat', 70)}
                disabled={controlLoading !== null}
                className={`px-3 py-2 text-xs font-bold uppercase rounded-lg transition-all
                  ${manualOverride?.hvac && manualOverride.hvac_mode === 'heat'
                    ? 'bg-amber-500 text-black ring-2 ring-amber-400'
                    : 'bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200'
                  }
                  ${controlLoading === 'hvac-heat' ? 'opacity-50 cursor-wait' : ''}
                  disabled:opacity-50 disabled:cursor-not-allowed
                `}
              >
                {controlLoading === 'hvac-heat' ? '...' : 'Heat 70°F'}
              </button>
              <button
                onClick={() => handleHVACControl('cool', 78)}
                disabled={controlLoading !== null}
                className={`px-3 py-2 text-xs font-bold uppercase rounded-lg transition-all
                  ${manualOverride?.hvac && manualOverride.hvac_mode === 'cool'
                    ? 'bg-amber-500 text-black ring-2 ring-amber-400'
                    : 'bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200'
                  }
                  ${controlLoading === 'hvac-cool' ? 'opacity-50 cursor-wait' : ''}
                  disabled:opacity-50 disabled:cursor-not-allowed
                `}
              >
                {controlLoading === 'hvac-cool' ? '...' : 'Cool 78°F'}
              </button>
            </div>
          </div>

          {/* HVAC Temperature Bands */}
          <details className="border-t border-zinc-800/70 pt-4">
            <summary className="cursor-pointer rounded-2xl border border-zinc-800/70 bg-black/20 p-4 text-sm font-black uppercase tracking-wider text-zinc-200 flex items-center justify-between gap-3">
              <span>Hysteresis Bands</span>
              <span className="text-xs font-bold normal-case tracking-normal text-zinc-500">
                Heat {temperatureBands.heat_on_temp_f}-{temperatureBands.heat_off_temp_f}°F · Cool {temperatureBands.cool_off_temp_f}-{temperatureBands.cool_on_temp_f}°F
              </span>
            </summary>

            <div className="pt-4 space-y-3">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <h3 className="text-sm font-black uppercase tracking-wider text-zinc-300">Temperature Bands</h3>
                  <div className="text-xs font-semibold text-zinc-500 mt-1">
                    Current Qingping temp {state.temperature || '---'}°F
                  </div>
                </div>
                <button
                  onClick={resetTemperatureBands}
                  disabled={controlLoading !== null}
                  className="px-3 py-2 text-xs font-bold uppercase rounded-lg bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  Reset
                </button>
              </div>

              <div className="grid grid-cols-1 gap-3">
                <div className="grid grid-cols-1 lg:grid-cols-[minmax(180px,1fr)_minmax(0,1.5fr)] gap-4 rounded-2xl border border-rose-400/20 bg-zinc-900/30 p-4">
                  <div>
                    <div className="text-sm font-black uppercase tracking-wider text-zinc-200">Heat</div>
                    <div className="text-3xl font-black tracking-tighter text-zinc-100 mt-1">
                      {temperatureBands.heat_on_temp_f}-{temperatureBands.heat_off_temp_f}°F
                    </div>
                    <div className="text-xs font-semibold text-zinc-500 mt-1">
                      Resume at or below {temperatureBands.heat_on_temp_f}° · pause at or above {temperatureBands.heat_off_temp_f}°
                    </div>
                  </div>
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                    <div className="rounded-xl border border-zinc-800/70 bg-black/20 p-3">
                      <div className="text-[10px] font-black uppercase tracking-wider text-zinc-500 mb-2">Move Band</div>
                      <div className="grid grid-cols-2 gap-2">
                        <button onClick={() => updateTemperatureBand('heat', 'shift', -1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">-1°</button>
                        <button onClick={() => updateTemperatureBand('heat', 'shift', 1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">+1°</button>
                      </div>
                    </div>
                    <div className="rounded-xl border border-zinc-800/70 bg-black/20 p-3">
                      <div className="text-[10px] font-black uppercase tracking-wider text-zinc-500 mb-2">Band Width</div>
                      <div className="grid grid-cols-2 gap-2">
                        <button onClick={() => updateTemperatureBand('heat', 'spread', -1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">Tighter</button>
                        <button onClick={() => updateTemperatureBand('heat', 'spread', 1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">Wider</button>
                      </div>
                    </div>
                  </div>
                </div>

                <div className="grid grid-cols-1 lg:grid-cols-[minmax(180px,1fr)_minmax(0,1.5fr)] gap-4 rounded-2xl border border-sky-400/20 bg-zinc-900/30 p-4">
                  <div>
                    <div className="text-sm font-black uppercase tracking-wider text-zinc-200">Cool</div>
                    <div className="text-3xl font-black tracking-tighter text-zinc-100 mt-1">
                      {temperatureBands.cool_off_temp_f}-{temperatureBands.cool_on_temp_f}°F
                    </div>
                    <div className="text-xs font-semibold text-zinc-500 mt-1">
                      Stop at or below {temperatureBands.cool_off_temp_f}° · start above {temperatureBands.cool_on_temp_f}°
                    </div>
                  </div>
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                    <div className="rounded-xl border border-zinc-800/70 bg-black/20 p-3">
                      <div className="text-[10px] font-black uppercase tracking-wider text-zinc-500 mb-2">Move Band</div>
                      <div className="grid grid-cols-2 gap-2">
                        <button onClick={() => updateTemperatureBand('cool', 'shift', -1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">-1°</button>
                        <button onClick={() => updateTemperatureBand('cool', 'shift', 1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">+1°</button>
                      </div>
                    </div>
                    <div className="rounded-xl border border-zinc-800/70 bg-black/20 p-3">
                      <div className="text-[10px] font-black uppercase tracking-wider text-zinc-500 mb-2">Band Width</div>
                      <div className="grid grid-cols-2 gap-2">
                        <button onClick={() => updateTemperatureBand('cool', 'spread', -1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">Tighter</button>
                        <button onClick={() => updateTemperatureBand('cool', 'spread', 1)} disabled={controlLoading !== null} className="px-3 py-2 text-xs font-black uppercase rounded-lg bg-zinc-800 text-zinc-300 hover:bg-zinc-700 disabled:opacity-50">Wider</button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </details>
        </div>
      </section>

      {/* CO2 Timeline */}
      <section className="bg-zinc-900/30 border border-zinc-800/50 rounded-3xl p-6">
        <div className="flex justify-between items-center mb-4">
          <h2 className="text-xs font-bold uppercase tracking-widest text-zinc-500">CO2 Today (ppm)</h2>
          <div className="flex gap-4 text-[10px] uppercase font-bold text-zinc-600">
            <span className="flex items-center gap-1"><div className="w-2 h-2 rounded-full bg-emerald-500 opacity-50" /> Target 500</span>
            <span className="flex items-center gap-1"><div className="w-2 h-2 rounded-full bg-red-500 opacity-50" /> Critical 2000</span>
          </div>
        </div>
        <CO2Chart data={history} />
      </section>

      {/* Activity Log */}
      <section className="bg-zinc-900/30 border border-zinc-800/50 rounded-3xl p-6 mb-12">
        <h2 className="text-xs font-bold uppercase tracking-widest text-zinc-500 mb-6">Today's Activity</h2>
        <div className="space-y-4">
          {events.length === 0 ? (
            <div className="text-zinc-600 text-sm">Waiting for events...</div>
          ) : (
            events.map((event, idx) => (
              <div key={event.id} className="flex items-start gap-4 group">
                <div className="font-mono text-zinc-600 text-sm whitespace-nowrap pt-0.5">
                  {event.timestamp.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
                </div>
                <div className="flex-1 flex justify-between items-center">
                  <span className="text-zinc-300 text-sm">{event.message}</span>
                  {idx === events.length - 1 && (
                    <span className="text-[10px] font-bold uppercase text-emerald-500 bg-emerald-500/10 px-2 py-0.5 rounded animate-pulse">
                      now
                    </span>
                  )}
                </div>
              </div>
            ))
          )}
        </div>
      </section>

      {/* Historical Charts */}
      <HistoricalCharts />

      {/* Office Replay */}
      <OfficeReplay />

      {/* Mobile App Download */}
      <div className="flex justify-end pb-16">
        <a
          href="/apps/office-climate/latest.apk"
          download
          className="inline-flex items-center gap-2 rounded-full border border-zinc-800/80 px-3 py-1.5 text-xs font-medium text-zinc-500 transition-colors hover:text-zinc-300 hover:border-zinc-700"
        >
          <svg
            aria-hidden="true"
            viewBox="0 0 20 20"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.7"
            className="h-3.5 w-3.5"
          >
            <path strokeLinecap="round" strokeLinejoin="round" d="M10 3v8m0 0 3-3m-3 3-3-3M4 14.5h12" />
          </svg>
          <span>Install App</span>
        </a>
      </div>

      {/* System Health Status Bar */}
      <footer className="fixed bottom-0 left-0 right-0 bg-black/80 backdrop-blur-md border-t border-zinc-800 p-3 flex justify-center items-center gap-6">
        <ConnectionDot connected={apiConnected} label="API" />
        <div className="flex items-center gap-2 text-zinc-800">•</div>
        <ConnectionDot connected={wsConnected} label="WebSocket" />
        <div className="flex items-center gap-2 text-zinc-800">•</div>
        <ConnectionDot connected={apiConnected && state.co2 > 0} label="Qingping" />
        <div className="flex items-center gap-2 text-zinc-800">•</div>
        <ConnectionDot connected={apiConnected} label="YoLink" />
      </footer>
    </div>
  );
};

export default App;
