import { useCallback, useEffect, useRef, useState } from 'react';
import type { RouteState } from '../../shared/types';

type UseAppRefreshParams = {
  refreshHistoryStats: () => Promise<void>;
  refreshLibraryData: () => Promise<void>;
  refreshPlaylists: () => Promise<void>;
  refreshQobuzHomeHighlight: () => Promise<void>;
  refreshQobuzOverview: () => Promise<void>;
  refreshRecentHistory: () => Promise<void>;
  refreshRecentlyPlayed: () => Promise<void>;
  refreshSettingsSupport: () => Promise<void>;
  route: RouteState;
  setNotice: (message: string) => void;
};

export function useAppRefresh({
  refreshHistoryStats,
  refreshLibraryData,
  refreshPlaylists,
  refreshQobuzHomeHighlight,
  refreshQobuzOverview,
  refreshRecentHistory,
  refreshRecentlyPlayed,
  refreshSettingsSupport,
  route,
  setNotice
}: UseAppRefreshParams) {
  const [loading, setLoading] = useState(true);
  const initialRefreshStartedRef = useRef(false);
  const skipInitialHomeRefreshRef = useRef(false);

  const refreshCore = useCallback(async () => {
    setLoading(true);
    await Promise.allSettled([refreshLibraryData(), refreshPlaylists(), refreshSettingsSupport()]);
    setLoading(false);
  }, [refreshLibraryData, refreshPlaylists, refreshSettingsSupport]);

  useEffect(() => {
    if (initialRefreshStartedRef.current) return;
    initialRefreshStartedRef.current = true;
    let cancelled = false;

    const refreshInitialHome = async () => {
      skipInitialHomeRefreshRef.current = true;
      await Promise.allSettled([
        refreshSettingsSupport(),
        refreshPlaylists(),
        ...(route.view === 'home' ? [refreshRecentlyPlayed()] : []),
        refreshQobuzOverview()
      ]);
      if (!cancelled) setLoading(false);
      await Promise.allSettled([refreshLibraryData(), refreshQobuzHomeHighlight()]);
    };

    const refreshInitialCore = async () => {
      if (route.view === 'albums' || route.view === 'songs' || route.view === 'artists') {
        await Promise.allSettled([
          refreshPlaylists(),
          refreshSettingsSupport(),
          refreshQobuzOverview()
        ]);
        if (!cancelled) setLoading(false);
        return;
      }
      await Promise.allSettled([refreshCore(), refreshQobuzOverview()]);
    };

    const refreshInitial =
      route.view === 'home' || route.view === 'discover' ? refreshInitialHome : refreshInitialCore;
    setLoading(true);
    refreshInitial().catch((error) => {
      if (!cancelled) {
        setLoading(false);
        setNotice(error instanceof Error ? error.message : 'Unable to load app data');
      }
    });

    return () => {
      cancelled = true;
    };
  }, [
    refreshCore,
    refreshLibraryData,
    refreshPlaylists,
    refreshQobuzHomeHighlight,
    refreshQobuzOverview,
    refreshRecentlyPlayed,
    refreshSettingsSupport,
    route.view,
    setNotice
  ]);

  useEffect(() => {
    if (route.view === 'home' || route.view === 'discover') {
      if (skipInitialHomeRefreshRef.current) {
        skipInitialHomeRefreshRef.current = false;
        return;
      }
      if (route.view === 'home') refreshRecentlyPlayed().catch(() => undefined);
      refreshQobuzOverview().catch(() => undefined);
      refreshQobuzHomeHighlight().catch(() => undefined);
    }
  }, [refreshQobuzHomeHighlight, refreshQobuzOverview, refreshRecentlyPlayed, route.view]);

  useEffect(() => {
    if (route.view === 'history') {
      refreshHistoryStats().catch(() => undefined);
      refreshRecentHistory().catch(() => undefined);
    }
  }, [refreshHistoryStats, refreshRecentHistory, route.view]);

  useEffect(() => {
    if (route.view === 'settings') {
      refreshSettingsSupport().catch(() => undefined);
      refreshQobuzOverview().catch(() => undefined);
    }
  }, [refreshQobuzOverview, refreshSettingsSupport, route.view]);

  return {
    loading,
    refreshCore
  };
}
