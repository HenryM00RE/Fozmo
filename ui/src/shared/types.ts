import type {
  LibraryBrowseFacets as GeneratedLibraryBrowseFacets,
  QobuzAlbumPageResponse as GeneratedQobuzAlbumPageResponse,
  QobuzFeaturedPlaylistsResponse as GeneratedQobuzFeaturedPlaylistsResponse
} from './generated/api-types';

export type JsonRecord = Record<string, unknown>;

export type CustomDisplayFontSettings = JsonRecord & {
  custom_display_font_enabled?: boolean;
  custom_display_font_scale_percent?: number;
  custom_display_font_name?: string | null;
  custom_display_font_url?: string | null;
  custom_display_font_version?: number;
  custom_display_font_supported_ranges?: [number, number][];
};

export type {
  AlbumSummary,
  LibraryBrowsePageFor_AlbumSummary,
  LibraryBrowsePageFor_ArtistSummary,
  LibraryBrowsePageFor_TrackSummary,
  LibraryFoldersResponse,
  LibrarySearchResponse,
  LibrarySummary,
  PairingRevocationResponse,
  PairingStartResponse,
  PlaylistSummary,
  ProfilesResponse,
  QobuzAlbum,
  QobuzHomeResponse,
  QobuzPlaylist,
  QobuzStatusResponse as QobuzStatus,
  RemoteAccessSettingsDto,
  RemoteAccessSettingsResponse,
  RemoteAccessSettingsUpdateRequest,
  RemoteAccessStatus,
  RemoteLinkCodeResponse,
  RemoteSessionMetadataDto,
  RemoteSessionRequest,
  RemoteSessionResponse,
  RemoteSessionRevocationResponse,
  RemoteSessionsResponse,
  StatusResponse,
  TrackSummary,
  ZoneProfile as ZoneProfileContract
} from './generated/api-types';

export type QobuzFeaturedPlaylistsResponse = Omit<
  GeneratedQobuzFeaturedPlaylistsResponse,
  'has_more' | 'playlists' | 'total'
> & {
  has_more: boolean;
  playlists: JsonRecord[];
  total: number | null;
};
export type QobuzAlbumPageResponse = Omit<
  GeneratedQobuzAlbumPageResponse,
  'albums' | 'has_more' | 'total'
> & {
  albums: JsonRecord[];
  has_more: boolean;
  total: number | null;
};

export type ViewId =
  | 'home'
  | 'discover'
  | 'library'
  | 'history'
  | 'albums'
  | 'album'
  | 'songs'
  | 'artists'
  | 'artist'
  | 'qobuz-album'
  | 'qobuz-playlist'
  | 'playlists'
  | 'playlist'
  | 'settings';

export interface RouteState {
  view: ViewId;
  id?: string | number | null;
  title?: string | null;
  albumHint?: LibraryAlbum | null;
}

export interface LibraryTrack extends JsonRecord {
  id?: number;
  track_id?: number;
  file_name?: string;
  name?: string;
  title?: string;
  artist?: string;
  album?: string;
  album_artist?: string;
  album_id?: number | string | null;
  art_id?: number | string | null;
  image_url?: string | null;
  duration_secs?: number;
  sample_rate?: number | null;
  bit_depth?: number | null;
  qobuz_track?: QobuzTrack | JsonRecord | null;
  play_source?: ResolvedPlaySource | null;
  qobuz_source?: ResolvedPlaySource | null;
}

export interface LibraryAlbum extends JsonRecord {
  id?: number | string;
  title?: string;
  album_artist?: string;
  artist?: string;
  version?: string | null;
  image_url?: string | null;
  cover_url?: string | null;
  art_id?: number | string | null;
  tracks?: LibraryTrack[];
  qobuz_album_versions?: JsonRecord[];
}

export type LibraryBrowseSort = 'popularity' | 'name' | 'releaseDate' | 'albums' | 'songs';
export type LibraryBrowseDirection = 'asc' | 'desc';
export type LibraryBrowseKind = 'albums' | 'tracks' | 'artists';

export interface LibraryFacetOption extends JsonRecord {
  value: string;
  label: string;
  count: number;
}

