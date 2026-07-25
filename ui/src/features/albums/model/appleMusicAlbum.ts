import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  ResolvedPlaySource,
  SourceRef
} from '../../../shared/types';

function record(value: unknown): JsonRecord | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as JsonRecord) : null;
}

function records(value: unknown) {
  return Array.isArray(value)
    ? value.map(record).filter((item): item is JsonRecord => item !== null)
    : [];
}

function text(value: unknown, fallback = '') {
  const result = String(value ?? '').trim();
  return result || fallback;
}

function optionalText(value: unknown) {
  return text(value) || null;
}

function optionalNumber(value: unknown) {
  if (value === null || value === undefined || value === '') return null;
  const result = Number(value);
  return Number.isFinite(result) ? result : null;
}

function strings(value: unknown) {
  return Array.isArray(value) ? value.map((item) => text(item)).filter(Boolean) : [];
}

function offersLossless(variants: string[]) {
  return variants.some((variant) => variant.toLowerCase().includes('lossless'));
}

function offersHighResolutionLossless(variants: string[]) {
  return variants.some((variant) => {
    const normalized = variant.toLowerCase().replace(/[^a-z0-9]/g, '');
    return normalized.includes('highresolutionlossless') || normalized.includes('hireslossless');
  });
}

export function appleMusicSourceFromCatalogSong(
  song: JsonRecord,
  album?: JsonRecord | null
): SourceRef | null {
  const songId = text(song.song_id);
  if (!songId) return null;
  return {
    kind: 'apple_music_track',
    song_id: songId,
    storefront: optionalText(song.storefront ?? album?.storefront),
    title: optionalText(song.title),
    artist: optionalText(song.artist),
    album: optionalText(song.album_title ?? album?.title),
    album_artist: optionalText(song.album_artist ?? album?.artist),
    album_id: optionalText(song.album_id ?? album?.album_id),
    artwork_url: optionalText(song.artwork_url ?? album?.artwork_url),
    duration_secs: optionalNumber(song.duration_secs),
    track_number: optionalNumber(song.track_number),
    disc_number: optionalNumber(song.disc_number),
    isrc: optionalText(song.isrc)
  };
}

export function appleMusicSourceFromAlbumTrack(track: LibraryTrack): SourceRef | null {
  const playSource = record(track.play_source);
  if (
    playSource &&
    String(playSource.kind || '').includes('apple_music') &&
    text(playSource.song_id)
  ) {
    return playSource as SourceRef;
  }
  return appleMusicSourceFromCatalogSong(track);
}

export function appleMusicAlbumToLibraryDetail(catalogAlbum: JsonRecord): JsonRecord {
  const albumId = text(catalogAlbum.album_id);
  const storefront = text(catalogAlbum.storefront);
  const title = text(catalogAlbum.title, 'Untitled album');
  const artist = text(catalogAlbum.artist, 'Unknown artist');
  const artworkUrl = optionalText(catalogAlbum.artwork_url);
  const releaseDate = optionalText(catalogAlbum.release_date);
  const albumVariants = strings(catalogAlbum.audio_variants);
  const catalogTracks = records(catalogAlbum.tracks);
  const allVariants = [
    ...albumVariants,
    ...catalogTracks.flatMap((track) => strings(track.audio_variants))
  ];
  const highResolutionLossless = offersHighResolutionLossless(allVariants);
  const lossless = offersLossless(allVariants);
  const qualityLabel = highResolutionLossless
    ? 'Hi-Res Lossless'
    : lossless
      ? 'Lossless'
      : 'Apple Music';
  const tracks = catalogTracks
    .map((track, index) => {
      const source = appleMusicSourceFromCatalogSong(track, catalogAlbum);
      if (!source) return null;
      return {
        ...track,
        song_id: source.song_id,
        title: source.title || `Track ${index + 1}`,
        artist: source.artist || artist,
        album: source.album || title,
        album_artist: source.album_artist || artist,
        album_id: source.album_id || albumId,
        image_url: source.artwork_url || artworkUrl,
        duration_secs: source.duration_secs || 0,
        track_number: source.track_number || index + 1,
        disc_number: source.disc_number || 1,
        format: lossless ? 'ALAC' : 'Apple Music',
        play_source: source as ResolvedPlaySource
      } as LibraryTrack;
    })
    .filter((track): track is LibraryTrack => track !== null);
  const duration = tracks.reduce((sum, track) => sum + (Number(track.duration_secs) || 0), 0);
  const yearMatch = releaseDate?.match(/^(\d{4})/);
  const album = {
    id: albumId,
    album_id: albumId,
    provider: 'apple_music',
    storefront,
    title,
    artist,
    album_artist: artist,
    image_url: artworkUrl,
    release_date: releaseDate,
    year: yearMatch ? Number(yearMatch[1]) : null,
    upc: optionalText(catalogAlbum.upc),
    audio_variants: albumVariants,
    duration_secs: duration,
    track_count: tracks.length,
    tracks
  } as LibraryAlbum;
  return {
    provider: 'apple_music',
    quality_label: qualityLabel,
    album,
    tracks,
    versions: [
      {
        id: `apple_music:${storefront || 'default'}:${albumId}`,
        provider: 'apple_music',
        source_label: 'Apple Music',
        title,
        artist,
        year: album.year,
        track_count: tracks.length,
        format: lossless ? 'ALAC' : 'Apple Music',
        image_url: artworkUrl,
        audio_variants: allVariants,
        is_primary: true
      }
    ]
  };
}
