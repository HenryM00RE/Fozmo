import { endpoints } from '../../../shared/lib/api';
import { normalizeQobuzAlbumId, safeArray } from '../../../shared/lib/appSupport';
import { storedProfileId } from '../../../shared/lib/profileSelection';
import type { JsonRecord, LibraryAlbum } from '../../../shared/types';
import { favoriteAlbumKey, importLegacyFavoriteAlbums } from './albumFavorites';

const FAVORITE_ALBUMS_CACHE_MS = 60_000;
const ALBUM_DETAIL_CACHE_MS = 60_000;
const APPLE_MUSIC_CATALOG_ALBUM_CACHE_MS = 5 * 60_000;
const QOBUZ_ALBUMS_CACHE_MS = 5 * 60_000;

type CachedPromise<T> = {
  loadedAt: number;
  promise: Promise<T>;
};

export type QobuzAlbumsResult = {
  loggedIn: boolean;
  albums: LibraryAlbum[];
};

let legacyFavoriteImportPromise: Promise<void> | null = null;
const albumDetailCache = new Map<string, CachedPromise<JsonRecord>>();
const appleMusicCatalogAlbumCache = new Map<string, CachedPromise<JsonRecord>>();
let favoriteAlbumsCache: CachedPromise<LibraryAlbum[]> | null = null;
let qobuzAlbumsCache: CachedPromise<QobuzAlbumsResult> | null = null;

function isFresh<T>(cache: CachedPromise<T> | null | undefined, ttlMs: number) {
  return Boolean(cache && Date.now() - cache.loadedAt < ttlMs);
}

function ensureLegacyFavoritesImported() {
  if (!legacyFavoriteImportPromise) {
    legacyFavoriteImportPromise = importLegacyFavoriteAlbums();
  }
  return legacyFavoriteImportPromise;
}

export function loadFavoriteAlbumsCached(options: { force?: boolean } = {}) {
  if (!options.force && isFresh(favoriteAlbumsCache, FAVORITE_ALBUMS_CACHE_MS)) {
    return favoriteAlbumsCache!.promise;
  }

  const promise = ensureLegacyFavoritesImported()
    .then(() => endpoints.favoriteAlbums())
    .then((favorites) => safeArray<LibraryAlbum>(favorites))
    .catch((error) => {
      favoriteAlbumsCache = null;
      throw error;
    });

  favoriteAlbumsCache = { loadedAt: Date.now(), promise };
  return promise;
}

export function loadAlbumDetailCached(id: string | number, options: { force?: boolean } = {}) {
  const key = String(id);
  const cached = albumDetailCache.get(key);
  if (!options.force && isFresh(cached, ALBUM_DETAIL_CACHE_MS)) {
    return cached!.promise;
  }

  const promise = endpoints.album(id).catch((error) => {
    albumDetailCache.delete(key);
    throw error;
  });

  albumDetailCache.set(key, { loadedAt: Date.now(), promise });
  return promise;
}

export function updateAlbumDetailCache(
  id: string | number | null | undefined,
  detail: JsonRecord | null | undefined
) {
  if (id === null || id === undefined || !detail) return;
  albumDetailCache.set(String(id), {
    loadedAt: Date.now(),
    promise: Promise.resolve(detail)
  });
}

function appleMusicCatalogAlbumCacheKey(
  albumId: string | number,
  storefront?: string,
  profileId = storedProfileId() || 'server-active'
) {
  const storefrontId =
    String(storefront || '')
      .trim()
      .toLowerCase() || 'current';
  return `${profileId}:${storefrontId}:${String(albumId).trim()}`;
}

export function loadAppleMusicCatalogAlbumCached(
  albumId: string | number,
  storefront?: string,
  options: { force?: boolean } = {}
) {
  const key = appleMusicCatalogAlbumCacheKey(albumId, storefront);
  const cached = appleMusicCatalogAlbumCache.get(key);
  if (!options.force && isFresh(cached, APPLE_MUSIC_CATALOG_ALBUM_CACHE_MS)) {
    return cached!.promise;
  }

  const promise = endpoints.appleMusicCatalogAlbum(String(albumId), storefront).catch((error) => {
    if (appleMusicCatalogAlbumCache.get(key)?.promise === promise) {
      appleMusicCatalogAlbumCache.delete(key);
    }
    throw error;
  });
  appleMusicCatalogAlbumCache.set(key, { loadedAt: Date.now(), promise });
  return promise;
}

