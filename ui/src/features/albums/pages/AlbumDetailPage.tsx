import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { endpoints } from '../../../shared/lib/api';
import {
  applyQobuzVersionToQobuzTracks,
  descriptionParagraphs,
  formatAlbumDate,
  formatLongDuration,
  idValue,
  normalizeQobuzAlbumId,
  orderAlbumTracks,
  plainDescription,
  positiveNumber,
  qobuzFormatIdForVersion,
  qobuzTrackFromAlbumTrack,
  resolveViewingVersion,
  safeArray,
  shuffled,
  titleOf
} from '../../../shared/lib/appSupport';
import {
  localTrackToQueueItem,
  qobuzTrackToQueueItem,
  resolvedPlaySourceToQueueItem
} from '../../../shared/lib/queue';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type {
  JsonRecord,
  LibraryAlbum,
  LibraryTrack,
  QobuzTrack,
  QueueItem,
  ResolvedPlaySource
} from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { actionMenuPosition } from '../../../shared/ui/menuPosition';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { PlayNextIcon } from '../../../shared/ui/PlayNextIcon';
import { useActionMenuScrollLock } from '../../../shared/ui/useActionMenuScrollLock';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import { loadQobuzAlbumDetail } from '../../qobuz/model/qobuzData';
import { AlbumCreditsPanel } from '../components/AlbumCreditsPanel';
import { AlbumDescriptionModal } from '../components/AlbumDescriptionModal';
import { AlbumDetailHeader } from '../components/AlbumDetailHeader';
import { AlbumMetadataEditorModal } from '../components/AlbumMetadataEditorModal';
import { AlbumTrackList } from '../components/AlbumTrackList';
import { AlbumVersionsPanel } from '../components/AlbumVersionsPanel';
import {
  addFavoriteAlbumCached,
  loadAlbumDetailCached,
  loadFavoriteAlbumsCached,
  removeFavoriteAlbumCached,
  updateAlbumDetailCache
} from '../model/albumData';
import { favoriteAlbumKey, favoriteAlbumPayload } from '../model/albumFavorites';
import {
  type AlbumSelectionItem,
  albumArtworkForViewingVersion,
  albumTrackSelectionKeyForQueueItem
} from '../model/albumModel';
import { localTracksWithLinkedQobuzMetadata } from '../model/linkedQobuzMetadata';

function versionRowIdentity(version: JsonRecord) {
  const rawId = String(idValue(version.id));
  if (version.provider === 'qobuz' && rawId.startsWith('qobuz:')) return rawId;
  const qobuzId = normalizeQobuzAlbumId(
    idValue(version.open_album_id, version.qobuz_album_id, version.provider_id, rawId)
  );
  if (qobuzId) return `qobuz:${versionIdentityTier(version)}:${qobuzId}`;
  if (version.id !== null && version.id !== undefined) return `id:${version.id}`;
  return `${version.provider || ''}:${version.title || ''}:${version.version || ''}`;
}

function versionIdentityTier(version: JsonRecord) {
  const tier = String(version.tier || '')
    .trim()
    .toLowerCase();
  if (tier) return tier;
  const label = String(version.source_label || version.version || '').toLowerCase();
  const sampleRate = Number(version.sample_rate) || 0;
  const bitDepth = Number(version.bit_depth) || 0;
  if (label.includes('hi-res') || label.includes('hi res') || bitDepth >= 24 || sampleRate > 48_000)
    return 'hires';
  if (
    label.includes('cd') ||
    (bitDepth > 0 && bitDepth <= 16 && sampleRate > 0 && sampleRate <= 48_000)
  )
    return 'cd';
  return 'album';
}

function mergeAlbumVersionRows(current: JsonRecord[], additions: JsonRecord[]) {
  const seen = new Set(current.map(versionRowIdentity));
  const next = [...current];
  additions.forEach((version) => {
    const key = versionRowIdentity(version);
    if (seen.has(key)) return;
    seen.add(key);
    next.push(version);
  });
  return next;
}

function detailDescription(detail: JsonRecord | null | undefined) {
  const album = (detail?.album || detail) as JsonRecord | null | undefined;
  const canonical = detail?.canonical_album as JsonRecord | null | undefined;
  return String(album?.description || canonical?.description || detail?.description || '').trim();
}

