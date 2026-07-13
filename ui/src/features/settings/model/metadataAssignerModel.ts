import type { JsonRecord, LibraryAlbum } from '../../../shared/types';

export type QobuzAlbumChoice = {
  id: string;
  title: string;
  artist: string;
  year: string | null;
  image_url: string | null;
  tracks_count: number | null;
  duration: number | null;
  maximum_sampling_rate: number | null;
  maximum_bit_depth: number | null;
  hires: boolean;
};

export function initialQobuzQuery(album: LibraryAlbum) {
  return [album.album_artist || album.artist, album.title].filter(Boolean).join(' ').trim();
}

export function normalizeQobuzAlbum(raw: unknown): QobuzAlbumChoice | null {
  const album = raw as JsonRecord | null | undefined;
  if (!album) return null;
  const id = album.id ?? album.qobuz_album_id ?? album.qobuz_id;
  if (id === null || id === undefined || id === '') return null;
  return {
    id: String(id),
    title: String(album.title || ''),
    artist: String(album.artist || album.album_artist || ''),
    year: album.year === null || album.year === undefined ? null : String(album.year),
    image_url:
      typeof album.image_url === 'string'
        ? album.image_url
        : typeof album.cover_url === 'string'
          ? album.cover_url
          : null,
    tracks_count: nullableNumber(album.tracks_count ?? album.track_count),
    duration: nullableNumber(album.duration),
    maximum_sampling_rate: nullableNumber(album.maximum_sampling_rate ?? album.sample_rate),
    maximum_bit_depth: nullableNumber(album.maximum_bit_depth ?? album.bit_depth),
    hires: Boolean(album.hires)
  };
}

function nullableNumber(value: unknown) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

export function qobuzIdKey(id: unknown) {
  const value = String(id || '').trim();
  return value
    .replace(/^qobuz:album:/, '')
    .replace(/^qobuz:cd:/, '')
    .replace(/^qobuz:hires:/, '')
    .replace(/^qobuz:/, '');
}

export function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error || 'Unknown error');
}
