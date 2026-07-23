import { createApiClient } from '../../generated/api-endpoints';
import type {
  CustomDisplayFontSettings,
  JsonRecord,
  LibraryAlbum,
  LibraryBrowsePage,
  LibraryBrowseParams,
  LibraryFoldersResponse,
  LibrarySearchResponse,
  LibraryTrack,
  PairingStartResponse,
  Playlist,
  ProfilesResponse,
  QobuzFeaturedPlaylistsResponse,
  QobuzTrack,
  QueueState,
  RemoteAccessSettingsUpdateRequest,
  ResolvedPlaySource,
  SourceRef
} from '../../types';
import { rememberProfileId } from '../profileSelection';
import { profileIdFromBody, requestHeaders } from '../requestContext';

export { playbackSequenceClientForPath } from '../requestContext';

interface QobuzPlaybackOptions {
  radioAuto?: boolean;
  expectedCurrent?: string | null;
}

export interface AutoMetaProgress extends JsonRecord {
  job_id?: number | null;
  status?: string;
  running?: boolean;
  processed?: number;
  total?: number;
  exact_matched?: number;
  musicbrainz_matched?: number;
  qobuz_matched?: number;
  no_proper_match?: number;
  skipped?: number;
  current_album?: string | null;
  current_version?: string | null;
  phase?: string | null;
  mode?: string | null;
  last_result?: string | null;
  error?: string | null;
  link_qobuz?: boolean;
  started_at?: number | null;
  updated_at?: number | null;
  finished_at?: number | null;
  elapsed_secs?: number | null;
  eta_secs?: number | null;
  rate_per_min?: number | null;
  remaining?: number;
  pause_requested?: boolean;
  stop_requested?: boolean;
  recent_results?: AutoMetaJobItem[];
  error_count?: number;
  errors?: unknown;
  failed?: number;
  failures?: unknown;
}

export interface AutoMetaJobItem extends JsonRecord {
  id: number;
  job_id: number;
  album_id: number;
  version_id: number;
  album_title: string;
  version_label: string;
  phase: string;
  status: string;
  attempts: number;
  musicbrainz_release_id?: string | null;
  qobuz_album_id?: string | null;
  message?: string | null;
  started_at?: number | null;
  finished_at?: number | null;
  updated_at: number;
}

export class ApiError extends Error {
  status: number;
  category: ApiErrorCategory;

  constructor(status: number, message: string, category = apiErrorCategory(status)) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.category = category;
  }
}

export type ApiErrorCategory =
  | 'validation'
  | 'unavailable'
  | 'authentication'
  | 'not_found'
  | 'conflict'
  | 'retryable_network'
  | 'persistence'
  | 'internal';

function apiErrorCategory(status: number): ApiErrorCategory {
  if (
    status === 0 ||
    status === 408 ||
    status === 425 ||
    status === 429 ||
    status === 502 ||
    status === 504
  ) {
    return 'retryable_network';
  }
  if (status === 401 || status === 403) return 'authentication';
  if (status === 404) return 'not_found';
  if (status === 409) return 'conflict';
  if (status === 400 || status === 413 || status === 422) return 'validation';
  if (status >= 500) return 'unavailable';
  return 'internal';
}

export function asApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  return new ApiError(
    0,
    error instanceof Error ? error.message : 'Network request failed',
    'retryable_network'
  );
}

function shouldTryLocalPairing(path: string, status: number) {
  const cleanPath = path.split('?')[0];
  return (
    cleanPath !== '/api/pairing/start' &&
    cleanPath !== '/api/sessions/browser' &&
    cleanPath !== '/api/remote/session' &&
    (status === 401 || status === 403)
  );
}

let localPairingPromise: Promise<boolean> | null = null;

async function requestLocalPairingToken() {
  if (!localPairingPromise) {
    localPairingPromise = apiClient
      .postPairingStart()
      .then(async (payload: PairingStartResponse | null) => {
        const token = String(payload?.token || '').trim();
        if (!token) return false;
        await apiClient.postSessionsBrowser({ pairing_token: token });
        return true;
      })
      .catch(() => false)
      .finally(() => {
        localPairingPromise = null;
      });
  }
  return localPairingPromise;
}

