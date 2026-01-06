
import React from 'react';
import { AreaChart, Area, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, ReferenceLine } from 'recharts';
import { ClimateDataPoint, Threshold } from '../types';

interface CO2ChartProps {
  data: ClimateDataPoint[];
}

const CustomTooltip = ({ active, payload, label }: any) => {
  if (active && payload && payload.length) {
    return (
      <div className="bg-zinc-900 border border-zinc-800 p-3 rounded-lg shadow-xl">
        <p className="text-xs text-zinc-500 mb-1">{label}</p>
        <p className="text-lg font-bold text-emerald-400">{payload[0].value} ppm</p>
        <p className="text-[10px] text-zinc-600 uppercase tracking-tighter">
          {payload[0].payload.occupancy}
        </p>
      </div>
    );
  }
  return null;
};

const CO2Chart: React.FC<CO2ChartProps> = ({ data }) => {
  return (
    <div className="h-64 w-full mt-4">
      <ResponsiveContainer width="100%" height="100%">
        <AreaChart data={data} margin={{ top: 10, right: 10, left: -20, bottom: 0 }}>
          <defs>
            <linearGradient id="colorCo2" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor="#10b981" stopOpacity={0.3}/>
              <stop offset="95%" stopColor="#10b981" stopOpacity={0}/>
            </linearGradient>
          </defs>
          <CartesianGrid strokeDasharray="3 3" vertical={false} stroke="#27272a" />
          <XAxis 
            dataKey="time" 
            axisLine={false}
            tickLine={false}
            tick={{ fill: '#71717a', fontSize: 10 }}
            minTickGap={30}
          />
          <YAxis 
            axisLine={false}
            tickLine={false}
            tick={{ fill: '#71717a', fontSize: 10 }}
            domain={[0, 2500]}
          />
          <Tooltip content={<CustomTooltip />} />
          <ReferenceLine y={Threshold.NORMAL} stroke="#10b981" strokeDasharray="3 3" label={{ position: 'right', value: 'target', fill: '#10b981', fontSize: 10 }} />
          <ReferenceLine y={Threshold.CRITICAL} stroke="#ef4444" strokeDasharray="3 3" label={{ position: 'right', value: 'critical', fill: '#ef4444', fontSize: 10 }} />
          <Area 
            type="monotone" 
            dataKey="co2" 
            stroke="#10b981" 
            strokeWidth={2}
            fillOpacity={1} 
            fill="url(#colorCo2)" 
            isAnimationActive={false}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
};

export default CO2Chart;
