/**
 * API service for communicating with the orchestrator backend
 */

const API_PORT = import.meta.env.VITE_API_PORT || '8080';
const API_BASE = `http://localhost:${API_PORT}`;
const WS_URL = `ws://localhost:${API_PORT}/ws`;

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
    last_update: string | null;
  };
  erv: {
    running: boolean;
  };
  hvac: {
    mode: string;
    setpoint_c: number;
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
  const response = await fetch(`${API_BASE}/status`);
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
