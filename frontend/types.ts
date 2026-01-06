
export enum OccupancyState {
  PRESENT = 'PRESENT',
  AWAY = 'AWAY'
}

export enum DeviceStatus {
  OPEN = 'OPEN',
  CLOSED = 'CLOSED',
  ON = 'ON',
  OFF = 'OFF',
  FULL = 'FULL',
  HEAT = 'HEAT',
  COOL = 'COOL'
}

export interface ActivityEvent {
  id: string;
  timestamp: Date;
  type: string;
  message: string;
  co2?: number;
}

export interface ClimateDataPoint {
  time: string;
  co2: number;
  occupancy: OccupancyState;
  venting: boolean;
}

export interface OfficeState {
  co2: number;
  temperature: number;
  tempTrend: 'up' | 'down' | 'stable';
  humidity: number;
  tvoc: number;
  door: DeviceStatus;
  window: DeviceStatus;
  hvacMode: DeviceStatus;
  hvacTarget: number;
  ventMode: DeviceStatus;
  occupancy: OccupancyState;
  lastUpdated: Date;
  isSystemError: boolean;
}

export enum Threshold {
  NORMAL = 800,
  ELEVATED = 1500,
  CRITICAL = 2000
}
