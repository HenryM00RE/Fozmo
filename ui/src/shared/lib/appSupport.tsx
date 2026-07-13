import type { CSSProperties } from 'react';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  Playlist,
  QobuzTrack,
  QueueItem,
  SourceRef
} from '../types';
import { endpoints } from './api';
import { normalizeQueueItem, sourceRefToQueueItem } from './queue';
export const RECENTLY_PLAYED_COLLAPSED_COUNT = 6;

export type GlobalSearchBucket = {
  songs: LibraryTrack[];
  albums: LibraryAlbum[];
  artists: JsonRecord[];
};

export type GlobalSearchState = {
  local: GlobalSearchBucket;
  qobuz: GlobalSearchBucket;
  localLoading: boolean;
  qobuzLoading: boolean;
  localError: string | null;
  qobuzError: string | null;
};

export type GlobalSearchPlacement = 'next' | 'end';
export type GlobalSearchSource = 'local' | 'qobuz';

export const emptyGlobalSearchBucket = (): GlobalSearchBucket => ({
  songs: [],
  albums: [],
  artists: []
});

export const initialGlobalSearchState = (): GlobalSearchState => ({
  local: emptyGlobalSearchBucket(),
  qobuz: emptyGlobalSearchBucket(),
  localLoading: false,
  qobuzLoading: false,
  localError: null,
  qobuzError: null
});

export function nextPlaylistName(playlists: Playlist[]) {
  const existing = new Set(playlists.map((playlist) => String(playlist.name || '').trim()));
  let index = playlists.length + 1;
  while (existing.has(`Playlist ${index}`)) index += 1;
  return `Playlist ${index}`;
}

export function titleOf(item: JsonRecord | null | undefined, fallback = 'Untitled') {
  return String(item?.title ?? item?.name ?? item?.file_name ?? fallback);
}

export function artistOf(item: JsonRecord | null | undefined) {
  return String(item?.artist ?? item?.album_artist ?? '');
}

export function albumArt(item: JsonRecord | null | undefined, size?: number) {
  const direct = item?.image_url ?? item?.cover_url;
  if (typeof direct === 'string' && direct) return direct;
  return endpoints.artUrl(
    (item?.art_id ?? item?.cover_art_id) as string | number | null | undefined,
    size
  );
}

export function albumArtSrcSet(item: JsonRecord | null | undefined) {
  const direct = item?.image_url ?? item?.cover_url;
  if (typeof direct === 'string' && direct) return undefined;
  const artId = (item?.art_id ?? item?.cover_art_id) as string | number | null | undefined;
  if (artId === undefined || artId === null || artId === '') return undefined;
  return [160, 256, 512].map((size) => `${endpoints.artUrl(artId, size)} ${size}w`).join(', ');
}

export function queueItemArt(item: QueueItem | null | undefined) {
  if (!item) return null;
  if (item.imageUrl) return item.imageUrl;
  if (item.qobuzTrack?.image_url) return item.qobuzTrack.image_url;
  if (item.resolvedSource?.image_url) return item.resolvedSource.image_url;
  return endpoints.artUrl(item.artId ?? item.resolvedSource?.art_id);
}

export function warmImage(src: string | null | undefined) {
  if (!src || typeof Image === 'undefined') return;
  const image = new Image();
  image.decoding = 'async';
  image.src = src;
}

export function artFallback() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" className="player-art-placeholder">
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="2" />
    </svg>
  );
}

export function queueItemFromSource(source: SourceRef) {
  return sourceRefToQueueItem(source);
}

export function safeArray<T = JsonRecord>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

export function idValue(...values: unknown[]): string | number {
  for (const value of values) {
    if (typeof value === 'number' && Number.isFinite(value)) return value;
    if (typeof value === 'string' && value.trim()) return value;
  }
  return '';
}

export function sourceTrack(track: QueueItem) {
  if (track.resolvedSource) return queueItemFromSource(track.resolvedSource) || track;
  return track;
}

export function longTitleClass(title: string) {
  const length = title.trim().length;
  if (length > 62) return 'is-ultra-long-title';
  if (length > 44) return 'is-extra-long-title';
  if (length > 30) return 'is-long-title';
  return '';
}

