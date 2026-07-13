import { useCallback, useState } from 'react';
import type { JsonRecord } from '../../../shared/types';
import {
  loadQobuzHomeAlbumOfTheWeek,
  loadQobuzOverviewData,
  richerQobuzHome
} from '../model/qobuzData';

export function useQobuzHome() {
  const [qobuzStatus, setQobuzStatus] = useState<JsonRecord | null>(null);
  const [qobuzStatusLoaded, setQobuzStatusLoaded] = useState(false);
  const [qobuzHome, setQobuzHome] = useState<JsonRecord | null>(null);

  const refreshQobuzOverview = useCallback(async () => {
    try {
      const overview = await loadQobuzOverviewData();
      if (overview.qobuzStatus) setQobuzStatus(overview.qobuzStatus);
      if (overview.qobuzHome) setQobuzHome(overview.qobuzHome);
    } finally {
      setQobuzStatusLoaded(true);
    }
  }, []);

  const refreshQobuzHomeHighlight = useCallback(async () => {
    const value = await loadQobuzHomeAlbumOfTheWeek();
    setQobuzHome((current) => richerQobuzHome(current, value));
  }, []);

  return {
    qobuzHome,
    qobuzStatus,
    qobuzStatusLoaded,
    refreshQobuzHomeHighlight,
    refreshQobuzOverview
  };
}
