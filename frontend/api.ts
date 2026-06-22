/**
 * API service for communicating with the orchestrator backend
 */

// Determine API port: use env override, or URL port, or nothing (for standard ports via tunnel)
const envPort = import.meta.env.VITE_API_PORT;
const urlPort = window.location.port;  // Empty string for standard ports (80, 443)
const API_PORT = envPort || urlPort || '';  // Don't default to 8080 - trust the URL

const API_HOST = import.meta.env.VITE_API_HOST || window.location.hostname;

// Use current protocol and port from URL (or env override)
const protocol = window.location.protocol === 'https:' ? 'https:' : 'http:';
const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const portSuffix = API_PORT ? `:${API_PORT}` : '';

const API_BASE = `${protocol}//${API_HOST}${portSuffix}`;
const WS_URL = `${wsProtocol}//${API_HOST}${portSuffix}/ws`;

const TOKEN_KEY = 'auth_token';
const EMAIL_KEY = 'user_email';
const CSRF_COOKIE = 'office_csrf';
const CSRF_HEADER = 'X-CSRF-Token';

export function getAuthToken(): string | null {
  return null;
}

export function setAuthToken(_token: string, _email: string) {
  clearAuthToken();
}

export function clearAuthToken() {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(EMAIL_KEY);
}

export function getUserEmail(): string | null {
  return null;
}

export function isAuthenticated(): boolean {
  return false;
}

function getCsrfToken(): string | null {
  const prefix = `${CSRF_COOKIE}=`;
  const cookie = document.cookie
    .split(';')
    .map((value) => value.trim())
    .find((value) => value.startsWith(prefix));
  return cookie ? decodeURIComponent(cookie.slice(prefix.length)) : null;
}

function addCsrfHeader(headers: HeadersInit = {}): HeadersInit {
  const csrfToken = getCsrfToken();
  if (!csrfToken) return headers;
  return { ...headers, [CSRF_HEADER]: csrfToken };
}

function authFetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
  const method = (init.method || 'GET').toUpperCase();
  const headers = method === 'GET' || method === 'HEAD'
    ? init.headers
    : addCsrfHeader(init.headers);
  return fetch(input, {
    ...init,
    headers,
    credentials: 'include',
  });
}

/**
 * Check if current connection is authenticated (via token or trusted network)
 * Returns true if we can access the API without a token (trusted network)
 */
export async function checkTrustedNetwork(): Promise<boolean> {
  try {
    const response = await authFetch(`${API_BASE}/status`);
    return response.ok;
  } catch {
    return false;
  }
}

export async function startLogin(): Promise<{ authorization_url: string }> {
  clearAuthToken();
  const returnTo = `${window.location.pathname}${window.location.search}${window.location.hash}` || '/';
  const params = new URLSearchParams({
    redirect: '1',
    return_to: returnTo,
  });
  return { authorization_url: `${API_BASE}/auth/login?${params.toString()}` };
}

export async function logout(): Promise<void> {
  await authFetch(`${API_BASE}/auth/logout`, { method: 'POST' });
  clearAuthToken();
}

export interface ApiStatus {
  state: string;
  is_present: boolean;
  safety_interlock: boolean;
  erv_should_run: boolean;
  sensors: {
    mac_active: boolean;
    external_monitor: boolean;
    motion_detected: boolean;
    door_open: boolean;
    window_open: boolean;
    co2_ppm: number | null;
  };
  air_quality: {
    co2_ppm: number | null;
    temp_c: number | null;
    humidity: number | null;
    pm25: number | null;
    pm10: number | null;
    tvoc: number | null;
    noise_db: number | null;
    last_update: string | null;
    trusted_office_reading?: boolean;
    ignored_reason?: string | null;
  };
  erv: {
    running: boolean;
    speed?: string;
  };
  hvac: {
    mode: string;
    setpoint_c: number;
    temperature_bands?: HVACTemperatureBands;
    temperature_band_defaults?: HVACTemperatureBands;
  };
  manual_override?: {
    erv: boolean;
    erv_speed: string | null;
    erv_expires_in: number | null;
    hvac: boolean;
    hvac_mode: string | null;
    hvac_setpoint_f: number | null;
    hvac_expires_in: number | null;
  };
}

export interface HVACTemperatureBands {
  heat_on_temp_f: number;
  heat_off_temp_f: number;
  cool_off_temp_f: number;
  cool_on_temp_f: number;
}