export function clampPercent(value: number, max = 100) {
  return Math.max(0, Math.min(max, Math.round(Number.isFinite(value) ? value : 0)));
}

export function sliderFillStyle(percent: number) {
  return { '--slider-fill': `${Math.max(0, Math.min(100, percent))}%` } as CSSProperties;
}

export function positiveNumber(value: unknown) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

export function albumTrackOrder(track: LibraryTrack, index: number) {
  return {
    disc: positiveNumber(track.disc_number) || 1,
    track: positiveNumber(track.track_number) || index + 1,
    index
  };
}

export function orderAlbumTracks(tracks: LibraryTrack[]) {
  return tracks
    .map((track, index) => ({ track, order: albumTrackOrder(track, index) }))
    .sort(
      (a, b) =>
        a.order.disc - b.order.disc ||
        a.order.track - b.order.track ||
        a.order.index - b.order.index
    )
    .map((item) => item.track);
}

export function formatAlbumDate(album: JsonRecord | null | undefined) {
  const raw = album?.release_date || album?.date || album?.release_date_original;
  if (raw) {
    const value = String(raw);
    const match = value.match(/^(\d{4})(?:-(\d{2})(?:-(\d{2}))?)?/);
    if (!match) return value;
    const [, year, month, day] = match;
    if (!month) return year;
    const parsed = new Date(Date.UTC(Number(year), Number(month) - 1, Number(day || 1)));
    if (Number.isNaN(parsed.getTime())) return year;
    return parsed.toLocaleDateString('en-GB', {
      year: 'numeric',
      month: 'long',
      ...(day ? { day: 'numeric' } : {}),
      timeZone: 'UTC'
    });
  }
  return album?.year ? String(album.year) : '';
}