export type LibraryBrowseFacets = Partial<GeneratedLibraryBrowseFacets> &
  JsonRecord & {
    genres?: LibraryFacetOption[];
    decades?: LibraryFacetOption[];
    qualities?: LibraryFacetOption[];
    sources?: LibraryFacetOption[];
  };

export interface LibraryBrowseParams {
  q?: string;
  limit?: number;
  offset?: number;
  sort?: LibraryBrowseSort;
  direction?: LibraryBrowseDirection;
  genre?: string | null;
  decade?: string | number | null;
  quality?: string | null;
  source?: string | null;
  include_facets?: boolean;
}

export interface LibraryBrowsePage<T> extends JsonRecord {
  items: T[];
  total: number;
  limit: number;
  offset: number;
  has_more?: boolean;
  facets?: LibraryBrowseFacets;
}

export interface QobuzTrack extends JsonRecord {
  id?: number;
  track_id?: number;
  title?: string;
  artist?: string;
  album?: string;
  album_id?: string | number | null;
  image_url?: string | null;
  duration?: number;
  duration_secs?: number;
  format_id?: number | string | null;
  radio?: boolean;
  playlist_context?: PlaylistPlaybackContext | null;
}

export interface PlaylistPlaybackContext {
  playlist_id: string;
}

export interface SourceRef extends JsonRecord {
  kind?: 'local_track' | 'qobuz_track' | 'local' | 'qobuz' | string;
  track_id?: number;
  file_name?: string | null;
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  album_artist?: string | null;
  album_id?: string | number | null;
  art_id?: string | number | null;
  image_url?: string | null;
  duration_secs?: number | null;
  format_id?: string | number | null;
  radio?: boolean | null;
  playlist_context?: PlaylistPlaybackContext | null;
}

export interface ResolvedPlaySource extends JsonRecord {
  kind?: 'local' | 'qobuz' | string;
  track_id?: number;
  title?: string | null;
  artist?: string | null;
  album?: string | null;
  album_artist?: string | null;
  album_id?: string | number | null;
  art_id?: string | number | null;
  image_url?: string | null;
  duration_secs?: number | null;
  format_id?: string | number | null;
  file_name?: string | null;
}

export interface QueueItem extends JsonRecord {
  title: string;
  artist: string;
  album: string;
  albumArtist?: string;
  albumId?: string | number | null;
  artId?: string | number | null;
  imageUrl?: string | null;
  durationSecs: number;
  filename?: string | null;
  ref?: { track_id?: number; file_name?: string | null };
  qobuzTrack?: QobuzTrack;
  resolvedSource?: SourceRef;
  radio?: boolean;
  playlistContext?: PlaylistPlaybackContext | null;
}

export type QueueKind = 'local' | 'qobuz' | 'mixed' | null;
export type LoopMode = 'off' | 'loop';

export interface QueueState {
  kind: QueueKind;
  cursor: number;
  items: QueueItem[];
  loopMode: LoopMode;
}

export interface ZoneProfile extends JsonRecord {
  id: string;
  name: string;
  protocol?: string;
  capabilities?: JsonRecord & {
    max_sample_rate?: number;
    max_bit_depth?: number;
    max_dsd_rate?: number | null;
    supports_dsd128?: boolean;
    supports_dsd256?: boolean;
    capability_detection_source?: string;
    capability_detection_status?: string;
    capability_detection_message?: string | null;
  };
  enabled?: boolean;
  device_name?: string | null;
  icon?: string | null;
  device_type?: string | null;
  hegel?: JsonRecord | null;
  upnp_calibrated_capabilities?:
    | (JsonRecord & {
        max_sample_rate?: number;
        max_bit_depth?: number;
        max_dsd_rate?: number | null;
      })
    | null;
  status?: string;
  playing_state?: string | null;
  track_title?: string | null;
}

export interface Playlist extends JsonRecord {
  id: string;
  name: string;
  description?: string | null;
  items?: QueueItem[];
  createdAt?: number | string | null;
  created_at?: number | string | null;
  updatedAt?: number | string | null;
  updated_at?: number | string | null;
}
