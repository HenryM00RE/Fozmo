import { endpoints } from '../../../shared/lib/api';
import { normalizeQobuzAlbumId } from '../../../shared/lib/appSupport';
import type { LibraryAlbum } from '../../../shared/types';

export const FAV_ALBUMS_KEY = 'favorite_albums';
export const FAV_ALBUMS_IMPORTED_KEY = 'favorite_albums_server_imported';

export function isQobuzFavoriteAlbum(album: LibraryAlbum | null | undefined) {
  return Boolean(
    album &&
      (album.qobuz_id ||
        album.qobuz_album_id ||
        (typeof album.id === 'string' && album.id.includes('qobuz')) ||
        album.provider === 'qobuz' ||
        album.is_qobuz)
  );
}

export function favoriteAlbumKey(album: LibraryAlbum | null | undefined) {
  if (!album) return '';
  const isQobuz = isQobuzFavoriteAlbum(album);
  const id = isQobuz ? normalizeQobuzAlbumId(album) : (album.id ?? album.album_id);
  if (id === null || id === undefined || id === '') return '';
  return `${isQobuz ? 'qobuz' : 'local'}:${id}`;
}

export function favoriteAlbumPayload(album: LibraryAlbum | null | undefined) {
  const key = favoriteAlbumKey(album);
  if (!album || !key) return null;
  const isQobuz = key.startsWith('qobuz:');
  const id = key.slice(key.indexOf(':') + 1);
  return {
    id: String(id),
    provider: isQobuz ? 'qobuz' : 'local',
    title: album.title || 'Unknown album',
    album_artist: album.album_artist || album.artist || 'Unknown artist',
    art_id: numberOrNull(album.art_id),
    image_url: album.image_url || null,
    year: numberOrNull(album.year),
    is_qobuz: isQobuz,
    qobuz_id: isQobuz ? String(id) : null,
    qobuz_album_id: isQobuz ? String(id) : null,
    hires: Boolean(album.hires)
  };
}

export function readLegacyFavoriteAlbums() {
  try {
    const raw = localStorage.getItem(FAV_ALBUMS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as LibraryAlbum[]) : [];
  } catch {
    return [];
  }
}

export async function importLegacyFavoriteAlbums() {
  try {
    if (localStorage.getItem(FAV_ALBUMS_IMPORTED_KEY)) return;
    const legacyAlbums = readLegacyFavoriteAlbums();
    if (!legacyAlbums.length) {
      localStorage.setItem(FAV_ALBUMS_IMPORTED_KEY, '1');
      return;
    }
    await Promise.allSettled(
      legacyAlbums
        .map(favoriteAlbumPayload)
        .filter(Boolean)
        .map((payload) => endpoints.addFavoriteAlbum(payload))
    );
    localStorage.setItem(FAV_ALBUMS_IMPORTED_KEY, '1');
  } catch {
    // Favorites still load from the server when legacy browser storage is unavailable.
  }
}

function numberOrNull(value: unknown) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}
