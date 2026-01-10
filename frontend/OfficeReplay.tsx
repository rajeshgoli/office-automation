import React, { useState, useEffect, useRef } from 'react';
import { format, parseISO } from 'date-fns';
import { fetchHistory } from './api';

interface Event {
  timestamp: string;
  type: 'arrival' | 'departure' | 'co2_rise' | 'co2_fall' | 'erv_on' | 'erv_off' | 'door' | 'motion';
  description: string;
  data?: any;
}

interface HistoricalData {
  sensor_readings: any[];
  occupancy_history: any[];
  device_events: any[];
  climate_actions: any[];
}

const OfficeReplay: React.FC = () => {
  const [data, setData] = useState<HistoricalData | null>(null);
  const [events, setEvents] = useState<Event[]>([]);
  const [currentIndex, setCurrentIndex] = useState(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const intervalRef = useRef<NodeJS.Timeout | null>(null);

  useEffect(() => {
    const loadData = async () => {
      try {
        const historyData = await fetchHistory(24);
        setData(historyData);

        // Build event timeline
        const timeline: Event[] = [];

        // Add occupancy events
        historyData.occupancy_history.forEach((occ) => {
          timeline.push({
            timestamp: occ.timestamp,
            type: occ.state === 'present' ? 'arrival' : 'departure',
            description:
              occ.state === 'present'
                ? '👤 Arrived at office'
                : '🚪 Left office',
            data: occ,
          });
        });

        // Add device events
        historyData.device_events.forEach((evt) => {
          if (evt.device_type === 'door') {
            timeline.push({
              timestamp: evt.timestamp,
              type: 'door',
              description: `🚪 Door ${evt.event}`,
              data: evt,
            });
          } else if (evt.device_type === 'motion' && evt.event === 'detected') {
            timeline.push({
              timestamp: evt.timestamp,
              type: 'motion',
              description: '👋 Motion detected',
              data: evt,
            });
          }
        });

        // Add ERV actions
        historyData.climate_actions.forEach((action) => {
          if (action.system === 'erv') {
            const isOn = action.action !== 'off';
            timeline.push({
              timestamp: action.timestamp,
              type: isOn ? 'erv_on' : 'erv_off',
              description: isOn
                ? `💨 ERV ${action.action.toUpperCase()} (${action.reason})`
                : `⏸️ ERV OFF (${action.reason})`,
              data: action,
            });
          }
        });

        // Sort by timestamp
        timeline.sort((a, b) => new Date(a.timestamp).getTime() - new Date(b.timestamp).getTime());

        setEvents(timeline);
      } catch (error) {
        console.error('Failed to load historical data:', error);
      }
    };

    loadData();
  }, []);

  useEffect(() => {
    if (isPlaying && currentIndex < events.length - 1) {
      intervalRef.current = setInterval(() => {
        setCurrentIndex((prev) => {
          if (prev >= events.length - 1) {
            setIsPlaying(false);
            return prev;
          }
          return prev + 1;
        });
      }, 1000 / speed);
    } else {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    }

    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
      }
    };
  }, [isPlaying, currentIndex, events.length, speed]);

  const handlePlayPause = () => {
    if (currentIndex >= events.length - 1) {
      setCurrentIndex(0);
    }
    setIsPlaying(!isPlaying);
  };

  const handleReset = () => {
    setCurrentIndex(0);
    setIsPlaying(false);
  };

  const getEventIcon = (type: Event['type']) => {
    switch (type) {
      case 'arrival':
        return '👤';
      case 'departure':
        return '🚪';
      case 'erv_on':
        return '💨';
      case 'erv_off':
        return '⏸️';
      case 'door':
        return '🚪';
      case 'motion':
        return '👋';
      case 'co2_rise':
        return '📈';
      case 'co2_fall':
        return '📉';
      default:
        return '📍';
    }
  };

  const getEventColor = (type: Event['type']) => {
    switch (type) {
      case 'arrival':
        return 'bg-green-500';
      case 'departure':
        return 'bg-red-500';
      case 'erv_on':
        return 'bg-blue-500';
      case 'erv_off':
        return 'bg-gray-500';
      case 'door':
        return 'bg-yellow-500';
      case 'motion':
        return 'bg-purple-500';
      default:
        return 'bg-gray-500';
    }
  };

  if (!data || events.length === 0) {
    return (
      <div className="bg-gray-800 rounded-lg p-6">
        <h2 className="text-2xl font-bold text-white mb-4">Office Replay</h2>
        <div className="text-center text-gray-400">Loading timeline...</div>
      </div>
    );
  }

  const currentEvent = events[currentIndex];
  const progress = ((currentIndex + 1) / events.length) * 100;

  return (
    <div className="bg-gray-800 rounded-lg p-6">
      <h2 className="text-2xl font-bold text-white mb-6">Office Replay 🎬</h2>

      {/* Progress bar */}
      <div className="mb-6">
        <div className="flex justify-between text-sm text-gray-400 mb-2">
          <span>
            Event {currentIndex + 1} of {events.length}
          </span>
          <span>{format(parseISO(currentEvent.timestamp), 'MMM d, HH:mm:ss')}</span>
        </div>
        <div className="w-full bg-gray-700 rounded-full h-2">
          <div
            className="bg-blue-500 h-2 rounded-full transition-all duration-300"
            style={{ width: `${progress}%` }}
          />
        </div>
      </div>

      {/* Current event display */}
      <div className="bg-gray-900 rounded-lg p-6 mb-6 min-h-[120px]">
        <div className="flex items-center gap-4">
          <div
            className={`text-4xl flex items-center justify-center w-16 h-16 rounded-full ${getEventColor(
              currentEvent.type
            )}`}
          >
            {getEventIcon(currentEvent.type)}
          </div>
          <div className="flex-1">
            <div className="text-xl font-bold text-white mb-2">{currentEvent.description}</div>
            <div className="text-gray-400 text-sm">
              {format(parseISO(currentEvent.timestamp), 'HH:mm:ss')}
            </div>
            {currentEvent.data?.co2_ppm && (
              <div className="text-gray-400 text-sm mt-1">
                CO₂: {currentEvent.data.co2_ppm} ppm
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Controls */}
      <div className="flex gap-4 items-center justify-center">
        <button
          onClick={handleReset}
          className="px-4 py-2 bg-gray-700 text-white rounded-lg hover:bg-gray-600"
        >
          ⏮️ Reset
        </button>
        <button
          onClick={handlePlayPause}
          className="px-6 py-2 bg-blue-600 text-white rounded-lg hover:bg-blue-700 font-bold"
        >
          {isPlaying ? '⏸️ Pause' : '▶️ Play'}
        </button>
        <select
          value={speed}
          onChange={(e) => setSpeed(Number(e.target.value))}
          className="px-4 py-2 bg-gray-700 text-white rounded-lg"
        >
          <option value={0.5}>0.5x</option>
          <option value={1}>1x</option>
          <option value={2}>2x</option>
          <option value={4}>4x</option>
        </select>
        <input
          type="range"
          min={0}
          max={events.length - 1}
          value={currentIndex}
          onChange={(e) => setCurrentIndex(Number(e.target.value))}
          className="flex-1"
        />
      </div>

      {/* Timeline preview */}
      <div className="mt-6 overflow-x-auto">
        <div className="flex gap-2 pb-4">
          {events.slice(Math.max(0, currentIndex - 5), currentIndex + 10).map((evt, idx) => {
            const actualIndex = Math.max(0, currentIndex - 5) + idx;
            const isCurrent = actualIndex === currentIndex;
            const isPast = actualIndex < currentIndex;

            return (
              <div
                key={actualIndex}
                onClick={() => setCurrentIndex(actualIndex)}
                className={`flex-shrink-0 w-16 h-16 rounded-lg flex items-center justify-center text-2xl cursor-pointer transition-all ${
                  isCurrent
                    ? `${getEventColor(evt.type)} scale-110 shadow-lg`
                    : isPast
                    ? 'bg-gray-700 opacity-50'
                    : 'bg-gray-700 hover:bg-gray-600'
                }`}
              >
                {getEventIcon(evt.type)}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
};

export default OfficeReplay;