function applyDetailDescription(detail: JsonRecord, description: string) {
  if (!description || detailDescription(detail)) return detail;
  if (detail.album && typeof detail.album === 'object') {
    return {
      ...detail,
      album: {
        ...(detail.album as JsonRecord),
        description
      }
    };
  }
  return {
    ...detail,
    description
  };
}

function qobuzAlbumViewArt(art: string | null, remoteSurface = false) {
  if (remoteSurface) return art;
  const prefix = 'https://static.qobuz.com/images/covers/';
  if (!art?.toLowerCase().startsWith(prefix)) return art;
  const lower = art.toLowerCase();
  if (lower.endsWith('_org.jpg') || lower.endsWith('_max.jpg')) return art;
  if (!lower.endsWith('.jpg')) return art;
  const withoutExt = art.slice(0, -'.jpg'.length);
  const suffixStart = withoutExt.lastIndexOf('_');
  if (suffixStart < 0) return art;
  const suffix = withoutExt.slice(suffixStart + 1);
  if (!/^\d+$/.test(suffix)) return art;
  return `${withoutExt.slice(0, suffixStart)}_org.jpg`;
}

function qobuzVersionTracksFromCanonical(
  detail: JsonRecord | null | undefined,
  version: JsonRecord,
  fallbackTracks: LibraryTrack[]
) {
  const canonicalTracks = safeArray<LibraryTrack>(detail?.canonical_tracks);
  if (!canonicalTracks.length) return fallbackTracks;
  const qobuzAlbumId = idValue(
    (detail?.canonical_album as JsonRecord | undefined)?.qobuz_album_id,
    (detail?.album as JsonRecord | undefined)?.qobuz_album_id,
    (detail?.album as JsonRecord | undefined)?.qobuz_id
  );
  const formatId = qobuzFormatIdForVersion(version);
  return orderAlbumTracks(
    canonicalTracks.map((track, index) => {
      const qobuzSource = (track.qobuz_source || track.play_source) as JsonRecord | undefined;
      const qobuzTrackId =
        positiveNumber(track.qobuz_track_id) ||
        positiveNumber(qobuzSource?.track_id) ||
        positiveNumber(track.id);
      return {
        ...track,
        id: qobuzTrackId || index + 1,
        track_id: qobuzTrackId || undefined,
        album_id: qobuzAlbumId || track.album_id,
        sample_rate: version.sample_rate || track.sample_rate,
        bit_depth: version.bit_depth || track.bit_depth,
        format: version.format || track.format || 'FLAC',
        qobuz_track: {
          id: qobuzTrackId || track.qobuz_track_id,
          track_id: qobuzTrackId || track.qobuz_track_id,
          title: track.title,
          artist: track.artist,
          album: track.album,
          album_id: qobuzAlbumId || track.album_id,
          image_url: track.image_url,
          duration_secs: track.duration_secs,
          duration: track.duration_secs,
          format_id: formatId
        },
        play_source: qobuzSource
          ? {
              ...qobuzSource,
              format_id: formatId
            }
          : track.play_source
      } as LibraryTrack;
    })
  );
}

function AlbumDetailSkeleton() {
  const rows = Array.from({ length: 7 }, (_, index) => index);
  return (
    <section className="view album-detail-view">
      <div
        className="album-detail react-album-detail-loading"
        role="status"
        aria-label="Loading album"
        aria-busy="true"
      >
        <div className="album-detail-skeleton-header" aria-hidden="true">
          <span className="album-skeleton-cover skeleton-shimmer" />
          <div className="album-skeleton-info">
            <span className="album-skeleton-date skeleton-shimmer" />
            <span className="album-skeleton-title skeleton-shimmer" />
            <span className="album-skeleton-artist skeleton-shimmer" />
            <span className="album-skeleton-description skeleton-shimmer" />
            <span className="album-skeleton-description is-wide skeleton-shimmer" />
            <span className="album-skeleton-description is-short skeleton-shimmer" />
            <div className="album-skeleton-actions">
              <span className="album-skeleton-action is-primary skeleton-shimmer" />
              <span className="album-skeleton-action skeleton-shimmer" />
              <span className="album-skeleton-action is-icon skeleton-shimmer" />
              <span className="album-skeleton-quality skeleton-shimmer" />
            </div>
          </div>
        </div>
        <div className="segmented album-skeleton-tabs" aria-hidden="true">
          <span className="skeleton-shimmer" />
          <span className="skeleton-shimmer" />
          <span className="skeleton-shimmer" />
        </div>
        <section
          className="playlist-panel album-track-panel album-skeleton-track-panel"
          aria-hidden="true"
        >
          <ul className="file-list song-list album-skeleton-track-list">
            {rows.map((index) => (
              <li className="file-item album-track-item album-skeleton-track" key={index}>
                <span className="album-skeleton-track-dot skeleton-shimmer" />
                <span className="album-skeleton-track-title skeleton-shimmer" />
                <span className="album-skeleton-track-duration skeleton-shimmer" />
                <span className="album-skeleton-track-count skeleton-shimmer" />
                <span className="album-skeleton-track-more skeleton-shimmer" />
              </li>
            ))}
          </ul>
        </section>
      </div>
    </section>
  );
}

