import { colonStorageKey } from '../../../shared/identity';
import { endpoints } from '../../../shared/lib/api';
import { safeArray } from '../../../shared/lib/appSupport';
import type { JsonRecord, QobuzAlbumPageResponse } from '../../../shared/types';

type QobuzAlbumShelfCacheEntry = {
  loadedAt: number;
  response: QobuzAlbumPageResponse;
};

const qobuzAlbumShelfCacheStorageKey = colonStorageKey('qobuz-album-shelf:v3');
const qobuzAlbumShelfCacheTtlMs = 15 * 60 * 1000;
const qobuzAlbumShelfCacheMaxStaleMs = 24 * 60 * 60 * 1000;
const qobuzAlbumShelfCacheMaxEntries = 24;
const qobuzAlbumShelfMemoryCache = new Map<string, QobuzAlbumShelfCacheEntry>();

const qobuzAlbumCategoryAliases: Record<string, string> = {
  'new-releases': 'new',
  'most-streamed': 'popular',
  'press-awards': 'acclaimed',
  qobuzissims: 'standouts'
};

export function canonicalQobuzAlbumCategory(value: string | null | undefined) {
  const normalized = String(value || 'standouts')
    .trim()
    .toLowerCase();
  return qobuzAlbumCategoryAliases[normalized] || normalized || 'standouts';
}

export function qobuzAlbumShelfCacheKey(
  category = 'standouts',
  limit = 12,
  offset = 0,
  genreId?: string | number | null
) {
  const normalizedCategory = canonicalQobuzAlbumCategory(category);
  const normalizedLimit = normalizedNonNegativeNumber(limit, 12);
  const normalizedOffset = normalizedNonNegativeNumber(offset, 0);
  const normalizedGenre =
    genreId === undefined || genreId === null || genreId === '' ? 'all' : String(genreId);
  return `category:${normalizedCategory}|limit:${normalizedLimit}|offset:${normalizedOffset}|genre:${normalizedGenre}`;
}

export function readQobuzAlbumShelfCache(key: string): QobuzAlbumPageResponse | null {
  const memoryEntry = qobuzAlbumShelfMemoryCache.get(key);
  if (isFreshQobuzAlbumShelfCacheEntry(memoryEntry)) return memoryEntry.response;

  const storageEntry = readStoredQobuzAlbumShelfCache()[key];
  if (!isFreshQobuzAlbumShelfCacheEntry(storageEntry)) return null;

  qobuzAlbumShelfMemoryCache.set(key, storageEntry);
  return storageEntry.response;
}

export async function loadQobuzAlbumShelfCached(
  category = 'standouts',
  limit = 12,
  offset = 0,
  genreId?: string | number | null,
  signal?: AbortSignal
) {
  const normalizedCategory = canonicalQobuzAlbumCategory(category);
  const cacheKey = qobuzAlbumShelfCacheKey(normalizedCategory, limit, offset, genreId);
  const cachedResponse = readQobuzAlbumShelfCache(cacheKey);
  if (cachedResponse) return cachedResponse;

  try {
    const response = normalizeQobuzAlbumPageResponse(
      await endpoints.qobuzHomeSection(normalizedCategory, genreId, limit, offset, signal),
      normalizedCategory,
      limit,
      offset
    );
    writeQobuzAlbumShelfCache(cacheKey, response);
    return response;
  } catch (error) {
    const staleEntry =
      readStoredQobuzAlbumShelfCache()[cacheKey] || qobuzAlbumShelfMemoryCache.get(cacheKey);
    if (isStaleButUsableQobuzAlbumShelfCacheEntry(staleEntry)) return staleEntry.response;
    throw error;
  }
}

export function qobuzAlbumPageFromCollection(
  albums: JsonRecord[],
  limit = 12,
  offset = 0
): QobuzAlbumPageResponse {
  const normalizedLimit = normalizedNonNegativeNumber(limit, 12);
  const normalizedOffset = normalizedNonNegativeNumber(offset, 0);
  const pageAlbums = albums.slice(normalizedOffset, normalizedOffset + normalizedLimit);
  return {
    albums: pageAlbums,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count: pageAlbums.length,
    total: albums.length,
    has_more: normalizedOffset + pageAlbums.length < albums.length
  };
}

export function qobuzAlbumPageFromPreview(
  albums: JsonRecord[],
  limit = 12,
  offset = 0
): QobuzAlbumPageResponse {
  const normalizedLimit = normalizedNonNegativeNumber(limit, 12);
  const normalizedOffset = normalizedNonNegativeNumber(offset, 0);
  const pageAlbums = albums.slice(normalizedOffset, normalizedOffset + normalizedLimit);
  return {
    albums: pageAlbums,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count: pageAlbums.length,
    total: null,
    has_more: pageAlbums.length >= normalizedLimit
  };
}

export const qobuzAlbumShelfFallbackResponse = qobuzAlbumPageFromPreview;