export interface ApiEvent {
  id: string;
  timestamp: string;
  type: string;
  message: string;
  co2?: number;
}

/**
 * Fetch current status from orchestrator
 */
export async function fetchStatus(): Promise<ApiStatus> {
  const response = await authFetch(`${API_BASE}/status`);

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${response.statusText}`);
  }

  return response.json();
}

/**
 * Fetch recent events from orchestrator
 */
export async function fetchEvents(limit: number = 50): Promise<ApiEvent[]> {
  const response = await authFetch(`${API_BASE}/events?limit=${limit}`);
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${response.statusText}`);
  }
  return response.json();
}

/**
 * Fetch historical data from orchestrator
 */
export async function fetchHistory(hours: number = 24): Promise<any> {
  const response = await authFetch(`${API_BASE}/history?hours=${hours}`);

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${response.statusText}`);
  }

  return response.json();
}

/**
 * Convert Celsius to Fahrenheit
 */
export function toFahrenheit(celsius: number): number {
  return Math.round(celsius * 9 / 5 + 32);
}

export type ERVSpeed = 'off' | 'quiet' | 'medium' | 'turbo';

/**
 * Set ERV speed manually
 */
export async function setERVSpeed(speed: ERVSpeed): Promise<{ ok: boolean; error?: string }> {
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  const response = await authFetch(`${API_BASE}/erv`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ speed }),
  });

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  return response.json();
}

export type HVACMode = 'off' | 'heat' | 'cool';
export type PresenceState = 'present' | 'away';

/**
 * Set HVAC mode manually
 */
export async function setHVACMode(mode: HVACMode, setpoint_f: number = 70): Promise<{ ok: boolean; error?: string }> {
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  const response = await authFetch(`${API_BASE}/hvac`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ mode, setpoint_f }),
  });

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  return response.json();
}

/**
 * Manually correct detected presence.
 */
export async function setPresence(state: PresenceState): Promise<{ ok: boolean; error?: string; state?: string; is_present?: boolean }> {
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  const response = await authFetch(`${API_BASE}/presence`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ state }),
  });

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  return response.json();
}

/**
 * Update HVAC temperature hysteresis bands.
 */
export async function setHVACTemperatureBands(
  temperature_bands: HVACTemperatureBands
): Promise<{
  ok: boolean;
  error?: string;
  temperature_bands?: HVACTemperatureBands;
  temperature_band_defaults?: HVACTemperatureBands;
}> {
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  const response = await authFetch(`${API_BASE}/hvac/temperature-bands`, {
    method: 'POST',
    headers,
    body: JSON.stringify({ temperature_bands }),
  });

  if (response.status === 401) {
    clearAuthToken();
    throw new Error('Authentication required');
  }

  return response.json();
}

/**
 * WebSocket connection manager for real-time updates
 */
export class StatusWebSocket {
  private ws: WebSocket | null = null;
  private reconnectTimer: number | null = null;
  private onStatusCallback: ((status: ApiStatus) => void) | null = null;
  private onConnectionCallback: ((connected: boolean) => void) | null = null;

  connect() {
    if (this.ws?.readyState === WebSocket.OPEN) {
      return;
    }

    try {
      this.ws = new WebSocket(WS_URL);

      this.ws.onopen = () => {
        console.log('WebSocket connected');
        this.onConnectionCallback?.(true);
        if (this.reconnectTimer) {
          clearTimeout(this.reconnectTimer);
          this.reconnectTimer = null;
        }
      };

      this.ws.onmessage = (event) => {
        try {
          const status = JSON.parse(event.data) as ApiStatus;
          this.onStatusCallback?.(status);
        } catch (e) {
          console.error('Failed to parse WebSocket message:', e);
        }
      };

      this.ws.onclose = () => {
        console.log('WebSocket disconnected');
        this.onConnectionCallback?.(false);
        this.scheduleReconnect();
      };

      this.ws.onerror = (error) => {
        console.error('WebSocket error:', error);
        this.onConnectionCallback?.(false);
      };
    } catch (e) {
      console.error('Failed to create WebSocket:', e);
      this.onConnectionCallback?.(false);
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, 3000);
  }

  disconnect() {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  onStatus(callback: (status: ApiStatus) => void) {
    this.onStatusCallback = callback;
  }

  onConnection(callback: (connected: boolean) => void) {
    this.onConnectionCallback = callback;
  }
}
