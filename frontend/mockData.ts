
import { OfficeState, DeviceStatus, OccupancyState, ActivityEvent, ClimateDataPoint } from './types';

export const generateMockHistory = (): ClimateDataPoint[] => {
  const history: ClimateDataPoint[] = [];
  const now = new Date();
  for (let i = 48; i >= 0; i--) {
    const time = new Date(now.getTime() - i * 30 * 60 * 1000);
    const hour = time.getHours();
    
    // Simulate typical office cycle
    let occupancy = OccupancyState.AWAY;
    if (hour >= 9 && hour <= 12) occupancy = OccupancyState.PRESENT;
    if (hour > 12 && hour < 13) occupancy = OccupancyState.AWAY; // Lunch
    if (hour >= 13 && hour <= 18) occupancy = OccupancyState.PRESENT;

    let co2 = 400;
    if (occupancy === OccupancyState.PRESENT) {
      co2 = 600 + Math.sin(i / 5) * 400 + (Math.random() * 100);
    } else {
      co2 = 400 + Math.random() * 50;
    }

    history.push({
      time: time.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }),
      co2: Math.round(co2),
      occupancy,
      venting: co2 > 1200
    });
  }
  return history;
};

export const INITIAL_STATE: OfficeState = {
  co2: 847,
  temperature: 72,
  tempTrend: 'stable',
  humidity: 45,
  door: DeviceStatus.CLOSED,
  window: DeviceStatus.CLOSED,
  hvacMode: DeviceStatus.COOL,
  hvacTarget: 70,
  ventMode: DeviceStatus.OFF,
  occupancy: OccupancyState.PRESENT,
  lastUpdated: new Date(),
  isSystemError: false
};

export const MOCK_EVENTS: ActivityEvent[] = [
  { id: '1', timestamp: new Date(new Date().setHours(8, 47)), type: 'arrival', message: 'Arrived · CO2 was 412 ppm (pre-ventilated)', co2: 412 },
  { id: '2', timestamp: new Date(new Date().setHours(10, 23)), type: 'departure', message: 'Away 23 min · Vented 847→493 ppm', co2: 493 },
  { id: '3', timestamp: new Date(new Date().setHours(12, 15)), type: 'departure', message: 'Away 8 min · Vented 721→518 ppm', co2: 518 },
  { id: '4', timestamp: new Date(), type: 'status', message: 'Present · CO2 847 ppm', co2: 847 },
];
