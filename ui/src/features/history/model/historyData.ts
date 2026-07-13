import { endpoints } from '../../../shared/lib/api';
import { normalizeSearchText, primaryArtistName, safeArray } from '../../../shared/lib/appSupport';
import type { JsonRecord } from '../../../shared/types';

const artistProfileImageCache = new Map<string, Promise<string | null>>();
const historyStatsCache = new Map<string, { loadedAt: number; promise: Promise<JsonRecord> }>();
const HISTORY_STATS_CACHE_MS = 60_000;

export function loadHistoryStats(range = '4w', options: { force?: boolean } = {}) {
  const key = String(range || '4w');
  const cached = historyStatsCache.get(key);
  const now = Date.now();
  if (!options.force && cached && now - cached.loadedAt < HISTORY_STATS_CACHE_MS) {
    return cached.promise;
  }

  const promise = endpoints.historyStats(key).catch((error) => {
    historyStatsCache.delete(key);
    throw error;
  });
  historyStatsCache.set(key, { loadedAt: now, promise });
  return promise;
}

export function loadRecentHistory(limit = 50, excludeRadio = true) {
  return endpoints.recentHistory(limit, excludeRadio);
}

export async function lookupQobuzArtistProfileImage(name: string) {
  const primaryName = primaryArtistName(name);
  const key = primaryName.toLowerCase();
  if (!key) return null;
  const cached = artistProfileImageCache.get(key);
  if (cached) return cached;

  const promise = (async () => {
    try {
      const response = await endpoints.qobuzArtistSearch(primaryName, 5);
      const artists = safeArray<JsonRecord>(response?.artists);
      const targetKey = normalizeSearchText(primaryName);
      const match =
        artists.find((artist) => normalizeSearchText(artist.name) === targetKey) || artists[0];
      if (!match) return null;
      if (match.image_url) return String(match.image_url);
      if (!match.id) return null;
      const core = await endpoints.qobuzArtistCore(match.id as string | number);
      const artist = (core?.artist || {}) as JsonRecord;
      return artist.image_url ? String(artist.image_url) : null;
    } catch {
      return null;
    }
  })();

  artistProfileImageCache.set(key, promise);
  return promise;
}
