
import React from 'react';

export const STATUS_CONFIG = {
  PRESENT_QUIET: {
    label: 'PRESENT · QUIET',
    color: 'bg-emerald-500',
    text: 'text-emerald-500',
    border: 'border-emerald-500/30',
    icon: '🟢'
  },
  PRESENT_ELEVATED: {
    label: 'PRESENT · ELEVATED',
    color: 'bg-yellow-500',
    text: 'text-yellow-500',
    border: 'border-yellow-500/30',
    icon: '🟡'
  },
  PRESENT_VENTING: {
    label: 'PRESENT · VENTING',
    color: 'bg-orange-500',
    text: 'text-orange-500',
    border: 'border-orange-500/30',
    icon: '🟠'
  },
  AWAY_CLEARING: {
    label: 'AWAY · CLEARING',
    color: 'bg-blue-500',
    text: 'text-blue-500',
    border: 'border-blue-500/30',
    icon: '🔵'
  },
  AWAY_CLEAR: {
    label: 'AWAY · CLEAR',
    color: 'bg-blue-400',
    text: 'text-blue-400',
    border: 'border-blue-400/30',
    icon: '🔵'
  },
  OPEN_AIR: {
    label: 'OPEN AIR',
    color: 'bg-gray-400',
    text: 'text-gray-400',
    border: 'border-gray-400/30',
    icon: '⚪'
  },
  ERROR: {
    label: 'CHECK SYSTEM',
    color: 'bg-red-500',
    text: 'text-red-500',
    border: 'border-red-500/30',
    icon: '🔴'
  }
};
