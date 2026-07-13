import { useCallback } from 'react';

interface UseProfileScopedRefreshOptions {
  refreshHistoryStats: () => Promise<unknown>;
  refreshLibraryData: () => Promise<unknown>;
  refreshRecentHistory: () => Promise<unknown>;
  refreshRecentlyPlayed: () => Promise<unknown>;
}

export function useProfileScopedRefresh({
  refreshHistoryStats,
  refreshLibraryData,
  refreshRecentHistory,
  refreshRecentlyPlayed
}: UseProfileScopedRefreshOptions) {
  return useCallback(async () => {
    await Promise.allSettled([
      refreshLibraryData(),
      refreshRecentlyPlayed(),
      refreshHistoryStats(),
      refreshRecentHistory()
    ]);
  }, [refreshHistoryStats, refreshLibraryData, refreshRecentHistory, refreshRecentlyPlayed]);
}