async function parseResponse<T>(response: Response): Promise<T> {
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

function withQuery(
  path: string,
  params?: Record<string, string | number | boolean | null | undefined>
) {
  if (!params) return path;
  const url = new URL(path, window.location.origin);
  Object.entries(params).forEach(([key, value]) => {
    if (value !== null && value !== undefined && value !== '')
      url.searchParams.set(key, String(value));
  });
  return `${url.pathname}${url.search}${url.hash}`;
}

function browseQuery(
  params: LibraryBrowseParams
): Record<string, string | number | boolean | null | undefined> {
  return { ...params };
}

export async function apiRequest<T = unknown>(
  path: string,
  options: {
    method?: string;
    body?: unknown;
    signal?: AbortSignal;
    silentStatuses?: number[];
    cache?: RequestCache;
  } = {}
): Promise<T> {
  const explicitProfileId =
    path === '/api/profiles/select' && (options.method || 'GET') === 'POST'
      ? profileIdFromBody(options.body)
      : '';
  if (explicitProfileId) {
    rememberProfileId(explicitProfileId);
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method: options.method || 'GET',
      cache: options.cache ?? (options.method ? 'no-store' : 'default'),
      credentials: 'same-origin',
      headers: requestHeaders(path, options.body),
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: options.signal
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error;
    throw asApiError(error);
  }

  if (!response.ok) {
    if (shouldTryLocalPairing(path, response.status)) {
      const paired = await requestLocalPairingToken();
      if (paired) {
        const retry = await fetch(path, {
          method: options.method || 'GET',
          cache: options.cache ?? (options.method ? 'no-store' : 'default'),
          credentials: 'same-origin',
          headers: requestHeaders(path, options.body),
          body: options.body === undefined ? undefined : JSON.stringify(options.body),
          signal: options.signal
        });
        if (retry.ok) return parseResponse<T>(retry);
        if (options.silentStatuses?.includes(retry.status)) {
          throw new ApiError(retry.status, retry.statusText);
        }
        const retryMessage = await retry.text().catch(() => retry.statusText);
        throw new ApiError(retry.status, retryMessage || retry.statusText);
      }
    }
    if (options.silentStatuses?.includes(response.status)) {
      throw new ApiError(response.status, response.statusText);
    }
    const message = await response.text().catch(() => response.statusText);
    throw new ApiError(response.status, message || response.statusText);
  }

  return parseResponse<T>(response);
}

export const api = {
  get: <T = unknown>(
    path: string,
    params?: Record<string, string | number | boolean | null | undefined>,
    signal?: AbortSignal,
    cache?: RequestCache
  ) => apiRequest<T>(withQuery(path, params), { signal, cache }),
  post: <T = unknown>(path: string, body?: unknown, silentStatuses?: number[]) =>
    apiRequest<T>(path, { method: 'POST', body, silentStatuses }),
  put: <T = unknown>(path: string, body?: unknown) => apiRequest<T>(path, { method: 'PUT', body }),
  delete: <T = unknown>(path: string) => apiRequest<T>(path, { method: 'DELETE' })
};

export const apiClient = createApiClient({ request: apiRequest });

async function uploadForm<T = unknown>(path: string, formData: FormData): Promise<T> {
  const response = await fetch(path, {
    method: 'POST',
    cache: 'no-store',
    credentials: 'same-origin',
    headers: requestHeaders(path),
    body: formData
  });

  if (!response.ok) {
    const message = await response.text().catch(() => response.statusText);
    throw new ApiError(response.status, message || response.statusText);
  }

  if (response.status === 204) return undefined as T;
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

export const endpoints = {
  status: () => apiClient.getStatus(),
  zoneStatus: (zoneId: string, signal?: AbortSignal) =>
    apiClient.getZonesByZoneIdStatus({ zone_id: zoneId }, { signal }),
  zones: () => apiClient.getZones(),
  selectZone: (zoneId: string) => api.post('/api/zones/select', { zone_id: zoneId }),
  enableZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/enable`),
  disableZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/disable`),
  renameZone: (zoneId: string, name: string) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/rename`, { name }),
  calibrateZone: (zoneId: string) => apiClient.postZonesByZoneIdCalibrate({ zone_id: zoneId }),
  updateZoneSettings: (zoneId: string, settings: unknown) =>
    api.post<JsonRecord>(`/api/zones/${encodeURIComponent(zoneId)}/settings`, settings),
  zoneHegelStatus: (zoneId: string, body: unknown) =>
    api.post<JsonRecord>(`/api/zones/${encodeURIComponent(zoneId)}/hegel/status`, body),
  devices: () => api.get<JsonRecord[]>('/api/devices'),
  profileRecentSearches: (profileId: string) =>
    api.get<{ searches: string[] }>(
      `/api/profiles/${encodeURIComponent(profileId)}/recent-searches`
    ),
  updateProfileRecentSearches: (profileId: string, searches: string[]) =>
    api.put<{ searches: string[] }>(
      `/api/profiles/${encodeURIComponent(profileId)}/recent-searches`,
      { searches }
    ),
  selectDevice: (name: string | null) => api.post('/api/select-device', { name }),
  updateConfig: (config: unknown) => api.post('/api/config', config),
  appearance: () => api.get<CustomDisplayFontSettings>('/api/appearance'),
  saveAppearance: (settings: unknown) =>
    api.post<CustomDisplayFontSettings>('/api/appearance', settings),
  remoteSettings: () => apiClient.getRemoteSettings({ silentStatuses: [401, 403, 404] }),
  remoteAccessStatus: () => apiClient.getRemoteStatus({ silentStatuses: [401, 403, 404] }),
  saveRemoteSettings: (settings: RemoteAccessSettingsUpdateRequest) =>
    apiClient.postRemoteSettings(settings, { silentStatuses: [401, 403, 404] }),
  createRemoteLinkCode: () => apiClient.postRemoteLinkCode({ silentStatuses: [401, 403, 404] }),
  remoteSessions: () => apiClient.getRemoteSessions({ silentStatuses: [401, 403, 404] }),
  revokeRemoteSession: (id: string) =>
    apiClient.postRemoteSessionsByIdRevoke(
      { id },
      {
        silentStatuses: [401, 403, 404]
      }
    ),
  exchangeRemoteSession: (code: string) =>
    apiClient.postRemoteSession({ code }, { silentStatuses: [401, 403, 404] }),
  exchangePairingSession: (pairingToken: string) =>
    apiClient.postSessionsBrowser(
      { pairing_token: pairingToken },
      { silentStatuses: [401, 403, 410, 429] }
    ),
  uploadDisplayFont: (font: File) => {
    const formData = new FormData();
    formData.append('font', font, font.name || 'display.ttf');
    return uploadForm<CustomDisplayFontSettings>('/api/appearance/display-font', formData);
  },
  play: (body: unknown) => api.post('/api/play', body),
  playZone: (zoneId: string, body: unknown) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/play`, body),
  playArtistRadio: (body: { artist_name: string; mode?: string }) =>
    api.post('/api/artist-radio/play', body),
  playArtistRadioZone: (zoneId: string, body: { artist_name: string; mode?: string }) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/artist-radio/play`, body),
  zoneQueue: (zoneId: string, body: unknown) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/queue`, body, [409]),
  shuffleZoneQueue: (zoneId: string, body: unknown) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/queue/shuffle`, body, [409]),
  pause: () => api.post('/api/pause'),
  pauseZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/pause`),
  resume: () => api.post('/api/resume'),
  resumeZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/resume`),
  stop: () => api.post('/api/stop'),
  stopZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/stop`),
  next: () => api.post('/api/next'),
  nextZone: (zoneId: string) => api.post(`/api/zones/${encodeURIComponent(zoneId)}/next`),
  seek: (seconds: number) => api.post('/api/seek', { seconds }),
  seekZone: (zoneId: string, seconds: number) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/seek`, { seconds }),
  loopMode: (zoneId: string, mode: string) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/loop-mode`, { mode }),
  volume: (volume: number) => api.post('/api/volume', { volume }),
  volumeZone: (zoneId: string, volume: number) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/volume`, { volume }),
  deviceVolume: (volume: number) => api.post('/api/device-volume', { volume }),
  deviceVolumeZone: (zoneId: string, volume: number) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/device-volume`, { volume }),
  updateZoneConfig: (zoneId: string, config: unknown) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/config`, config),
  zoneEq: (zoneId: string) => api.get<JsonRecord>(`/api/zones/${encodeURIComponent(zoneId)}/eq`),
  setZoneEq: (zoneId: string, config: unknown) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/eq`, config),

  summary: () => apiClient.getLibrarySummary(),
  albums: () => apiClient.getLibraryAlbums() as Promise<LibraryAlbum[]>,
  browseAlbums: (query: LibraryBrowseParams, signal?: AbortSignal) =>
    apiClient.getLibraryBrowseAlbums(browseQuery(query), { signal }) as Promise<
      LibraryBrowsePage<LibraryAlbum>
    >,
  browseTracks: (query: LibraryBrowseParams, signal?: AbortSignal) =>
    apiClient.getLibraryBrowseTracks(browseQuery(query), { signal }) as Promise<
      LibraryBrowsePage<LibraryTrack>
    >,
  browseArtists: (query: LibraryBrowseParams, signal?: AbortSignal) =>
    apiClient.getLibraryBrowseArtists(browseQuery(query), { signal }) as Promise<
      LibraryBrowsePage<JsonRecord>
    >,
  album: (id: string | number) =>
    api.get<JsonRecord>(`/api/library/albums/${encodeURIComponent(String(id))}`),
  autoMetaRun: (body: { link_qobuz: boolean; mode?: string }) =>
    api.post<AutoMetaProgress>('/api/library/autometa/run', body, [409]),
  autoMetaJob: (body: { link_qobuz: boolean; mode?: string }) =>
    api.post<AutoMetaProgress>('/api/library/autometa/jobs', body, [409]),
  autoMetaProgress: () => api.get<AutoMetaProgress>('/api/library/autometa/progress'),
  autoMetaStatus: () => api.get<AutoMetaProgress>('/api/library/autometa/status'),
  autoMetaPause: (jobId: string | number) =>
    api.post<AutoMetaProgress>(
      `/api/library/autometa/jobs/${encodeURIComponent(String(jobId))}/pause`
    ),
  autoMetaResume: (jobId: string | number) =>
    api.post<AutoMetaProgress>(
      `/api/library/autometa/jobs/${encodeURIComponent(String(jobId))}/resume`
    ),
  autoMetaStop: (jobId: string | number) =>
    api.post<AutoMetaProgress>(
      `/api/library/autometa/jobs/${encodeURIComponent(String(jobId))}/stop`
    ),
  autoMetaItems: (jobId: string | number, status?: string) =>
    api.get<AutoMetaJobItem[]>(
      `/api/library/autometa/jobs/${encodeURIComponent(String(jobId))}/items${status ? `?status=${encodeURIComponent(status)}` : ''}`
    ),
  autoMetaAudit: () => api.get<JsonRecord[]>('/api/library/autometa/audit'),
  albumByQobuzId: (id: string | number) =>
    api.get<JsonRecord>(`/api/library/qobuz-albums/${encodeURIComponent(String(id))}`),
  albumQobuzLink: (albumId: string | number, qobuzAlbumId: string | number) =>
    api.post<JsonRecord>(`/api/library/albums/${encodeURIComponent(String(albumId))}/qobuz/link`, {
      qobuz_album_id: qobuzAlbumId
    }),
  albumQobuzUnlink: (albumId: string | number) =>
    api.post<JsonRecord>(`/api/library/albums/${encodeURIComponent(String(albumId))}/qobuz/unlink`),
  albumQobuzCreditsRefresh: (albumId: string | number) =>
    api.post<JsonRecord>(
      `/api/library/albums/${encodeURIComponent(String(albumId))}/qobuz/credits/refresh`
    ),
  uploadAlbumCover: (albumId: string | number, cover: File) => {
    const formData = new FormData();
    formData.append('cover', cover, cover.name || 'cover');
    return uploadForm<JsonRecord>(
      `/api/library/albums/${encodeURIComponent(String(albumId))}/cover`,
      formData
    );
  },
  albumVersionPrimary: (albumId: string | number, versionId: string | number) =>
    api.post<JsonRecord>(
      `/api/library/albums/${encodeURIComponent(String(albumId))}/versions/${encodeURIComponent(String(versionId))}/primary`
    ),
  albumPlaySources: (id: string | number, startIndex = 0, shuffle = false, versionId?: number) =>
    api.post<{ sources?: ResolvedPlaySource[] }>(
      `/api/library/albums/${encodeURIComponent(String(id))}/play-sources`,
      {
        start_index: startIndex,
        shuffle,
        ...(versionId === undefined ? {} : { version_id: versionId })
      }
    ),
  favoriteAlbums: () => api.get<LibraryAlbum[]>('/api/library/favorite-albums'),
  addFavoriteAlbum: (album: unknown) =>
    api.post<LibraryAlbum>('/api/library/favorite-albums', album),
  removeFavoriteAlbum: (album: unknown) =>
    apiRequest<JsonRecord>('/api/library/favorite-albums', { method: 'DELETE', body: album }),
  tracks: () => apiClient.getLibraryTracks() as Promise<LibraryTrack[]>,
  artists: () => apiClient.getLibraryArtists() as Promise<JsonRecord[]>,
  warmArtistProfileImageCache: (limit: number, signal?: AbortSignal) =>
    apiRequest<JsonRecord>('/api/library/artists/profile-image-cache/warm', {
      method: 'POST',
      body: { limit },
      signal
    }),
  librarySearch: (q: string, signal?: AbortSignal) =>
    apiClient.getLibrarySearch({ q }, { signal }) as Promise<LibrarySearchResponse>,
  folders: () => apiClient.getLibraryFolders(),
  addFolder: (path: string) => api.post<LibraryFoldersResponse>('/api/library/folders', { path }),
  removeFolder: (path: string) =>
    apiRequest<LibraryFoldersResponse>('/api/library/folders', {
      method: 'DELETE',
      body: { path }
    }),
  pickFolder: () => api.post<{ path: string | null }>('/api/library/folders/pick'),
  rescanStatus: () => api.get<JsonRecord>('/api/library/rescan/status'),
  rescan: () => api.post<JsonRecord>('/api/library/rescan'),
  artUrl: (id?: string | number | null, size?: number) =>
    id === undefined || id === null || id === ''
      ? null
      : withQuery(`/api/library/art/${encodeURIComponent(String(id))}`, { size }),

  playlists: () => apiClient.getPlaylists() as Promise<Playlist[]>,
  savePlaylist: (id: string, playlist: Partial<Playlist>) =>
    api.put<Playlist>(`/api/playlists/${encodeURIComponent(id)}`, playlist),
  deletePlaylist: (id: string) => api.delete(`/api/playlists/${encodeURIComponent(id)}`),
  recordPlaylist: (id: string) => api.post(`/api/playlists/${encodeURIComponent(id)}/played`),

  profiles: () => apiClient.getProfiles(),
  createProfile: (name: string) => api.post<ProfilesResponse>('/api/profiles', { name }),
  selectProfile: (profileId: string) =>
    api.post<ProfilesResponse>('/api/profiles/select', {
      profile_id: profileId
    }),
  updateProfile: (profileId: string, name: string, color: string, image?: string | null) =>
    api.put<ProfilesResponse>(`/api/profiles/${encodeURIComponent(profileId)}`, {
      name,
      color,
      image
    }),
  deleteProfile: (profileId: string) =>
    api.delete<ProfilesResponse>(`/api/profiles/${encodeURIComponent(profileId)}`),
  historyStats: (range: string) => apiClient.getHistoryStats({ range }),
  recentHistory: (limit = 30, excludeRadio = false) =>
    apiClient.getHistoryRecent({ limit, exclude_radio: excludeRadio }),
  recentAlbums: (limit = 50) => apiClient.getLibraryRecentAlbums({ limit }),
  recentPlaylists: (limit = 50) => apiClient.getPlaylistsRecent({ limit }),

  qobuzStatus: () => apiClient.getQobuzStatus(),
  qobuzHome: () => apiClient.getQobuzHome(),
  qobuzHomeSection: (
    category: string,
    genreId?: string | number | null,
    limit = 12,
    offset = 0,
    signal?: AbortSignal
  ) =>
    api.get<JsonRecord>(
      '/api/qobuz/home/section',
      { category, genre_id: genreId ?? null, limit, offset },
      signal
    ),
  qobuzHomeAlbumOfTheWeek: () => api.get<JsonRecord>('/api/qobuz/home/album-of-the-week'),
  qobuzFeaturedPlaylists: (
    limit = 12,
    offset = 0,
    genreId?: string | number | null,
    tag?: string | null,
    signal?: AbortSignal
  ) =>
    api.get<QobuzFeaturedPlaylistsResponse>(
      '/api/qobuz/playlists/featured',
      { limit, offset, genre_id: genreId ?? null, tag: tag || null },
      signal
    ),
  qobuzPlaylistTags: (signal?: AbortSignal) =>
    api.get<JsonRecord[]>('/api/qobuz/playlists/tags', undefined, signal),
  qobuzGenres: (signal?: AbortSignal) =>
    api.get<JsonRecord[]>('/api/qobuz/genres', undefined, signal),
  qobuzPlaylist: (id: string | number, signal?: AbortSignal) =>
    api.get<JsonRecord>(
      `/api/qobuz/playlists/${encodeURIComponent(String(id))}`,
      undefined,
      signal
    ),
  qobuzSearch: (q: string, signal?: AbortSignal) => apiClient.getQobuzSearch({ q }, { signal }),
  qobuzAlbumSearch: (q: string, signal?: AbortSignal) =>
    api.get<LibraryAlbum[]>('/api/qobuz/search/albums', { q }, signal),
  qobuzAlbums: () => api.get<LibraryAlbum[]>('/api/qobuz/albums'),
  qobuzAlbum: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/albums/${encodeURIComponent(String(id))}`),
  qobuzTrack: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/tracks/${encodeURIComponent(String(id))}`),
  qobuzArtistSearch: (q: string, limit = 3, signal?: AbortSignal) =>
    api.get<JsonRecord>('/api/qobuz/artists/search', { q, limit }, signal),
  qobuzArtistImage: (q: string, signal?: AbortSignal) =>
    api.get<JsonRecord>('/api/qobuz/artists/image', { q }, signal),
  qobuzArtist: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/artists/${encodeURIComponent(String(id))}`),
  qobuzArtistCore: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/artists/${encodeURIComponent(String(id))}/core`),
  qobuzArtistTopTracks: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/artists/${encodeURIComponent(String(id))}/top-tracks`),
  qobuzArtistSimilar: (id: string | number) =>
    api.get<JsonRecord>(`/api/qobuz/artists/${encodeURIComponent(String(id))}/similar`),
  qobuzPlay: (track: QobuzTrack, queue: QobuzTrack[] = [], options: QobuzPlaybackOptions = {}) =>
    api.post('/api/qobuz/play', {
      track_id: Number(track.id ?? track.track_id),
      title: track.title || null,
      artist: track.artist || null,
      album: track.album || null,
      album_id: track.album_id || null,
      image_url: track.image_url || null,
      duration_secs: Number(track.duration_secs ?? track.duration ?? 0) || null,
      replace_current: true,
      ...(track.playlist_context ? { playlist_context: track.playlist_context } : {}),
      ...(track.format_id ? { format_id: track.format_id } : {}),
      ...(options.expectedCurrent ? { expected_current: options.expectedCurrent } : {}),
      ...(options.radioAuto ? { radio_auto: true } : {}),
      queue: queue
        .map((item) => ({
          track_id: Number(item.id ?? item.track_id),
          title: item.title || null,
          artist: item.artist || null,
          album: item.album || null,
          album_id: item.album_id || null,
          image_url: item.image_url || null,
          duration_secs: Number(item.duration_secs ?? item.duration ?? 0) || null,
          radio: Boolean(item.radio || options.radioAuto),
          ...(item.playlist_context ? { playlist_context: item.playlist_context } : {}),
          ...(item.format_id ? { format_id: item.format_id } : {})
        }))
        .filter((item) => Number.isFinite(item.track_id) && item.track_id > 0)
    }),
  qobuzPlayZone: (
    zoneId: string,
    track: QobuzTrack,
    queue: QobuzTrack[] = [],
    options: QobuzPlaybackOptions = {}
  ) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/qobuz/play`, {
      track_id: Number(track.id ?? track.track_id),
      title: track.title || null,
      artist: track.artist || null,
      album: track.album || null,
      album_id: track.album_id || null,
      image_url: track.image_url || null,
      duration_secs: Number(track.duration_secs ?? track.duration ?? 0) || null,
      replace_current: true,
      ...(track.playlist_context ? { playlist_context: track.playlist_context } : {}),
      ...(track.format_id ? { format_id: track.format_id } : {}),
      ...(options.expectedCurrent ? { expected_current: options.expectedCurrent } : {}),
      ...(options.radioAuto ? { radio_auto: true } : {}),
      queue: queue
        .map((item) => ({
          track_id: Number(item.id ?? item.track_id),
          title: item.title || null,
          artist: item.artist || null,
          album: item.album || null,
          album_id: item.album_id || null,
          image_url: item.image_url || null,
          duration_secs: Number(item.duration_secs ?? item.duration ?? 0) || null,
          radio: Boolean(item.radio || options.radioAuto),
          ...(item.playlist_context ? { playlist_context: item.playlist_context } : {}),
          ...(item.format_id ? { format_id: item.format_id } : {})
        }))
        .filter((item) => Number.isFinite(item.track_id) && item.track_id > 0)
    }),
  qobuzPrefetch: (
    track: QobuzTrack,
    expectedCurrent: string | null = null,
    options: QobuzPlaybackOptions = {}
  ) =>
    api.post(
      '/api/qobuz/prefetch',
      {
        track_id: Number(track.id ?? track.track_id),
        title: track.title || null,
        artist: track.artist || null,
        album: track.album || null,
        album_id: track.album_id || null,
        image_url: track.image_url || null,
        duration_secs: Number(track.duration_secs ?? track.duration ?? 0) || null,
        ...(track.format_id ? { format_id: track.format_id } : {}),
        ...(expectedCurrent ? { expected_current: expectedCurrent } : {}),
        ...(options.radioAuto ? { radio_auto: true } : {})
      },
      [409]
    ),
  qobuzPrefetchZone: (
    zoneId: string,
    track: QobuzTrack,
    expectedCurrent: string | null = null,
    options: QobuzPlaybackOptions = {}
  ) =>
    api.post(
      `/api/zones/${encodeURIComponent(zoneId)}/qobuz/prefetch`,
      {
        track_id: Number(track.id ?? track.track_id),
        title: track.title || null,
        artist: track.artist || null,
        album: track.album || null,
        album_id: track.album_id || null,
        image_url: track.image_url || null,
        duration_secs: Number(track.duration_secs ?? track.duration ?? 0) || null,
        ...(track.format_id ? { format_id: track.format_id } : {}),
        ...(expectedCurrent ? { expected_current: expectedCurrent } : {}),
        ...(options.radioAuto ? { radio_auto: true } : {})
      },
      [409]
    ),
  qobuzRadioNext: (body: {
    seed_track_id?: number;
    seed_artist_name?: string;
    exclude_track_ids?: number[];
    limit?: number;
  }) => api.post<JsonRecord | undefined>('/api/qobuz/radio/next', body),
  qobuzSettings: () => api.get<JsonRecord>('/api/qobuz/settings'),
  saveQobuzSettings: (settings: unknown) => api.post<JsonRecord>('/api/qobuz/settings', settings),
  qobuzLogout: () => api.post<JsonRecord>('/api/qobuz/logout'),
  qobuzCache: () => api.get<JsonRecord>('/api/qobuz/cache'),
  clearQobuzCache: () => api.post('/api/qobuz/cache/clear'),
  lastfmStatus: () => api.get<JsonRecord>('/api/lastfm/status'),
  saveLastfmSettings: (settings: unknown) => api.post<JsonRecord>('/api/lastfm/settings', settings),
  appleMusicCaptureStatus: () => api.get<JsonRecord>('/api/apple-music-capture/status'),
  appleMusicCaptureSettings: () => api.get<JsonRecord>('/api/apple-music-capture/settings'),
  saveAppleMusicCaptureSettings: (settings: unknown) =>
    api.post<JsonRecord>('/api/apple-music-capture/settings', settings),
  appleMusicCaptureDevices: () => api.get<JsonRecord>('/api/apple-music-capture/devices'),
  startAppleMusicCapture: (settings: unknown) =>
    api.post<JsonRecord>('/api/apple-music-capture/start', settings),
  stopAppleMusicCapture: () => api.post<JsonRecord>('/api/apple-music-capture/stop'),
  setAppleMusicCaptureRate: (rateHz: number) =>
    api.post<JsonRecord>('/api/apple-music-capture/rate', { rate_hz: rateHz }),
  appleMusicCaptureMetrics: () => api.get<JsonRecord>('/api/apple-music-capture/metrics'),
  appleMusicAppStatus: () => api.get<JsonRecord>('/api/apple-music-capture/music-app/status'),
  controlAppleMusicApp: (command: string) =>
    api.post<JsonRecord>('/api/apple-music-capture/music-app/control', { command }),
  appleMusicStatus: () =>
    api.get<JsonRecord>('/api/apple-music/status', undefined, undefined, 'no-store'),
  launchAppleMusicHelper: () => api.post<JsonRecord>('/api/apple-music/launch'),
  authorizeAppleMusic: () =>
    api.post<JsonRecord>('/api/apple-music/authorize', { present_ui: true }),
  playAppleMusicSong: (songId: string, storefront?: string) =>
    api.post<JsonRecord>('/api/apple-music/dev/play-song', {
      song_id: songId,
      storefront: storefront || null
    }),
  controlAppleMusic: (command: string) =>
    api.post<JsonRecord>('/api/apple-music/transport', { command }),
  stopAppleMusic: () => api.post<JsonRecord>('/api/apple-music/stop'),
  shutdownAppleMusicHelper: () => api.post<JsonRecord>('/api/apple-music/shutdown'),
  startAppleMusicProcessTap: (confirmSystemAudioCapture: boolean, muteOriginalAudio = true) =>
    api.post<JsonRecord>('/api/apple-music/process-tap/start', {
      confirm_system_audio_capture: confirmSystemAudioCapture,
      mute_original_audio: muteOriginalAudio
    }),
  stopAppleMusicProcessTap: () => api.post<JsonRecord>('/api/apple-music/process-tap/stop'),

  nowPlayingQueue: (zoneId: string, signal?: AbortSignal) =>
    api.get<{
      state?: QueueState | null;
      current_source?: SourceRef | null;
      queued_sources?: SourceRef[];
    }>(`/api/zones/${encodeURIComponent(zoneId)}/now-playing-queue`, undefined, signal, 'no-store'),
  saveNowPlayingQueue: (zoneId: string, state: QueueState) =>
    api.post(`/api/zones/${encodeURIComponent(zoneId)}/now-playing-queue`, { state }),

  eq: () => api.get<JsonRecord>('/api/eq'),
  setEq: (config: unknown) => api.post('/api/eq', config),
  eqPresets: () => api.get<JsonRecord[]>('/api/eq/presets'),
  eqPreset: (name: string) => api.get<JsonRecord>(`/api/eq/presets/${encodeURIComponent(name)}`),
  saveEqPreset: (preset: unknown) => api.post('/api/eq/presets', preset),
  deleteEqPreset: (name: string) => api.delete(`/api/eq/presets/${encodeURIComponent(name)}`),
  hegelSettings: () => api.get<JsonRecord>('/api/hegel/settings'),
  saveHegelSettings: (settings: unknown) => api.post('/api/hegel/settings', settings),
  hegelStatus: (body: unknown) => api.post<JsonRecord>('/api/hegel/status', body),
  hegelPower: (body: unknown) => api.post<JsonRecord>('/api/hegel/power', body),
  hegelInput: (body: unknown) => api.post<JsonRecord>('/api/hegel/input', body),
  hegelVolume: (body: unknown) => api.post<JsonRecord>('/api/hegel/volume', body),
  hegelMute: (body: unknown) => api.post<JsonRecord>('/api/hegel/mute', body)
};