export function updateAppleMusicCatalogAlbumCache(
  catalogAlbum: JsonRecord,
  albumId?: string | number,
  storefront?: string,
  profileId?: string
) {
  const resolvedAlbumId = String(albumId || catalogAlbum.album_id || catalogAlbum.id || '').trim();
  if (!resolvedAlbumId) return;
  const resolvedStorefront = String(storefront || catalogAlbum.storefront || '').trim();
  appleMusicCatalogAlbumCache.set(
    appleMusicCatalogAlbumCacheKey(resolvedAlbumId, resolvedStorefront, profileId),
    { loadedAt: Date.now(), promise: Promise.resolve(catalogAlbum) }
  );
}

export function invalidateAppleMusicCatalogAlbumCache() {
  appleMusicCatalogAlbumCache.clear();
}

export async function addFavoriteAlbumCached(payload: unknown) {
  const saved = await endpoints.addFavoriteAlbum(payload);
  updateFavoriteAlbumsCache((current) => upsertFavoriteAlbum(current, saved));
  return saved;
}

export async function removeFavoriteAlbumCached(payload: unknown) {
  const response = await endpoints.removeFavoriteAlbum(payload);
  const key = favoriteAlbumKey(payload as LibraryAlbum);
  if (key) {
    updateFavoriteAlbumsCache((current) =>
      current.filter((album) => favoriteAlbumKey(album) !== key)
    );
  } else {
    invalidateFavoriteAlbumsCache();
  }
  return response;
}

export function invalidateFavoriteAlbumsCache() {
  favoriteAlbumsCache = null;
}

export function loadQobuzAlbumsCached(options: { force?: boolean } = {}) {
  if (!options.force && isFresh(qobuzAlbumsCache, QOBUZ_ALBUMS_CACHE_MS)) {
    return qobuzAlbumsCache!.promise;
  }

  const promise = loadQobuzAlbums().catch((error) => {
    qobuzAlbumsCache = null;
    throw error;
  });

  qobuzAlbumsCache = { loadedAt: Date.now(), promise };
  return promise;
}

function updateFavoriteAlbumsCache(updater: (current: LibraryAlbum[]) => LibraryAlbum[]) {
  const loadedAt = Date.now();
  const promise = (favoriteAlbumsCache?.promise || Promise.resolve([]))
    .catch(() => [])
    .then((current) => updater(current));
  favoriteAlbumsCache = { loadedAt, promise };
}

function upsertFavoriteAlbum(current: LibraryAlbum[], saved: LibraryAlbum) {
  const savedKey = favoriteAlbumKey(saved);
  if (!savedKey) return current;
  const next = current.filter((album) => favoriteAlbumKey(album) !== savedKey);
  return [saved, ...next];
}

async function loadQobuzAlbums(): Promise<QobuzAlbumsResult> {
  const status = await endpoints.qobuzStatus();
  const loggedIn = Boolean(status.logged_in || status.authenticated);
  if (!loggedIn) return { loggedIn: false, albums: [] };
  const albums = await endpoints.qobuzAlbums();
  return {
    loggedIn: true,
    albums: safeArray<LibraryAlbum>(albums).map(qobuzAlbumForGrid)
  };
}

function qobuzAlbumForGrid(album: LibraryAlbum): LibraryAlbum {
  const qobuzAlbumId = normalizeQobuzAlbumId(album);
  return {
    ...album,
    id: qobuzAlbumId ? `qobuz:album:${qobuzAlbumId}` : album.id,
    qobuz_album_id: qobuzAlbumId || album.qobuz_album_id || album.id,
    qobuz_id: qobuzAlbumId || album.qobuz_id || album.id,
    provider: 'qobuz',
    is_qobuz: true,
    album_artist: album.album_artist || album.artist
  };
}
