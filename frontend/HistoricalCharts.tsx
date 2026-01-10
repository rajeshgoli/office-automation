import React, { useState, useEffect } from 'react';
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
  ReferenceLine,
} from 'recharts';
import { format, parseISO } from 'date-fns';
import { fetchHistory } from './api';

interface SensorReading {
  timestamp: string;
  co2_ppm: number | null;
  temp_c: number | null;
  humidity: number | null;
  tvoc: number | null;
}

interface HistoricalData {
  sensor_readings: SensorReading[];
  occupancy_history: any[];
  device_events: any[];
  climate_actions: any[];
}

const HistoricalCharts: React.FC = () => {
  const [data, setData] = useState<HistoricalData | null>(null);
  const [loading, setLoading] = useState(true);
  const [hours, setHours] = useState(24);
  const [activeMetric, setActiveMetric] = useState<'co2' | 'tvoc' | 'temp'>('co2');

  useEffect(() => {
    const loadData = async () => {
      setLoading(true);
      try {
        const historyData = await fetchHistory(hours);
        setData(historyData);
      } catch (error) {
        console.error('Failed to load historical data:', error);
      } finally {
        setLoading(false);
      }
    };

    loadData();
    // Refresh every 5 minutes
    const interval = setInterval(loadData, 5 * 60 * 1000);
    return () => clearInterval(interval);
  }, [hours]);

  if (loading) {
    return (
      <div className="bg-gray-800 rounded-lg p-6">
        <div className="text-center text-gray-400">Loading historical data...</div>
      </div>
    );
  }

  if (!data || !data.sensor_readings || data.sensor_readings.length === 0) {
    return (
      <div className="bg-gray-800 rounded-lg p-6">
        <div className="text-center text-gray-400">No historical data available</div>
      </div>
    );
  }

  // Prepare chart data (reverse to show chronological order)
  const chartData = [...data.sensor_readings].reverse().map((reading) => ({
    timestamp: reading.timestamp,
    time: format(parseISO(reading.timestamp), 'HH:mm'),
    co2: reading.co2_ppm,
    tvoc: reading.tvoc,
    temp: reading.temp_c ? reading.temp_c * 9 / 5 + 32 : null, // Convert to Fahrenheit
  }));

  const metrics = {
    co2: {
      label: 'CO₂ (ppm)',
      dataKey: 'co2',
      stroke: '#10b981',
      thresholds: [
        { value: 2000, label: 'Critical', color: '#ef4444' },
        { value: 1000, label: 'Elevated', color: '#f59e0b' },
      ],
    },
    tvoc: {
      label: 'tVOC Index',
      dataKey: 'tvoc',
      stroke: '#8b5cf6',
      thresholds: [
        { value: 250, label: 'Elevated', color: '#f59e0b' },
      ],
    },
    temp: {
      label: 'Temperature (°F)',
      dataKey: 'temp',
      stroke: '#3b82f6',
      thresholds: [],
    },
  };

  const currentMetric = metrics[activeMetric];

  return (
    <div className="bg-gray-800 rounded-lg p-4 sm:p-6">
      {/* Header - stack on mobile, side-by-side on desktop */}
      <div className="flex flex-col sm:flex-row sm:justify-between sm:items-center gap-4 mb-6">
        <h2 className="text-xl sm:text-2xl font-bold text-white">Historical Trends</h2>

        <div className="flex flex-col sm:flex-row gap-3 sm:gap-4">
          {/* Time range selector */}
          <select
            value={hours}
            onChange={(e) => setHours(Number(e.target.value))}
            className="bg-gray-700 text-white px-3 py-2 rounded-lg text-sm sm:text-base"
          >
            <option value={6}>Last 6 hours</option>
            <option value={12}>Last 12 hours</option>
            <option value={24}>Last 24 hours</option>
            <option value={48}>Last 48 hours</option>
            <option value={168}>Last week</option>
          </select>

          {/* Metric selector - full width on mobile */}
          <div className="flex gap-2">
            <button
              onClick={() => setActiveMetric('co2')}
              className={`flex-1 sm:flex-none px-3 sm:px-4 py-2 rounded-lg text-sm sm:text-base ${
                activeMetric === 'co2'
                  ? 'bg-green-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              CO₂
            </button>
            <button
              onClick={() => setActiveMetric('tvoc')}
              className={`flex-1 sm:flex-none px-3 sm:px-4 py-2 rounded-lg text-sm sm:text-base ${
                activeMetric === 'tvoc'
                  ? 'bg-purple-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              tVOC
            </button>
            <button
              onClick={() => setActiveMetric('temp')}
              className={`flex-1 sm:flex-none px-3 sm:px-4 py-2 rounded-lg text-sm sm:text-base ${
                activeMetric === 'temp'
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
              }`}
            >
              Temp
            </button>
          </div>
        </div>
      </div>

      <ResponsiveContainer width="100%" height={400}>
        <LineChart data={chartData}>
          <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
          <XAxis
            dataKey="time"
            stroke="#9ca3af"
            tick={{ fill: '#9ca3af' }}
          />
          <YAxis stroke="#9ca3af" tick={{ fill: '#9ca3af' }} />
          <Tooltip
            contentStyle={{
              backgroundColor: '#1f2937',
              border: '1px solid #374151',
              borderRadius: '0.5rem',
              color: '#fff',
            }}
            labelFormatter={(label) => `Time: ${label}`}
          />
          <Legend wrapperStyle={{ color: '#fff' }} />

          {/* Threshold lines */}
          {currentMetric.thresholds.map((threshold) => (
            <ReferenceLine
              key={threshold.value}
              y={threshold.value}
              stroke={threshold.color}
              strokeDasharray="3 3"
              label={{
                value: threshold.label,
                fill: threshold.color,
                fontSize: 12,
              }}
            />
          ))}

          <Line
            type="monotone"
            dataKey={currentMetric.dataKey}
            stroke={currentMetric.stroke}
            strokeWidth={2}
            dot={false}
            name={currentMetric.label}
            connectNulls
          />
        </LineChart>
      </ResponsiveContainer>

      {/* Data summary */}
      <div className="mt-6 grid grid-cols-3 gap-4 text-center">
        <div>
          <div className="text-gray-400 text-sm">Average</div>
          <div className="text-2xl font-bold text-white">
            {chartData.reduce((sum, d) => sum + (d[currentMetric.dataKey] || 0), 0) /
              chartData.filter((d) => d[currentMetric.dataKey] !== null).length || 0}
            {activeMetric === 'temp' ? '°F' : activeMetric === 'co2' ? ' ppm' : ''}
          </div>
        </div>
        <div>
          <div className="text-gray-400 text-sm">Peak</div>
          <div className="text-2xl font-bold text-white">
            {Math.max(...chartData.map((d) => d[currentMetric.dataKey] || 0))}
            {activeMetric === 'temp' ? '°F' : activeMetric === 'co2' ? ' ppm' : ''}
          </div>
        </div>
        <div>
          <div className="text-gray-400 text-sm">Readings</div>
          <div className="text-2xl font-bold text-white">
            {chartData.filter((d) => d[currentMetric.dataKey] !== null).length}
          </div>
        </div>
      </div>
    </div>
  );
};

export default HistoricalCharts;