export function normalizeQobuzAlbumPageResponse(
  value: QobuzAlbumPageResponse | JsonRecord[] | JsonRecord | null | undefined,
  category = 'standouts',
  limit = 12,
  offset = 0
): QobuzAlbumPageResponse {
  if (Array.isArray(value)) return qobuzAlbumPageFromResponseArray(value, limit, offset);

  const record = value && typeof value === 'object' ? (value as JsonRecord) : {};
  const targetCategory = canonicalQobuzAlbumCategory(category);
  const section = !safeArray(record.albums).length
    ? safeArray<JsonRecord>(record.sections).find(
        (item) => canonicalQobuzAlbumCategory(String(item.id || '')) === targetCategory
      )
    : null;
  const source = section || record;
  const albums = safeArray<JsonRecord>(source.albums);
  const normalizedLimit = normalizedNonNegativeNumber(
    source.limit,
    normalizedNonNegativeNumber(record.limit, normalizedNonNegativeNumber(limit, 12))
  );
  const normalizedOffset = normalizedNonNegativeNumber(
    source.offset,
    normalizedNonNegativeNumber(record.offset, normalizedNonNegativeNumber(offset, 0))
  );
  const count = normalizedNonNegativeNumber(
    source.count,
    normalizedNonNegativeNumber(record.count, albums.length)
  );
  const rawTotal = nullableNumber(source.total ?? record.total);
  const total =
    targetCategory === 'standouts' && rawTotal !== null && rawTotal <= normalizedOffset + count
      ? null
      : rawTotal;
  const hasSuspiciousStandoutsTotal = rawTotal !== total;
  const explicitHasMore =
    typeof source.has_more === 'boolean'
      ? source.has_more
      : typeof record.has_more === 'boolean'
        ? record.has_more
        : null;
  const hasMore = hasSuspiciousStandoutsTotal
    ? count >= normalizedLimit
    : explicitHasMore !== null
      ? explicitHasMore
      : total === null
        ? count >= normalizedLimit
        : normalizedOffset + count < total;

  return {
    albums,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count,
    total,
    has_more: hasMore
  };
}

export function clearQobuzAlbumShelfCache() {
  qobuzAlbumShelfMemoryCache.clear();
  try {
    window.localStorage.removeItem(qobuzAlbumShelfCacheStorageKey);
  } catch {
    // Local storage is a convenience cache only.
  }
}

function qobuzAlbumPageFromResponseArray(
  albums: JsonRecord[],
  limit = 12,
  offset = 0
): QobuzAlbumPageResponse {
  const normalizedLimit = normalizedNonNegativeNumber(limit, 12);
  const normalizedOffset = normalizedNonNegativeNumber(offset, 0);
  const count = albums.length;
  return {
    albums,
    limit: normalizedLimit,
    offset: normalizedOffset,
    count,
    total: null,
    has_more: count >= normalizedLimit
  };
}

function writeQobuzAlbumShelfCache(key: string, response: QobuzAlbumPageResponse) {
  const entry = { loadedAt: Date.now(), response };
  qobuzAlbumShelfMemoryCache.set(key, entry);

  const stored = readStoredQobuzAlbumShelfCache();
  stored[key] = entry;
  const pruned = Object.fromEntries(
    Object.entries(stored)
      .filter(([, value]) => isFreshQobuzAlbumShelfCacheEntry(value))
      .sort(([, a], [, b]) => b.loadedAt - a.loadedAt)
      .slice(0, qobuzAlbumShelfCacheMaxEntries)
  );

  try {
    window.localStorage.setItem(qobuzAlbumShelfCacheStorageKey, JSON.stringify(pruned));
  } catch {
    // Local storage is a convenience cache only.
  }
}

function readStoredQobuzAlbumShelfCache(): Record<string, QobuzAlbumShelfCacheEntry> {
  try {
    const parsed = JSON.parse(
      window.localStorage.getItem(qobuzAlbumShelfCacheStorageKey) || '{}'
    ) as unknown;
    if (!parsed || typeof parsed !== 'object') return {};
    return Object.fromEntries(
      Object.entries(parsed as Record<string, unknown>).filter(([, value]) =>
        isQobuzAlbumShelfCacheEntry(value)
      )
    ) as Record<string, QobuzAlbumShelfCacheEntry>;
  } catch {
    return {};
  }
}

function isFreshQobuzAlbumShelfCacheEntry(value: unknown): value is QobuzAlbumShelfCacheEntry {
  return (
    isQobuzAlbumShelfCacheEntry(value) && Date.now() - value.loadedAt < qobuzAlbumShelfCacheTtlMs
  );
}

function isStaleButUsableQobuzAlbumShelfCacheEntry(
  value: unknown
): value is QobuzAlbumShelfCacheEntry {
  return (
    isQobuzAlbumShelfCacheEntry(value) &&
    Date.now() - value.loadedAt < qobuzAlbumShelfCacheMaxStaleMs
  );
}

export function isQobuzAlbumShelfCacheEntry(value: unknown): value is QobuzAlbumShelfCacheEntry {
  if (!value || typeof value !== 'object') return false;
  const record = value as Partial<QobuzAlbumShelfCacheEntry>;
  const response = record.response;
  return (
    typeof record.loadedAt === 'number' &&
    Number.isFinite(record.loadedAt) &&
    Boolean(response && typeof response === 'object' && !Array.isArray(response)) &&
    Array.isArray(response?.albums) &&
    response.albums.every((item) =>
      Boolean(item && typeof item === 'object' && !Array.isArray(item))
    ) &&
    isNonNegativeInteger(response.limit) &&
    isNonNegativeInteger(response.offset) &&
    isNonNegativeInteger(response.count) &&
    (response.total === null || isNonNegativeInteger(response.total)) &&
    typeof response.has_more === 'boolean'
  );
}

function normalizedNonNegativeNumber(value: unknown, fallback: number) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : fallback;
}

function nullableNumber(value: unknown) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : null;
}

function isNonNegativeInteger(value: unknown) {
  return typeof value === 'number' && Number.isInteger(value) && value >= 0;
}
