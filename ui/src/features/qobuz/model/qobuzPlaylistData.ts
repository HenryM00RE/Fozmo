import { colonStorageKey } from '../../../shared/identity';
import { endpoints } from '../../../shared/lib/api';
import { albumArt, safeArray } from '../../../shared/lib/appSupport';
import { qobuzTrackToQueueItem } from '../../../shared/lib/queue';
import type {
  JsonRecord,
  QobuzFeaturedPlaylistsResponse,
  QobuzTrack,
  QueueItem
} from '../../../shared/types';

type QobuzPlaylistShelfCacheEntry = {
  loadedAt: number;
  response: QobuzFeaturedPlaylistsResponse;
};

type QobuzPlaylistDetailCacheEntry = {
  detail: JsonRecord;
  loadedAt: number;
};

const qobuzPlaylistShelfCacheStorageKey = colonStorageKey('qobuz-playlist-shelf:v2');
const qobuzPlaylistDetailCacheStorageKey = colonStorageKey('qobuz-playlist-detail:v1');
const qobuzPlaylistShelfCacheTtlMs = 15 * 60 * 1000;
const qobuzPlaylistDetailCacheTtlMs = 60 * 60 * 1000;
const qobuzPlaylistShelfCacheMaxEntries = 16;
const qobuzPlaylistDetailCacheMaxEntries = 80;
const qobuzPlaylistShelfMemoryCache = new Map<string, QobuzPlaylistShelfCacheEntry>();
const qobuzPlaylistDetailMemoryCache = new Map<string, QobuzPlaylistDetailCacheEntry>();

export async function loadQobuzPlaylistDetail(id: string | number, signal?: AbortSignal) {
  return endpoints.qobuzPlaylist(id, signal);
}

export function readQobuzPlaylistDetailCache(
  id: string | number | null | undefined
): JsonRecord | null {
  const key = qobuzPlaylistDetailCacheKey(id);
  if (!key) return null;

  const memoryEntry = qobuzPlaylistDetailMemoryCache.get(key);
  if (isFreshQobuzPlaylistDetailCacheEntry(memoryEntry)) return memoryEntry.detail;

  const storageEntry = readStoredQobuzPlaylistDetailCache()[key];
  if (!isFreshQobuzPlaylistDetailCacheEntry(storageEntry)) return null;

  qobuzPlaylistDetailMemoryCache.set(key, storageEntry);
  return storageEntry.detail;
}

export async function loadQobuzPlaylistDetailCached(id: string | number, signal?: AbortSignal) {
  const key = qobuzPlaylistDetailCacheKey(id);
  const cachedDetail = readQobuzPlaylistDetailCache(id);
  if (cachedDetail) return cachedDetail;

  try {
    const detail = await loadQobuzPlaylistDetail(id, signal);
    if (key) writeQobuzPlaylistDetailCache(key, detail);
    return detail;
  } catch (error) {
    const staleEntry = key
      ? readStoredQobuzPlaylistDetailCache()[key] || qobuzPlaylistDetailMemoryCache.get(key)
      : null;
    if (isQobuzPlaylistDetailCacheEntry(staleEntry)) return staleEntry.detail;
    throw error;
  }
}

export function qobuzPlaylistShelfCacheKey(
  limit = 12,
  offset = 0,
  genreId?: string | number | null,
  tag?: string | null
) {
  const normalizedLimit = Number.isFinite(Number(limit))
    ? Math.max(0, Math.trunc(Number(limit)))
    : 12;
  const normalizedOffset = Number.isFinite(Number(offset))
    ? Math.max(0, Math.trunc(Number(offset)))
    : 0;
  const normalizedGenre =
    genreId === undefined || genreId === null || genreId === '' ? 'all' : String(genreId);
  const normalizedTag = tag === undefined || tag === null || tag === '' ? 'all' : String(tag);
  return `limit:${normalizedLimit}|offset:${normalizedOffset}|genre:${normalizedGenre}|tag:${normalizedTag}`;
}

export function readQobuzPlaylistShelfCache(key: string): QobuzFeaturedPlaylistsResponse | null {
  const memoryEntry = qobuzPlaylistShelfMemoryCache.get(key);
  if (isFreshQobuzPlaylistShelfCacheEntry(memoryEntry)) return memoryEntry.response;

  const storageEntry = readStoredQobuzPlaylistShelfCache()[key];
  if (!isFreshQobuzPlaylistShelfCacheEntry(storageEntry)) return null;

  qobuzPlaylistShelfMemoryCache.set(key, storageEntry);
  return storageEntry.response;
}

