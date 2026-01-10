/**
 * API service for communicating with the orchestrator backend
 */

const API_PORT = import.meta.env.VITE_API_PORT || '8080';
const API_HOST = import.meta.env.VITE_API_HOST || window.location.hostname;

// Use current protocol and only add port if on localhost
const isLocalhost = API_HOST === 'localhost' || API_HOST === '127.0.0.1';
const protocol = window.location.protocol === 'https:' ? 'https:' : 'http:';
const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const portSuffix = isLocalhost ? `:${API_PORT}` : '';

const API_BASE = `${protocol}//${API_HOST}${portSuffix}`;
const WS_URL = `${wsProtocol}//${API_HOST}${portSuffix}/ws`;

const TOKEN_KEY = 'auth_token';
const EMAIL_KEY = 'user_email';

export function getAuthToken(): string | null {
  return localStorage.getItem(TOKEN_KEY);
}

export function setAuthToken(token: string, email: string) {
  localStorage.setItem(TOKEN_KEY, token);
  localStorage.setItem(EMAIL_KEY, email);
}

export function clearAuthToken() {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(EMAIL_KEY);
}

export function getUserEmail(): string | null {
  return localStorage.getItem(EMAIL_KEY);
}

export function isAuthenticated(): boolean {
  return getAuthToken() !== null;
}

export async function startLogin(): Promise<{ authorization_url: string }> {
  const response = await fetch(`${API_BASE}/auth/login`);
  if (!response.ok) {
    throw new Error('Failed to initiate login');
  }
  return response.json();
}

export async function logout(): Promise<void> {
  const token = getAuthToken();
  if (token) {
    await fetch(`${API_BASE}/auth/logout`, {
      method: 'POST',
      headers: { 'Authorization': `Bearer ${token}` }
    });
  }
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
    tvoc: number | null;
    noise_db: number | null;
    last_update: string | null;
  };
  erv: {
    running: boolean;
    speed?: string;
  };
  hvac: {
    mode: string;
    setpoint_c: number;
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
  const token = getAuthToken();
  const headers: HeadersInit = {};

  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const response = await fetch(`${API_BASE}/status`, { headers });

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
  const response = await fetch(`${API_BASE}/events?limit=${limit}`);
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
  const token = getAuthToken();
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const response = await fetch(`${API_BASE}/erv`, {
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

export type HVACMode = 'off' | 'heat';

/**
 * Set HVAC mode manually
 */
export async function setHVACMode(mode: HVACMode, setpoint_f: number = 70): Promise<{ ok: boolean; error?: string }> {
  const token = getAuthToken();
  const headers: HeadersInit = { 'Content-Type': 'application/json' };

  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  const response = await fetch(`${API_BASE}/hvac`, {
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
        // Send auth message first
        const token = getAuthToken();
        if (token) {
          this.ws!.send(JSON.stringify({ type: 'auth', token }));
        }
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