function AlbumArtLightbox({
  art,
  title,
  onClose
}: {
  art: string;
  title: string;
  onClose: () => void;
}) {
  const [screenCover, setScreenCover] = useState(false);

  useEffect(() => {
    document.body.classList.add('album-art-open');
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.body.classList.remove('album-art-open');
      document.removeEventListener('keydown', onKey);
    };
  }, [onClose]);

  if (typeof document === 'undefined') return null;

  return createPortal(
    <div
      className={`album-art-lightbox${screenCover ? ' is-screen-cover' : ''}`}
      role="dialog"
      aria-modal="true"
      aria-label={`${title} artwork`}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <button
        className="album-art-lightbox-close"
        type="button"
        aria-label="Close artwork"
        onClick={onClose}
      >
        <Icon path="M18 6 6 18M6 6l12 12" />
      </button>
      <button
        className="album-art-lightbox-frame"
        type="button"
        aria-label={screenCover ? 'Fit artwork to screen' : 'Fill screen with artwork'}
        aria-pressed={screenCover}
        onClick={() => setScreenCover((current) => !current)}
      >
        <img alt={`${title} artwork`} src={art} />
      </button>
    </div>,
    document.body
  );
}

export function AlbumDetailPage({
  id,
  playAlbum,
  onOpenArtist,
  onOpenLocalAlbum,
  onOpenQobuzAlbum,
  addItemsToQueue,
  providedDetail,
  kind = 'local',
  showQobuzStamp,
  onPlayQobuzTracks,
  selectedTrackKeys,
  selectionActive,
  onSelectionItemsChange,
  onToggleSelection,
  openPlaylistPickerForItems,
  remoteSurface = false,
  playbackStatus,
  customDisplayFont
}: {
  id?: string | number | null;
  playAlbum: (
    id: string | number,
    startIndex?: number,
    shuffle?: boolean,
    versionId?: number
  ) => Promise<void>;
  onOpenArtist: (name: string) => void;
  onOpenLocalAlbum?: (id: string | number) => void;
  onOpenQobuzAlbum?: (id: string | number, albumHint?: LibraryAlbum) => void;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  providedDetail?: JsonRecord | null;
  kind?: 'local' | 'qobuz';
  showQobuzStamp?: boolean;
  onPlayQobuzTracks?: (tracks: QobuzTrack[], startIndex?: number) => void;
  selectedTrackKeys: Set<string>;
  selectionActive: boolean;
  onSelectionItemsChange: (items: AlbumSelectionItem[]) => void;
  onToggleSelection: (key: string) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  remoteSurface?: boolean;
  playbackStatus: PlaybackStatus;
  customDisplayFont: CustomDisplayFontSettings | null;
}) {
  const [loadedDetail, setLoadedDetail] = useState<JsonRecord | null>(null);
  const [providedDetailOverride, setProvidedDetailOverride] = useState<
    JsonRecord | null | undefined
  >(undefined);
  const [activeTab, setActiveTab] = useState<'tracks' | 'credits' | 'versions'>('tracks');
  const [viewingVersionId, setViewingVersionId] = useState<string | number | null>(null);
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const [artworkOpen, setArtworkOpen] = useState(false);
  const [metadataEditorOpen, setMetadataEditorOpen] = useState(false);
  const [albumQueueMenu, setAlbumQueueMenu] = useState<{ x: number; y: number } | null>(null);
  const [trackMenu, setTrackMenu] = useState<{ index: number; x: number; y: number } | null>(null);
  const [favoriteKeys, setFavoriteKeys] = useState<Set<string>>(() => new Set());
  const [favoriteBusy, setFavoriteBusy] = useState(false);
  const qobuzEnhancementAttempts = useRef<Set<string>>(new Set());
  const creditsRefreshKeyRef = useRef('');
  useActionMenuScrollLock(Boolean(albumQueueMenu || trackMenu));
  useEffect(() => {
    if (providedDetail !== undefined) return;
    if (id === null || id === undefined) return;
    let cancelled = false;
    setLoadedDetail(null);
    loadAlbumDetailCached(id)
      .then((detail) => {
        if (!cancelled) setLoadedDetail(detail);
      })
      .catch(() => {
        if (!cancelled) setLoadedDetail(null);
      });
    return () => {
      cancelled = true;
    };
  }, [id, providedDetail]);
  useEffect(() => {
    setActiveTab('tracks');
    setViewingVersionId(null);
    setDescriptionOpen(false);
    setArtworkOpen(false);
    setMetadataEditorOpen(false);
    setProvidedDetailOverride(undefined);
  }, [id, providedDetail, kind]);
  useEffect(() => {
    if (!descriptionOpen) return undefined;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setDescriptionOpen(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [descriptionOpen]);
  useEffect(() => {
    const closeMenus = () => {
      setAlbumQueueMenu(null);
      setTrackMenu(null);
    };
    window.addEventListener('click', closeMenus);
    window.addEventListener('keydown', closeMenus);
    return () => {
      window.removeEventListener('click', closeMenus);
      window.removeEventListener('keydown', closeMenus);
    };
  }, []);
  const detail =
    providedDetailOverride !== undefined
      ? providedDetailOverride
      : providedDetail === undefined
        ? loadedDetail
        : providedDetail;
  const setCurrentDetail = (
    next: JsonRecord | null | ((current: JsonRecord | null) => JsonRecord | null)
  ) => {
    if (providedDetail === undefined) {
      setLoadedDetail((current) => {
        const resolved = typeof next === 'function' ? next(current) : next;
        if (kind !== 'qobuz') updateAlbumDetailCache(id, resolved);
        return resolved;
      });
      return;
    }
    setProvidedDetailOverride((current) => {
      const base = current !== undefined ? current : providedDetail;
      const resolved = typeof next === 'function' ? next(base) : next;
      if (kind !== 'qobuz') updateAlbumDetailCache(id, resolved);
      return resolved;
    });
  };
  const isQobuz = kind === 'qobuz';
  const album = (detail?.album || detail) as LibraryAlbum | null;
  const canonicalAlbum = (
    !isQobuz && detail?.canonical_album ? detail.canonical_album : null
  ) as LibraryAlbum | null;
  useEffect(() => {
    const localAlbumId = album?.id;
    if (
      activeTab !== 'credits' ||
      isQobuz ||
      localAlbumId === null ||
      localAlbumId === undefined ||
      album?.qobuz_match_status !== 'matched'
    ) {
      return;
    }
    const refreshKey = String(localAlbumId);
    if (creditsRefreshKeyRef.current === refreshKey) return;
    creditsRefreshKeyRef.current = refreshKey;
    let cancelled = false;
    endpoints
      .albumQobuzCreditsRefresh(localAlbumId)
      .then((nextDetail) => {
        if (!cancelled) setCurrentDetail(nextDetail);
      })
      .catch(() => {
        if (!cancelled) creditsRefreshKeyRef.current = '';
      });
    return () => {
      cancelled = true;
    };
  }, [activeTab, album?.id, album?.qobuz_match_status, isQobuz]);
  const linkedQobuzAlbumId = !isQobuz
    ? idValue(
        canonicalAlbum?.qobuz_album_id,
        canonicalAlbum?.qobuz_id,
        album?.qobuz_album_id,
        album?.qobuz_id
      )
    : '';
  const rawBaseTracks = useMemo(
    () => orderAlbumTracks(safeArray<LibraryTrack>(detail?.tracks || album?.tracks)),
    [detail, album]
  );
  const baseTracks = useMemo(
    () => (isQobuz ? rawBaseTracks : localTracksWithLinkedQobuzMetadata(detail, rawBaseTracks)),
    [detail, isQobuz, rawBaseTracks]
  );
  const versions = useMemo(() => safeArray<JsonRecord>(detail?.versions), [detail]);
  const viewingVersion = useMemo(
    () => resolveViewingVersion(album, versions, viewingVersionId),
    [album, versions, viewingVersionId]
  );
  const hasQobuzStamp = showQobuzStamp ?? (isQobuz || viewingVersion?.provider === 'qobuz');
  const tracks = useMemo(() => {
    if (isQobuz)
      return orderAlbumTracks(applyQobuzVersionToQobuzTracks(baseTracks, viewingVersion));
    if (viewingVersion?.provider === 'qobuz')
      return qobuzVersionTracksFromCanonical(detail, viewingVersion, baseTracks);
    return baseTracks;
  }, [baseTracks, detail, isQobuz, viewingVersion]);
  const art = albumArtworkForViewingVersion(album, viewingVersion);
  const albumViewArt = qobuzAlbumViewArt(art, remoteSurface);
  const albumId = isQobuz ? (id ?? album?.id ?? '') : (album?.id ?? id ?? '');
  const albumDate = formatAlbumDate(album);
  const artist = String(album?.album_artist || album?.artist || 'Unknown artist');
  const title = titleOf(album, 'Album');
  const descriptionSource = album?.description || canonicalAlbum?.description;
  const description = plainDescription(descriptionSource);
  const descriptionBlocks = descriptionParagraphs(descriptionSource);
  const titleClass =
    String(title).length > 58
      ? ' is-extra-long-title'
      : String(title).length > 38
        ? ' is-long-title'
        : '';
  const favoriteAlbum = useMemo(() => {
    if (!album) return null;
    if (isQobuz) return album;
    return canonicalAlbum?.qobuz_album_id || canonicalAlbum?.qobuz_id
      ? {
          ...album,
          qobuz_id: canonicalAlbum.qobuz_album_id || canonicalAlbum.qobuz_id,
          qobuz_album_id: canonicalAlbum.qobuz_album_id || canonicalAlbum.qobuz_id,
          image_url: canonicalAlbum.image_url || album.image_url,
          title: canonicalAlbum.title || album.title,
          album_artist: canonicalAlbum.album_artist || album.album_artist,
          artist: canonicalAlbum.album_artist || album.artist,
          year: canonicalAlbum.year ?? album.year
        }
      : album;
  }, [album, canonicalAlbum, isQobuz]);
  const currentFavoriteKey = favoriteAlbumKey(favoriteAlbum);
  const isFavorite = Boolean(currentFavoriteKey && favoriteKeys.has(currentFavoriteKey));
  const tracksByDisc = tracks.reduce<Record<string, LibraryTrack[]>>((groups, track) => {
    const disc = String(positiveNumber(track.disc_number) || 1);
    groups[disc] = groups[disc] || [];
    groups[disc].push(track);
    return groups;
  }, {});
  const discNumbers = Object.keys(tracksByDisc)
    .map(Number)
    .sort((a, b) => a - b);
  const hasMultipleDiscs = discNumbers.length > 1;
  const totalDuration =
    Number(album?.duration_secs) ||
    tracks.reduce((sum, track) => sum + (Number(track.duration_secs) || 0), 0);
  const genres =
    album?.genre ||
    Array.from(new Set(tracks.map((track) => track.genre).filter(Boolean))).join(', ');
  const creditsSummary = [
    totalDuration ? ['Length', formatLongDuration(totalDuration)] : null,
    tracks.length ? ['Tracks', `${tracks.length} ${tracks.length === 1 ? 'song' : 'songs'}`] : null,
    albumDate ? ['Release', albumDate] : null,
    ['Source', isQobuz ? 'Qobuz' : 'Local'],
    album?.label ? ['Label', String(album.label)] : null,
    genres ? ['Genre', String(genres)] : null
  ].filter(Boolean) as Array<[string, string]>;
  const playVisibleTracks = (startIndex = 0, shuffle = false) => {
    if (isQobuz) {
      const qobuzTracks = tracks.map(qobuzTrackFromAlbumTrack).filter(Boolean) as QobuzTrack[];
      if (!qobuzTracks.length) return;
      onPlayQobuzTracks?.(shuffle ? shuffled(qobuzTracks) : qobuzTracks, shuffle ? 0 : startIndex);
      return;
    }
    const versionId = positiveNumber(viewingVersion?.id) || undefined;
    if (albumId !== '') playAlbum(albumId, startIndex, shuffle, versionId);
  };
  const albumQueueItems = () => {
    if (isQobuz) {
      return tracks
        .map(qobuzTrackFromAlbumTrack)
        .filter(Boolean)
        .map((track) => qobuzTrackToQueueItem(track as QobuzTrack));
    }
    return tracks.map(
      (track) =>
        resolvedPlaySourceToQueueItem(track.play_source as ResolvedPlaySource) ||
        localTrackToQueueItem(track)
    );
  };
  const queueItemForAlbumTrack = (track: LibraryTrack) => {
    if (!isQobuz)
      return (
        resolvedPlaySourceToQueueItem(track.play_source as ResolvedPlaySource) ||
        localTrackToQueueItem(track)
      );
    const qobuzTrack = qobuzTrackFromAlbumTrack(track);
    return qobuzTrack ? qobuzTrackToQueueItem(qobuzTrack) : null;
  };
  const playbackFilenameForTrack = (track: LibraryTrack) => {
    if (isQobuz) return queueItemForAlbumTrack(track)?.filename || '';
    return String(track.file_name || track.name || queueItemForAlbumTrack(track)?.filename || '');
  };
  const selectionItems = useMemo(
    () =>
      tracks
        .map((track, index) => {
          const item = queueItemForAlbumTrack(track);
          const key = albumTrackSelectionKeyForQueueItem(item, index);
          return item && key ? { key, item } : null;
        })
        .filter(Boolean) as AlbumSelectionItem[],
    [isQobuz, tracks]
  );
  useEffect(() => {
    onSelectionItemsChange(selectionItems);
  }, [onSelectionItemsChange, selectionItems]);
  useEffect(() => {
    const hasCatalogVersions = versions.some((version) => idValue(version.open_album_id) !== '');
    if (isQobuz || linkedQobuzAlbumId === '' || (hasCatalogVersions && detailDescription(detail)))
      return undefined;
    const enhancementKey = String(linkedQobuzAlbumId);
    if (qobuzEnhancementAttempts.current.has(enhancementKey)) return undefined;
    qobuzEnhancementAttempts.current.add(enhancementKey);
    let cancelled = false;
    loadQobuzAlbumDetail(linkedQobuzAlbumId)
      .then((result) => {
        if (cancelled) return;
        const catalogVersions = safeArray<JsonRecord>(result.detail?.versions).map((version) => ({
          ...version,
          is_primary: false
        }));
        const remoteDescription = detailDescription(result.detail);
        if (!catalogVersions.length && !remoteDescription) return;
        setCurrentDetail((current) =>
          current
            ? {
                ...applyDetailDescription(current, remoteDescription),
                versions: mergeAlbumVersionRows(
                  safeArray<JsonRecord>(current.versions),
                  catalogVersions
                )
              }
            : current
        );
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [isQobuz, linkedQobuzAlbumId, versions]);
  useEffect(() => {
    let cancelled = false;
    loadFavoriteAlbumsCached()
      .then((favorites) => {
        if (!cancelled)
          setFavoriteKeys(
            new Set(safeArray<LibraryAlbum>(favorites).map(favoriteAlbumKey).filter(Boolean))
          );
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);
  const queueAlbum = (placement: 'next' | 'end') => {
    const items = albumQueueItems();
    if (items.length) addItemsToQueue(items, placement);
    setAlbumQueueMenu(null);
  };
  const setPrimaryVersion = async (versionId: string | number) => {
    const numericVersionId = Number(versionId);
    if (!isQobuz && album?.id !== undefined && Number.isFinite(numericVersionId)) {
      const nextDetail = await endpoints.albumVersionPrimary(album.id, numericVersionId);
      updateAlbumDetailCache(album.id, nextDetail);
      setCurrentDetail(nextDetail);
      return;
    }
    setViewingVersionId(versionId);
    setCurrentDetail((current) =>
      current
        ? {
            ...current,
            versions: safeArray<JsonRecord>(current.versions).map((version) => ({
              ...version,
              is_primary: String(version.id) === String(versionId)
            }))
          }
        : current
    );
  };
  const toggleFavorite = async () => {
    if (!favoriteAlbum || !currentFavoriteKey || favoriteBusy) return;
    const payload = favoriteAlbumPayload(favoriteAlbum);
    if (!payload) return;
    setFavoriteBusy(true);
    try {
      if (isFavorite) {
        await removeFavoriteAlbumCached(payload);
        setFavoriteKeys((current) => {
          const next = new Set(current);
          next.delete(currentFavoriteKey);
          return next;
        });
      } else {
        const saved = await addFavoriteAlbumCached(payload);
        const savedKey = favoriteAlbumKey(saved) || currentFavoriteKey;
        setFavoriteKeys((current) => new Set([...current, savedKey]));
      }
    } finally {
      setFavoriteBusy(false);
    }
  };

  if (!detail) {
    return <AlbumDetailSkeleton />;
  }

  return (
    <section className="view album-detail-view">
      <div className="album-detail">
        <AlbumDetailHeader
          albumDate={albumDate}
          art={albumViewArt}
          artist={artist}
          description={description}
          favoriteBusy={favoriteBusy || !currentFavoriteKey}
          isFavorite={isFavorite}
          showQobuzStamp={hasQobuzStamp}
          onOpenArtist={onOpenArtist}
          onOpenArtwork={() => {
            if (albumViewArt) setArtworkOpen(true);
          }}
          onOpenDescription={() => setDescriptionOpen(true)}
          onOpenQueueMenu={(rect) =>
            setAlbumQueueMenu(actionMenuPosition(rect, { menuHeight: 84 }))
          }
          onPlay={() => playVisibleTracks(0)}
          onShuffle={() => playVisibleTracks(0, true)}
          onToggleFavorite={() => {
            toggleFavorite().catch(() => undefined);
          }}
          title={title}
          titleClass={titleClass}
          tracks={tracks}
          customDisplayFont={customDisplayFont}
        />
        <div className="segmented" role="tablist">
          <button
            className={activeTab === 'tracks' ? 'on' : ''}
            type="button"
            role="tab"
            onClick={() => setActiveTab('tracks')}
          >
            Tracks
          </button>
          <button
            className={activeTab === 'credits' ? 'on' : ''}
            type="button"
            role="tab"
            onClick={() => setActiveTab('credits')}
          >
            Credits
          </button>
          <button
            className={activeTab === 'versions' ? 'on' : ''}
            type="button"
            role="tab"
            onClick={() => setActiveTab('versions')}
          >
            Versions
          </button>
        </div>
        <div
          className={`album-tab-panel${hasMultipleDiscs ? ' has-multiple-discs' : ''}`}
          data-album-tab-panel="tracks"
          hidden={activeTab !== 'tracks'}
        >
          <section className="playlist-panel album-track-panel">
            {hasMultipleDiscs ? (
              discNumbers.map((disc) => (
                <div className="react-album-disc-section" key={disc}>
                  <div className="disc-header">Disc {disc}</div>
                  <AlbumTrackList
                    tracks={tracksByDisc[String(disc)] || []}
                    allTracks={tracks}
                    isQobuz={isQobuz}
                    playbackStatus={playbackStatus}
                    onPlay={(index) => playVisibleTracks(index)}
                    onOpenMenu={(index, rect) =>
                      setTrackMenu({ index, ...actionMenuPosition(rect, { menuHeight: 156 }) })
                    }
                    selectedKeys={selectedTrackKeys}
                    selectionActive={selectionActive}
                    onToggleSelection={onToggleSelection}
                    getSelectionKey={(track, index) =>
                      albumTrackSelectionKeyForQueueItem(queueItemForAlbumTrack(track), index)
                    }
                    getPlaybackFilename={playbackFilenameForTrack}
                  />
                </div>
              ))
            ) : (
              <AlbumTrackList
                tracks={tracks}
                allTracks={tracks}
                isQobuz={isQobuz}
                playbackStatus={playbackStatus}
                onPlay={(index) => playVisibleTracks(index)}
                onOpenMenu={(index, rect) =>
                  setTrackMenu({ index, ...actionMenuPosition(rect, { menuHeight: 156 }) })
                }
                selectedKeys={selectedTrackKeys}
                selectionActive={selectionActive}
                onToggleSelection={onToggleSelection}
                getSelectionKey={(track, index) =>
                  albumTrackSelectionKeyForQueueItem(queueItemForAlbumTrack(track), index)
                }
                getPlaybackFilename={playbackFilenameForTrack}
              />
            )}
          </section>
        </div>
        <div
          className="album-tab-panel"
          data-album-tab-panel="credits"
          hidden={activeTab !== 'credits'}
        >
          <AlbumCreditsPanel
            title={title}
            artist={artist}
            tracks={tracks}
            infoItems={creditsSummary}
          />
        </div>
        <div
          className="album-tab-panel"
          data-album-tab-panel="versions"
          hidden={activeTab !== 'versions'}
        >
          <AlbumVersionsPanel
            versions={versions}
            fallbackAlbum={album}
            fallbackTracks={tracks}
            viewingVersionId={viewingVersion?.id as string | number | null | undefined}
            onViewVersion={(versionId) => {
              setViewingVersionId(versionId);
              setActiveTab('tracks');
            }}
            onOpenLocalAlbum={onOpenLocalAlbum}
            onOpenQobuzAlbum={onOpenQobuzAlbum}
            onSetPrimary={setPrimaryVersion}
            onEditLocalAlbum={() => setMetadataEditorOpen(true)}
          />
        </div>
      </div>
      {albumQueueMenu ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide is-open"
          ariaLabel="Album queue options"
          style={{ left: Math.max(12, albumQueueMenu.x), top: albumQueueMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => queueAlbum('next')}
          >
            <PlayNextIcon />
            <span>Add album next</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => queueAlbum('end')}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add album to queue</span>
          </button>
        </Menu>
      ) : null}
      {trackMenu && tracks[trackMenu.index] ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide is-open"
          ariaLabel="Track options"
          style={{ left: Math.max(12, trackMenu.x), top: trackMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item has-filled-icon"
            type="button"
            role="menuitem"
            onClick={() => {
              playVisibleTracks(trackMenu.index);
              setTrackMenu(null);
            }}
          >
            <PlaybarPlayIcon className="track-action-play-icon" />
            <span>Play from here</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              const track = tracks[trackMenu.index];
              const item = queueItemForAlbumTrack(track);
              if (item) addItemsToQueue([item], 'next');
              setTrackMenu(null);
            }}
          >
            <PlayNextIcon />
            <span>Add next</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              const track = tracks[trackMenu.index];
              const item = queueItemForAlbumTrack(track);
              if (item) openPlaylistPickerForItems([item], item.title || 'Track');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h12M4 12h9M4 17h7M18 15v6M15 18h6" />
            <span>Add to playlist</span>
          </button>
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              const track = tracks[trackMenu.index];
              const item = queueItemForAlbumTrack(track);
              if (item) addItemsToQueue([item], 'end');
              setTrackMenu(null);
            }}
          >
            <Icon path="M4 7h10M4 12h10M4 17h7M18 10v8M14 14h8" />
            <span>Add to queue</span>
          </button>
        </Menu>
      ) : null}
      {descriptionOpen ? (
        <AlbumDescriptionModal
          title={title}
          artist={artist}
          year={album?.year}
          paragraphs={descriptionBlocks.length ? descriptionBlocks : [description]}
          onClose={() => setDescriptionOpen(false)}
        />
      ) : null}
      {artworkOpen && albumViewArt ? (
        <AlbumArtLightbox art={albumViewArt} title={title} onClose={() => setArtworkOpen(false)} />
      ) : null}
      {metadataEditorOpen && !isQobuz && album ? (
        <AlbumMetadataEditorModal
          album={album}
          tracks={baseTracks}
          onClose={() => setMetadataEditorOpen(false)}
          onSaved={(nextDetail) => setCurrentDetail(nextDetail)}
        />
      ) : null}
    </section>
  );
}