export async function loadQobuzFeaturedPlaylistsCached(
  limit = 12,
  offset = 0,
  genreId?: string | number | null,
  tag?: string | null,
  signal?: AbortSignal
) {
  const cacheKey = qobuzPlaylistShelfCacheKey(limit, offset, genreId, tag);
  const cachedResponse = readQobuzPlaylistShelfCache(cacheKey);
  if (cachedResponse) return cachedResponse;

  try {
    const response = normalizeQobuzFeaturedPlaylistsResponse(
      await endpoints.qobuzFeaturedPlaylists(limit, offset, genreId, tag, signal),
      limit,
      offset
    );
    writeQobuzPlaylistShelfCache(cacheKey, response);
    return response;
  } catch (error) {
    const staleEntry =
      readStoredQobuzPlaylistShelfCache()[cacheKey] || qobuzPlaylistShelfMemoryCache.get(cacheKey);
    if (isQobuzPlaylistShelfCacheEntry(staleEntry)) return staleEntry.response;
    throw error;
  }
}

export function qobuzFeaturedPlaylistsFallbackResponse(
  playlists: JsonRecord[],
  limit = 12,
  offset = 0
): QobuzFeaturedPlaylistsResponse {
  const normalizedLimit = normalizedNonNegativeNumber(limit, 12);
  const normalizedOffset = normalizedNonNegativeNumber(offset, 0);
  const count = playlists.length;
  return {
    playlists,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count,
    total: null,
    has_more: count >= normalizedLimit
  };
}

export function normalizeQobuzFeaturedPlaylistsResponse(
  value: QobuzFeaturedPlaylistsResponse | JsonRecord[] | JsonRecord | null | undefined,
  limit = 12,
  offset = 0
): QobuzFeaturedPlaylistsResponse {
  if (Array.isArray(value)) return qobuzFeaturedPlaylistsFallbackResponse(value, limit, offset);

  const record = value && typeof value === 'object' ? (value as JsonRecord) : {};
  const playlists = safeArray<JsonRecord>(record.playlists);
  const normalizedLimit = normalizedNonNegativeNumber(
    record.limit,
    normalizedNonNegativeNumber(limit, 12)
  );
  const normalizedOffset = normalizedNonNegativeNumber(
    record.offset,
    normalizedNonNegativeNumber(offset, 0)
  );
  const count = normalizedNonNegativeNumber(record.count, playlists.length);
  const total = nullableNumber(record.total);
  const hasMore =
    typeof record.has_more === 'boolean'
      ? record.has_more
      : total === null
        ? count >= normalizedLimit
        : normalizedOffset + count < total;

  return {
    playlists,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count,
    total,
    has_more: hasMore
  };
}

export function qobuzPlaylistTracks(detail: JsonRecord | null | undefined): QobuzTrack[] {
  return safeArray<QobuzTrack>(detail?.tracks)
    .map((track) => ({ ...track, playlist_context: null }))
    .filter((track) => Number(track.id ?? track.track_id) > 0);
}

export function qobuzPlaylistQueueItems(detail: JsonRecord | null | undefined): QueueItem[] {
  return qobuzPlaylistTracks(detail).map(qobuzTrackToQueueItem);
}

export function qobuzPlaylistImage(value: JsonRecord | null | undefined) {
  const playlist = (
    value?.playlist && typeof value.playlist === 'object' ? value.playlist : value
  ) as JsonRecord | null | undefined;
  return albumArt(playlist);
}

export function isQobuzPlaylistRectangleImage(src: string | null | undefined) {
  return Boolean(src && /(?:^|_)rectangle(?:_mini)?\./i.test(src));
}

function writeQobuzPlaylistShelfCache(key: string, response: QobuzFeaturedPlaylistsResponse) {
  const entry = { loadedAt: Date.now(), response };
  qobuzPlaylistShelfMemoryCache.set(key, entry);

  const storedCache = readStoredQobuzPlaylistShelfCache();
  storedCache[key] = entry;
  const prunedEntries = Object.entries(storedCache)
    .filter(([, value]) => isFreshQobuzPlaylistShelfCacheEntry(value))
    .sort(([, a], [, b]) => b.loadedAt - a.loadedAt)
    .slice(0, qobuzPlaylistShelfCacheMaxEntries);
  writeStoredQobuzPlaylistShelfCache(Object.fromEntries(prunedEntries));
}

function qobuzPlaylistDetailCacheKey(id: string | number | null | undefined) {
  const value = id === undefined || id === null ? '' : String(id).trim();
  return value ? `playlist:${value}` : '';
}