export function plainDescription(value: unknown) {
  return String(value || '')
    .replace(/<[^>]*>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

export function decodeHtmlEntities(value: string) {
  if (typeof DOMParser === 'undefined') return value;
  const parsed = new DOMParser().parseFromString(value, 'text/html');
  return parsed.documentElement.textContent || '';
}

export function descriptionParagraphs(value: unknown) {
  return decodeHtmlEntities(
    String(value || '')
      .replace(/<\/p>/gi, '\n\n')
      .replace(/<br\s*\/?>/gi, '\n')
      .replace(/<[^>]*>/g, ' ')
  )
    .split(/\n{2,}/)
    .map((paragraph) => paragraph.replace(/\s+/g, ' ').trim())
    .filter(Boolean);
}

export function primaryArtistName(raw: unknown) {
  const name = String(raw || '').trim();
  if (!name) return '';
  return (
    name
      .split(/\s*(?:,|\sfeat\.?|\sft\.?|\swith\s|\sx\s|\s&\s|\sand\s|\svs\.?\s|\s\/\s)\s*/i)[0]
      ?.trim() || ''
  );
}

export function normalizeSearchText(value: unknown) {
  return String(value || '')
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .replace(/&/g, ' and ')
    .replace(/[^a-z0-9]+/g, ' ')
    .trim()
    .replace(/\s+/g, ' ');
}

export function wordBoundaryContains(text: string, query: string) {
  if (!text || !query) return false;
  return ` ${text} `.includes(` ${query} `);
}

export function compactMeta(parts: unknown[]) {
  return parts
    .filter(
      (part) =>
        part !== null &&
        part !== undefined &&
        String(part).trim() !== '' &&
        String(part) !== '00:00'
    )
    .map((part) => String(part).trim())
    .join(' / ');
}

export function artistMatchesName(value: unknown, name: string) {
  const candidate = normalizeSearchText(value);
  const target = normalizeSearchText(name);
  const primaryCandidate = normalizeSearchText(primaryArtistName(value));
  return Boolean(
    target &&
      (candidate === target ||
        primaryCandidate === target ||
        candidate.includes(target) ||
        target.includes(candidate))
  );
}

export const discographyBuckets = [
  { id: 'album', label: 'Albums' },
  { id: 'ep_single', label: 'Singles & EPs' },
  { id: 'library', label: 'In Library' },
  { id: 'compilation', label: 'Compilations' },
  { id: 'live', label: 'Live' }
] as const;

const EP_MAX_DURATION_SECONDS = 30 * 60;

export function albumVersionLabel(album: JsonRecord | null | undefined) {
  const version = String(album?.version || '').trim();
  return version && version.toLowerCase() !== 'standard' ? version : '';
}

export function discographyBucket(album: JsonRecord) {
  const raw = String(album.release_type || '').toLowerCase();
  const title = titleOf(album, '').toLowerCase();
  const trackCount = Number(album.tracks_count || album.track_count || 0);
  const duration = Number(album.duration || album.duration_secs || 0);
  const hasDuration = Number.isFinite(duration) && duration > 0;
  if (isLiveDiscographyAlbum(album)) return 'live';
  if (isRemixDiscographyAlbum(album)) return 'ep_single';
  if (
    raw === 'compilation' ||
    /\b(greatest hits|best of|anthology|collection|compilation)\b/.test(title)
  )
    return 'compilation';
  if (raw === 'single' || raw === 'ep' || raw === 'epmini') return 'ep_single';
  if (trackCount > 0 && trackCount <= 6 && hasDuration && duration <= EP_MAX_DURATION_SECONDS)
    return 'ep_single';
  return 'album';
}

export function dedupeDiscographyAlbums(albums: LibraryAlbum[]) {
  const grouped = new Map<string, LibraryAlbum[]>();
  albums.forEach((album) => {
    const key = discographyAlbumGroupKey(album);
    grouped.set(key, [...(grouped.get(key) || []), album]);
  });
  return Array.from(grouped.values()).map((group) => {
    const preferred = group.reduce((current, album) => preferredDiscographyAlbum(current, album));
    const versions = discographyAlbumVersions(group, preferred);
    return versions.length > 1 ? { ...preferred, qobuz_album_versions: versions } : preferred;
  });
}

export function discographyAlbumGroupKey(album: JsonRecord) {
  return [
    discographyBucket(album),
    normalizeSearchText(album.album_artist || album.artist),
    normalizeDiscographyTitle(titleOf(album, '')),
    discographyVariantKey(album)
  ].join('|');
}

function discographyAlbumVersions(group: LibraryAlbum[], preferred: LibraryAlbum) {
  const preferredId = normalizeQobuzAlbumId(preferred) || String(preferred.id || '');
  const ordered = [...group].sort((a, b) => discographyAlbumRank(b) - discographyAlbumRank(a));
  return ordered.sort((a, b) => {
    const aId = normalizeQobuzAlbumId(a) || String(a.id || '');
    const bId = normalizeQobuzAlbumId(b) || String(b.id || '');
    if (aId === preferredId) return -1;
    if (bId === preferredId) return 1;
    return 0;
  });
}

function normalizeDiscographyTitle(title: string) {
  return normalizeSearchText(
    title.replace(/[[(]?\s*\bexpanded(?:\s+edition)?\b\s*[\])]?:?/gi, ' ')
  );
}

function discographyVariantKey(album: LibraryAlbum) {
  if (isLiveDiscographyAlbum(album)) return 'live';
  if (isRemixDiscographyAlbum(album)) return 'remix';
  if (isExpandedDiscographyAlbum(album)) return 'default';
  const version = normalizeSearchText(albumVersionLabel(album));
  return version ? `version:${version}` : 'default';
}

function discographyDescriptor(album: JsonRecord) {
  return normalizeSearchText(`${titleOf(album, '')} ${albumVersionLabel(album)}`);
}

function isLiveDiscographyAlbum(album: JsonRecord) {
  const raw = String(album.release_type || '').toLowerCase();
  return raw === 'live' || wordBoundaryContains(discographyDescriptor(album), 'live');
}

function isRemixDiscographyAlbum(album: JsonRecord) {
  return /\b(remix|remixes|mixes|cover|covers)\b/.test(discographyDescriptor(album));
}

function isExpandedDiscographyAlbum(album: LibraryAlbum) {
  return wordBoundaryContains(discographyDescriptor(album), 'expanded');
}

function preferredDiscographyAlbum(existing: LibraryAlbum | undefined, candidate: LibraryAlbum) {
  if (!existing) return candidate;
  return discographyAlbumRank(candidate) > discographyAlbumRank(existing) ? candidate : existing;
}

function discographyAlbumRank(album: LibraryAlbum) {
  const trackCount = Number(album.tracks_count || album.track_count || 0);
  const duration = Number(album.duration || album.duration_secs || 0);
  return (
    (isExpandedDiscographyAlbum(album) ? 1_000_000 : 0) +
    (Number.isFinite(trackCount) ? trackCount * 100 : 0) +
    (Number.isFinite(duration) ? duration / 60 : 0)
  );
}

export function formatAlbumQualityStamp(tracks: LibraryTrack[]) {
  let maxRate = 0;
  let maxDepth = 16;
  let format = 'FLAC';
  tracks.forEach((track) => {
    const rate = Number(track.sample_rate);
    const depth = Number(track.bit_depth);
    if (Number.isFinite(rate) && rate > maxRate) maxRate = rate;
    if (Number.isFinite(depth) && depth > maxDepth) maxDepth = depth;
    if (track.format && String(track.format).trim()) format = String(track.format).toUpperCase();
  });
  const hz = maxRate || 44100;
  const khz = hz >= 1000 ? hz / 1000 : hz;
  const rateText = Number.isInteger(khz) ? String(khz) : khz.toFixed(1);
  return `${format} ${maxDepth}/${rateText}kHz`;
}

export function albumListenCount(track: JsonRecord) {
  const candidates = [track.play_count, track.listen_count, track.listens, track.plays];
  for (const candidate of candidates) {
    const count = Number(candidate);
    if (Number.isFinite(count) && count >= 0) return Math.trunc(count);
  }
  return 0;
}

export function formatLongDuration(seconds: unknown) {
  const totalSeconds = Math.max(0, Math.round(Number(seconds) || 0));
  if (!totalSeconds) return '';
  const totalMinutes = Math.max(1, Math.round(totalSeconds / 60));
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  const parts = [];
  if (hours) parts.push(`${hours} ${hours === 1 ? 'hour' : 'hours'}`);
  if (minutes || !hours) parts.push(`${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`);
  return parts.join(' ');
}

export function stringArray(value: unknown) {
  return Array.isArray(value) ? value.map((item) => String(item)).filter(Boolean) : [];
}

export const creditGroups = [
  {
    id: 'artists',
    title: 'Artists & Performance',
    roles: [
      'mainartist',
      'featuredartist',
      'associatedperformer',
      'performer',
      'artist',
      'vocal',
      'vocals',
      'allinstruments',
      'instrument'
    ]
  },
  {
    id: 'writing',
    title: 'Writing',
    roles: ['composer', 'lyricist', 'composerlyricist', 'writer', 'author']
  },
  {
    id: 'production',
    title: 'Production',
    roles: ['producer', 'coproducer', 'co-producer', 'executiveproducer', 'remixer', 'arranger']
  },
  {
    id: 'engineering',
    title: 'Engineering',
    roles: [
      'mixer',
      'mixingengineer',
      'masteringengineer',
      'recordingengineer',
      'engineer',
      'soundengineer'
    ]
  },
  { id: 'publishing', title: 'Publishing', roles: ['musicpublisher', 'publisher'] }
];

export function normalizeCreditRole(role: unknown) {
  return String(role || '')
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, '');
}

export function displayCreditRole(role: unknown) {
  return String(role || '')
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .replace(/\bCo Producer\b/i, 'Co-Producer');
}

export function creditGroupForRole(role: unknown) {
  const normalized = normalizeCreditRole(role);
  return creditGroups.find((group) => group.roles.includes(normalized));
}

export type CreditBuckets = Record<string, Record<string, Map<string, string>>>;

export function addCredit(grouped: CreditBuckets, groupId: string, role: unknown, name: unknown) {
  const cleanRole = String(role || '').trim();
  const cleanName = String(name || '').trim();
  if (!cleanRole || !cleanName) return;
  grouped[groupId] = grouped[groupId] || {};
  const roleLabel = displayCreditRole(cleanRole);
  grouped[groupId][roleLabel] = grouped[groupId][roleLabel] || new Map<string, string>();
  grouped[groupId][roleLabel].set(cleanName.toLowerCase(), cleanName);
}

export function collectAlbumCredits(tracks: LibraryTrack[]) {
  const grouped: CreditBuckets = {};
  tracks.forEach((track) => {
    safeArray<JsonRecord>(track.credits).forEach((credit) => {
      stringArray(credit.roles).forEach((role) => {
        addCredit(grouped, creditGroupForRole(role)?.id || 'other', role, credit.name);
      });
    });
    if (track.composer) addCredit(grouped, 'writing', 'Composer', track.composer);
    if (track.artist) addCredit(grouped, 'artists', 'Performer', track.artist);
  });
  return grouped;
}

export function trackCreditRows(track: LibraryTrack) {
  const rows: Array<[string, string]> = [];
  safeArray<JsonRecord>(track.credits).forEach((credit) => {
    const roles = stringArray(credit.roles).map(displayCreditRole).join(', ');
    if (credit.name && roles) rows.push([roles, String(credit.name)]);
  });
  if (
    track.composer &&
    !rows.some(([role, name]) => /composer/i.test(role) && name === track.composer)
  ) {
    rows.push(['Composer', String(track.composer)]);
  }
  if (track.work) rows.unshift(['Work', String(track.work)]);
  if (track.isrc) rows.push(['ISRC', String(track.isrc)]);
  if (track.copyright) rows.push(['Copyright', String(track.copyright)]);
  return rows;
}

export function versionQualityLabel(version: JsonRecord) {
  const parts = [];
  if (version.format) parts.push(String(version.format).toUpperCase());
  if (version.sample_rate) parts.push(`${(Number(version.sample_rate) / 1000).toFixed(1)}kHz`);
  if (version.bit_depth) parts.push(`${version.bit_depth}bit`);
  return parts.length ? parts.join(' ') : 'Quality unknown';
}

export function versionArt(version: JsonRecord) {
  const direct = version.image_url ?? version.cover_url;
  if (typeof direct === 'string' && direct) return direct;
  return endpoints.artUrl(
    (version.art_id ?? version.cover_art_id) as string | number | null | undefined
  );
}

export function sameVersionId(a: unknown, b: unknown) {
  return a !== null && a !== undefined && b !== null && b !== undefined && String(a) === String(b);
}

export function resolveViewingVersion(
  album: JsonRecord | null | undefined,
  versions: JsonRecord[],
  viewingVersionId?: string | number | null
) {
  if (!versions.length) return null;
  if (viewingVersionId !== null && viewingVersionId !== undefined) {
    const requested = versions.find((version) => sameVersionId(version.id, viewingVersionId));
    if (requested) return requested;
  }
  if (album?.primary_version_id !== null && album?.primary_version_id !== undefined) {
    const primary = versions.find((version) => sameVersionId(version.id, album.primary_version_id));
    if (primary) return primary;
  }
  return versions.find((version) => version.is_primary) || versions[0] || null;
}

export function qobuzFormatIdForVersion(version: JsonRecord | null | undefined) {
  if (!version || version.provider !== 'qobuz') return null;
  const rate = Number(version.sample_rate) || 0;
  const depth = Number(version.bit_depth) || 0;
  if (depth <= 16 && rate <= 44100) return 6;
  if (rate > 96000) return 27;
  return 7;
}

export function qobuzAlbumToLibraryShape(qobuzDetail: JsonRecord) {
  const album = (qobuzDetail.album || qobuzDetail) as JsonRecord;
  const rawTracks = orderAlbumTracks(safeArray<LibraryTrack>(qobuzDetail.tracks));
  const albumId = idValue(album.id, album.qobuz_album_id);
  const qobuzTracks = rawTracks.map((track, index) => {
    const maximumRate = Number(track.maximum_sampling_rate || album.maximum_sampling_rate || 44.1);
    return {
      ...track,
      album_id: track.album_id || albumId,
      album: track.album || album.title,
      image_url: track.image_url || album.image_url,
      duration_secs: track.duration_secs || track.duration,
      sample_rate: positiveNumber(track.sample_rate) || maximumRate * 1000,
      bit_depth:
        positiveNumber(track.bit_depth) ||
        positiveNumber(track.maximum_bit_depth) ||
        positiveNumber(album.maximum_bit_depth) ||
        16,
      track_number: track.track_number || index + 1,
      disc_number: track.disc_number || 1,
      qobuz_track: {
        ...track,
        album_id: track.album_id || albumId,
        album: track.album || album.title,
        image_url: track.image_url || album.image_url,
        duration: track.duration || track.duration_secs
      }
    };
  }) as LibraryTrack[];
  const versions: JsonRecord[] = [];
  if (album.hires) {
    versions.push({
      id: `qobuz:hires:${albumId}`,
      provider: 'qobuz',
      tier: 'hires',
      source_label: 'Qobuz Hi-Res',
      title: album.title,
      version: album.version,
      artist: album.artist || album.album_artist,
      year: album.year,
      track_count: qobuzTracks.length,
      sample_rate: album.maximum_sampling_rate ? Number(album.maximum_sampling_rate) * 1000 : null,
      bit_depth: album.maximum_bit_depth || 24,
      format: 'FLAC',
      image_url: album.image_url,
      is_primary: true
    });
  }
  versions.push({
    id: `qobuz:cd:${albumId}`,
    provider: 'qobuz',
    tier: 'cd',
    source_label: 'Qobuz CD',
    title: album.title,
    version: album.version,
    artist: album.artist || album.album_artist,
    year: album.year,
    track_count: qobuzTracks.length,
    sample_rate: 44100,
    bit_depth: 16,
    format: 'FLAC',
    image_url: album.image_url,
    is_primary: !album.hires
  });
  safeArray<LibraryAlbum>(qobuzDetail.qobuz_album_versions || album.qobuz_album_versions)
    .filter(
      (candidate) =>
        normalizeQobuzAlbumId(candidate) && normalizeQobuzAlbumId(candidate) !== String(albumId)
    )
    .forEach((candidate) => {
      const candidateId = normalizeQobuzAlbumId(candidate);
      const maximumRate = positiveNumber(candidate.maximum_sampling_rate);
      const maximumDepth = positiveNumber(candidate.maximum_bit_depth);
      versions.push({
        id: `qobuz:album:${candidateId}`,
        provider: 'qobuz',
        tier: 'catalog',
        source_label: albumVersionLabel(candidate) || 'Qobuz album',
        title: candidate.title,
        version: candidate.version,
        artist: candidate.album_artist || candidate.artist,
        year: candidate.year,
        track_count: candidate.tracks_count || candidate.track_count,
        sample_rate: maximumRate ? maximumRate * 1000 : null,
        bit_depth: maximumDepth || (candidate.hires ? 24 : 16),
        format: 'FLAC',
        image_url: candidate.image_url || candidate.cover_url,
        open_album_id: candidateId,
        is_primary: false
      });
    });

  return {
    album: {
      ...album,
      id: albumId,
      album_artist: album.album_artist || album.artist,
      track_count: qobuzTracks.length,
      qobuz_id: albumId,
      duration_secs:
        album.duration_secs ||
        album.duration ||
        qobuzTracks.reduce((sum, track) => sum + (Number(track.duration_secs) || 0), 0)
    },
    tracks: qobuzTracks,
    versions,
    candidates: []
  } as JsonRecord;
}

export function applyQobuzVersionToQobuzTracks(tracks: LibraryTrack[], version: JsonRecord | null) {
  if (!version || version.provider !== 'qobuz') return tracks;
  const formatId = qobuzFormatIdForVersion(version);
  return tracks.map((track) => {
    const source = (track.qobuz_track || track) as JsonRecord;
    return {
      ...track,
      sample_rate: version.sample_rate || track.sample_rate,
      bit_depth: version.bit_depth || track.bit_depth,
      format: version.format || track.format || 'FLAC',
      qobuz_track: {
        ...source,
        format_id: formatId,
        maximum_sampling_rate: version.sample_rate
          ? Number(version.sample_rate) / 1000
          : source.maximum_sampling_rate,
        maximum_bit_depth: version.bit_depth || source.maximum_bit_depth
      }
    } as LibraryTrack;
  });
}

export function qobuzTrackFromAlbumTrack(track: LibraryTrack) {
  const source = (track.qobuz_track || track) as QobuzTrack;
  const id = source.id ?? source.track_id ?? track.id ?? track.track_id;
  if (id === null || id === undefined) return null;
  return {
    ...source,
    id,
    track_id: source.track_id ?? id,
    title: source.title || track.title,
    artist: source.artist || track.artist,
    album: source.album || track.album,
    album_id: source.album_id || track.album_id,
    image_url: source.image_url || track.image_url,
    duration: source.duration || track.duration_secs,
    duration_secs: source.duration_secs || track.duration_secs
  } as QobuzTrack;
}

export function shuffled<T>(items: T[]) {
  const next = [...items];
  for (let i = next.length - 1; i > 0; i -= 1) {
    const j = Math.floor(Math.random() * (i + 1));
    [next[i], next[j]] = [next[j], next[i]];
  }
  return next;
}

export function normalizeQobuzAlbumId(album: unknown) {
  let id: unknown = album;
  if (album && typeof album === 'object') {
    const raw = album as JsonRecord;
    id = raw.qobuz_album_id || raw.qobuz_id || raw.album_id || raw.id;
  }
  if (id === null || id === undefined) return '';
  const value = String(id);
  if (value.startsWith('qobuz:album:')) return value.replace('qobuz:album:', '');
  if (value.startsWith('qobuz:cd:')) return value.replace('qobuz:cd:', '');
  if (value.startsWith('qobuz:hires:')) return value.replace('qobuz:hires:', '');
  return value;
}

export function resolveLocalAlbumId(album: JsonRecord, albums: LibraryAlbum[]) {
  const rawId = album.album_id ?? album.local_album_id ?? album.id;
  if (typeof rawId === 'number') return rawId;
  if (typeof rawId === 'string' && /^\d+$/.test(rawId)) return Number(rawId);
  const title = normalizeSearchText(album.title);
  const artist = normalizeSearchText(album.album_artist || album.artist);
  if (!title) return null;
  const matches = albums.filter((candidate) => normalizeSearchText(candidate.title) === title);
  if (matches.length === 1) return matches[0].id ?? null;
  const artistMatch = matches.find(
    (candidate) => normalizeSearchText(candidate.album_artist || candidate.artist) === artist
  );
  return artistMatch?.id ?? null;
}

export function dedupeRecentHistory(history: JsonRecord[]) {
  const seen = new Set<string>();
  const albums: JsonRecord[] = [];
  history.forEach((entry) => {
    if (entry.radio || (entry.source as JsonRecord | undefined)?.radio) return;
    const source = (entry.source || {}) as JsonRecord;
    const title = String(
      entry.album || source.album || entry.title || source.title || 'Unknown album'
    );
    const albumArtist = String(
      entry.album_artist || entry.artist || source.artist || 'Unknown artist'
    );
    const key = `${title.toLowerCase()}::${albumArtist.toLowerCase()}`;
    if (seen.has(key)) return;
    seen.add(key);
    const isQobuz = source.kind === 'qobuz_track';
    const qobuzAlbumId = isQobuz ? source.album_id || entry.qobuz_album_id || null : null;
    const localAlbumId = !isQobuz ? source.album_id || entry.album_id || null : null;
    albums.push({
      id: isQobuz
        ? qobuzAlbumId || `qobuz:track:${source.track_id}`
        : localAlbumId || `local:track:${source.track_id || entry.id}`,
      title,
      album_artist: albumArtist,
      art_id: entry.art_id || source.art_id || null,
      image_url: entry.image_url || source.image_url || null,
      year: null,
      is_qobuz: isQobuz,
      qobuz_album_id: qobuzAlbumId,
      source_track_id: source.track_id || null,
      album_id: localAlbumId,
      played_at: Number(entry.played_at || 0)
    });
  });
  return albums;
}

export function normalizeRecentPlaylistEntry(entry: JsonRecord, playlists: Playlist[]) {
  const playlistId = entry.playlist_id || entry.playlistId;
  if (!playlistId) return null;
  const playlist = playlists.find((item) => item.id === playlistId) || {
    id: String(playlistId),
    name: String(entry.title || 'Playlist'),
    items: safeArray<QueueItem>(entry.items).map(normalizeQueueItem).filter(Boolean) as QueueItem[]
  };
  const count = playlist.items?.length || 0;
  return {
    recent_type: 'playlist',
    id: `playlist:${playlist.id}`,
    playlist_id: playlist.id,
    title: playlist.name,
    album_artist: `Playlist - ${count} song${count === 1 ? '' : 's'}`,
    played_at: Number(entry.played_at || entry.playedAt || 0),
    is_playlist: true,
    items: playlist.items || []
  } as JsonRecord;
}

export function mergeRecentlyPlayed(
  history: JsonRecord[],
  recentPlaylists: JsonRecord[],
  playlists: Playlist[]
) {
  const playlistEntries = recentPlaylists
    .map((entry) => normalizeRecentPlaylistEntry(entry, playlists))
    .filter(Boolean) as JsonRecord[];
  const albumEntries = history.some((entry) => entry?.recent_type === 'album')
    ? history
    : dedupeRecentHistory(history);
  return [...playlistEntries, ...albumEntries]
    .filter(
      (item) =>
        !(item.is_qobuz && typeof item.id === 'string' && item.id.startsWith('qobuz:album:'))
    )
    .sort((a, b) => Number(b.played_at || 0) - Number(a.played_at || 0))
    .slice(0, 50);
}

export function recentlyPlayedSelectionKey(item: JsonRecord) {
  if (item.recent_type === 'playlist')
    return `playlist:${item.playlist_id || item.id || item.title || ''}`;
  const provider = item.is_qobuz ? 'qobuz' : 'local';
  const id = item.is_qobuz
    ? normalizeQobuzAlbumId(item) || item.qobuz_album_id || item.source_track_id || item.id
    : item.album_id || item.local_album_id || item.id;
  const title = normalizeSearchText(item.title);
  const artist = normalizeSearchText(item.album_artist || item.artist);
  return `${provider}:${id || `${title}::${artist}`}`;
}

export function playlistCoverItems(playlist: Playlist | JsonRecord | null | undefined) {
  const seen = new Set<string>();
  const arts: string[] = [];
  safeArray<QueueItem>(playlist?.items).forEach((item) => {
    const src =
      item.imageUrl ||
      item.qobuzTrack?.image_url ||
      item.resolvedSource?.image_url ||
      endpoints.artUrl(item.artId);
    if (!src || seen.has(src)) return;
    seen.add(src);
    arts.push(src);
  });
  return arts.slice(0, 4);
}

export function homeQobuzSectionTitle(section: JsonRecord) {
  const id = String(section.id || '')
    .trim()
    .toLowerCase();
  const title = String(section.title || '').trim();
  if (id === 'most-streamed' || title.toLowerCase() === 'most streamed') return 'Popular';
  if (id === 'qobuzissims' || title.toLowerCase() === 'qobuzissims') return 'Standouts';
  if (id === 'album-of-the-week' || title.toLowerCase() === 'album of the week')
    return 'Qobuz albums of the week';
  return title || 'Qobuz';
}

export function qobuzAlbumQualityLabel(album: JsonRecord) {
  const rate = Number(album.maximum_sampling_rate || album.sample_rate || 0);
  const depth = Number(album.maximum_bit_depth || album.bit_depth || 0);
  if (depth && rate) return `${depth}/${rate > 1000 ? Math.round(rate / 1000) : rate}kHz`;
  return album.hires ? 'Hi-Res' : '';
}
