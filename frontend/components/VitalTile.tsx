
import React from 'react';
import { DeviceStatus } from '../types';

interface VitalTileProps {
  label: string;
  value: string | number;
  unit?: string;
  icon?: string;
  status?: DeviceStatus;
  attention?: boolean;
}

const VitalTile: React.FC<VitalTileProps> = ({ label, value, unit, icon, status, attention }) => {
  const getStatusColor = () => {
    if (attention) return 'border-orange-500/50 bg-orange-500/5';
    if (status === DeviceStatus.OPEN || status === DeviceStatus.ON || status === DeviceStatus.FULL) {
      return 'border-zinc-700 bg-zinc-800/50';
    }
    return 'border-zinc-800/50 bg-zinc-900/30';
  };

  return (
    <div className={`p-4 rounded-2xl border transition-all duration-300 ${getStatusColor()}`}>
      <div className="flex items-center gap-2 mb-1">
        <span className="text-xl">{icon}</span>
        <span className="text-xs font-medium text-zinc-500 uppercase tracking-wider">{label}</span>
      </div>
      <div className="flex items-baseline gap-1">
        <span className="text-2xl font-bold text-zinc-100">{value}</span>
        {unit && <span className="text-sm font-medium text-zinc-500">{unit}</span>}
      </div>
    </div>
  );
};

export default VitalTile;