function writeQobuzPlaylistDetailCache(key: string, detail: JsonRecord) {
  const entry = { detail, loadedAt: Date.now() };
  qobuzPlaylistDetailMemoryCache.set(key, entry);

  const storedCache = readStoredQobuzPlaylistDetailCache();
  storedCache[key] = entry;
  const prunedEntries = Object.entries(storedCache)
    .filter(([, value]) => isFreshQobuzPlaylistDetailCacheEntry(value))
    .sort(([, a], [, b]) => b.loadedAt - a.loadedAt)
    .slice(0, qobuzPlaylistDetailCacheMaxEntries);
  writeStoredQobuzPlaylistDetailCache(Object.fromEntries(prunedEntries));
}

function isFreshQobuzPlaylistShelfCacheEntry(
  value: unknown
): value is QobuzPlaylistShelfCacheEntry {
  return (
    isQobuzPlaylistShelfCacheEntry(value) &&
    Date.now() - value.loadedAt < qobuzPlaylistShelfCacheTtlMs
  );
}

function isQobuzPlaylistShelfCacheEntry(value: unknown): value is QobuzPlaylistShelfCacheEntry {
  if (!value || typeof value !== 'object') return false;
  const candidate = value as Partial<QobuzPlaylistShelfCacheEntry>;
  const response = candidate.response;
  return (
    typeof candidate.loadedAt === 'number' &&
    Boolean(response && typeof response === 'object' && !Array.isArray(response)) &&
    Array.isArray(response?.playlists) &&
    response.playlists.every((item) =>
      Boolean(item && typeof item === 'object' && !Array.isArray(item))
    )
  );
}

function nullableNumber(value: unknown) {
  if (value === null || value === undefined || value === '') return null;
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : null;
}

function normalizedNonNegativeNumber(value: unknown, fallback: number) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : fallback;
}

function isFreshQobuzPlaylistDetailCacheEntry(
  value: unknown
): value is QobuzPlaylistDetailCacheEntry {
  return (
    isQobuzPlaylistDetailCacheEntry(value) &&
    Date.now() - value.loadedAt < qobuzPlaylistDetailCacheTtlMs
  );
}

function isQobuzPlaylistDetailCacheEntry(value: unknown): value is QobuzPlaylistDetailCacheEntry {
  if (!value || typeof value !== 'object') return false;
  const candidate = value as Partial<QobuzPlaylistDetailCacheEntry>;
  return (
    typeof candidate.loadedAt === 'number' &&
    Boolean(
      candidate.detail && typeof candidate.detail === 'object' && !Array.isArray(candidate.detail)
    )
  );
}

function readStoredQobuzPlaylistShelfCache(): Record<string, QobuzPlaylistShelfCacheEntry> {
  const storage = browserStorage();
  if (!storage) return {};

  try {
    const raw = storage.getItem(qobuzPlaylistShelfCacheStorageKey);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    return Object.entries(parsed).reduce<Record<string, QobuzPlaylistShelfCacheEntry>>(
      (cache, [key, value]) => {
        if (isQobuzPlaylistShelfCacheEntry(value)) cache[key] = value;
        return cache;
      },
      {}
    );
  } catch {
    return {};
  }
}

function writeStoredQobuzPlaylistShelfCache(cache: Record<string, QobuzPlaylistShelfCacheEntry>) {
  const storage = browserStorage();
  if (!storage) return;

  try {
    storage.setItem(qobuzPlaylistShelfCacheStorageKey, JSON.stringify(cache));
  } catch {
    // Storage can be unavailable or full; memory cache still covers this session.
  }
}

function readStoredQobuzPlaylistDetailCache(): Record<string, QobuzPlaylistDetailCacheEntry> {
  const storage = browserStorage();
  if (!storage) return {};

  try {
    const raw = storage.getItem(qobuzPlaylistDetailCacheStorageKey);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    return Object.entries(parsed).reduce<Record<string, QobuzPlaylistDetailCacheEntry>>(
      (cache, [key, value]) => {
        if (isQobuzPlaylistDetailCacheEntry(value)) cache[key] = value;
        return cache;
      },
      {}
    );
  } catch {
    return {};
  }
}

function writeStoredQobuzPlaylistDetailCache(cache: Record<string, QobuzPlaylistDetailCacheEntry>) {
  const storage = browserStorage();
  if (!storage) return;

  try {
    storage.setItem(qobuzPlaylistDetailCacheStorageKey, JSON.stringify(cache));
  } catch {
    // Storage can be unavailable or full; memory cache still covers this session.
  }
}

function browserStorage(): Storage | null {
  try {
    return globalThis.localStorage || null;
  } catch {
    return null;
  }
}
