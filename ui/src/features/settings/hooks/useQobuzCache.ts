import { useCallback, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { clearQobuzAlbumShelfCache } from '../../qobuz/model/qobuzAlbumShelfData';

export function useQobuzCache() {
  const [qobuzCache, setQobuzCache] = useState<JsonRecord | null>(null);

  const reloadQobuzCache = useCallback(() => {
    return endpoints
      .qobuzCache()
      .then(setQobuzCache)
      .catch(() => setQobuzCache(null));
  }, []);

  const clearQobuzCache = useCallback(() => {
    return endpoints.clearQobuzCache().then(() => {
      clearQobuzAlbumShelfCache();
      return reloadQobuzCache();
    });
  }, [reloadQobuzCache]);

  return {
    clearQobuzCache,
    qobuzCache,
    reloadQobuzCache
  };
}
