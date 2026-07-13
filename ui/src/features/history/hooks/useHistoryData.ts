import { useCallback, useRef, useState } from 'react';
import type { JsonRecord } from '../../../shared/types';
import { loadHistoryStats, loadRecentHistory } from '../model/historyData';

export function useHistoryData() {
  const [historyStats, setHistoryStats] = useState<JsonRecord | null>(null);
  const [recentHistory, setRecentHistory] = useState<JsonRecord[]>([]);
  const [historyStatsLoading, setHistoryStatsLoading] = useState(true);
  const [recentHistoryLoading, setRecentHistoryLoading] = useState(true);
  const statsRequestIdRef = useRef(0);
  const recentRequestIdRef = useRef(0);

  const refreshHistoryStats = useCallback(async () => {
    const requestId = statsRequestIdRef.current + 1;
    statsRequestIdRef.current = requestId;
    setHistoryStatsLoading(true);
    try {
      const nextStats = await loadHistoryStats('4w', { force: true });
      if (statsRequestIdRef.current === requestId) setHistoryStats(nextStats);
    } finally {
      if (statsRequestIdRef.current === requestId) setHistoryStatsLoading(false);
    }
  }, []);

  const refreshRecentHistory = useCallback(async () => {
    const requestId = recentRequestIdRef.current + 1;
    recentRequestIdRef.current = requestId;
    setRecentHistoryLoading(true);
    try {
      const nextRecentHistory = await loadRecentHistory(50, true);
      if (recentRequestIdRef.current === requestId) setRecentHistory(nextRecentHistory);
    } finally {
      if (recentRequestIdRef.current === requestId) setRecentHistoryLoading(false);
    }
  }, []);

  const refreshHistoryData = useCallback(async () => {
    await Promise.allSettled([refreshHistoryStats(), refreshRecentHistory()]);
  }, [refreshHistoryStats, refreshRecentHistory]);

  return {
    historyStats,
    historyStatsLoading,
    recentHistory,
    recentHistoryLoading,
    refreshHistoryData,
    refreshHistoryStats,
    refreshRecentHistory
  };
}
