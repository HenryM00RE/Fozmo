import { useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { sourceRefToQueueItem } from '../../../shared/lib/queue';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { JsonRecord, LibraryAlbum, QueueItem, SourceRef } from '../../../shared/types';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import { loadAppleMusicCatalogAlbumCached } from '../model/albumData';
import type { AlbumSelectionItem } from '../model/albumModel';
import {
  appleMusicAlbumToLibraryDetail,
  appleMusicSourceFromAlbumTrack
} from '../model/appleMusicAlbum';
import { AlbumDetailPage } from './AlbumDetailPage';

export function AppleMusicAlbumPage({
  id,
  storefront,
  onOpenArtist,
  onOpenLocalAlbum,
  onOpenQobuzAlbum,
  playAlbum,
  playItems,
  addItemsToQueue,
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
  storefront?: string | null;
  onOpenArtist: (artist: string) => void;
  onOpenLocalAlbum?: (id: string | number) => void;
  onOpenQobuzAlbum?: (id: string | number, albumHint?: LibraryAlbum) => void;
  playAlbum?: (
    id: string | number,
    startIndex?: number,
    shuffle?: boolean,
    versionId?: number
  ) => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => void;
  selectedTrackKeys: Set<string>;
  selectionActive: boolean;
  onSelectionItemsChange: (items: AlbumSelectionItem[]) => void;
  onToggleSelection: (key: string) => void;
  openPlaylistPickerForItems: (items: QueueItem[], title?: string, onAdded?: () => void) => void;
  remoteSurface?: boolean;
  playbackStatus: PlaybackStatus;
  customDisplayFont: CustomDisplayFontSettings | null;
}) {
  const [catalogAlbum, setCatalogAlbum] = useState<JsonRecord | null>(null);
  const [linkedDetail, setLinkedDetail] = useState<JsonRecord | null>(null);
  const [loadError, setLoadError] = useState('');

  useEffect(() => {
    if (id === null || id === undefined || String(id).trim() === '') {
      setCatalogAlbum(null);
      setLoadError('This Apple Music album URL does not contain a catalog ID.');
      return;
    }
    let cancelled = false;
    setCatalogAlbum(null);
    setLinkedDetail(null);
    setLoadError('');
    Promise.all([
      loadAppleMusicCatalogAlbumCached(String(id), storefront || undefined),
      endpoints.albumByAppleMusicId(String(id)).catch(() => null)
    ])
      .then(([album, linked]) => {
        if (cancelled) return;
        setCatalogAlbum(album);
        setLinkedDetail(linked);
      })
      .catch((error) => {
        if (cancelled) return;
        setLoadError(
          error instanceof Error ? error.message : 'Apple Music could not load this album.'
        );
      });
    return () => {
      cancelled = true;
    };
  }, [id, storefront]);

  const detail = useMemo(() => {
    if (!catalogAlbum) return null;
    const appleDetail = appleMusicAlbumToLibraryDetail(catalogAlbum);
    if (!linkedDetail?.album) return appleDetail;
    const linkedAlbum = linkedDetail.album as JsonRecord;
    const linkedAlbumId = linkedAlbum.id;
    const linkedVersions = Array.isArray(linkedDetail.versions)
      ? (linkedDetail.versions as JsonRecord[])
      : [];
    const groupedVersions = linkedVersions.map((version) => ({
      ...version,
      ...(version.provider === 'local' ? { open_local_album_id: linkedAlbumId } : {}),
      ...(version.provider === 'qobuz' ? { open_album_id: version.provider_id } : {})
    }));
    return {
      ...appleDetail,
      linked_album: linkedAlbum,
      linked_album_id: linkedAlbumId,
      linked_tracks: linkedDetail.tracks,
      canonical_album: linkedDetail.canonical_album,
      canonical_tracks: linkedDetail.canonical_tracks,
      qobuz_track_links: linkedDetail.qobuz_track_links,
      versions: groupedVersions.length ? groupedVersions : appleDetail.versions
    };
  }, [catalogAlbum, linkedDetail]);

  if (loadError) {
    return (
      <section className="view album-detail-view">
        <div className="panel raised">
          <span className="section-label">Apple Music</span>
          <h1>Could not load album</h1>
          <p>{loadError}</p>
        </div>
      </section>
    );
  }

  return (
    <AlbumDetailPage
      id={id}
      providedDetail={detail}
      kind="apple_music"
      playAlbum={playAlbum || (async () => undefined)}
      onPlayAppleMusicTracks={(tracks, startIndex = 0) => {
        const items = tracks
          .map(appleMusicSourceFromAlbumTrack)
          .filter((source): source is SourceRef => source !== null)
          .map(sourceRefToQueueItem)
          .filter((item): item is QueueItem => item !== null);
        if (items.length) playItems(items, startIndex);
      }}
      onOpenArtist={onOpenArtist}
      onOpenLocalAlbum={onOpenLocalAlbum}
      onOpenQobuzAlbum={onOpenQobuzAlbum}
      addItemsToQueue={addItemsToQueue}
      playbackStatus={playbackStatus}
      selectedTrackKeys={selectedTrackKeys}
      selectionActive={selectionActive}
      onSelectionItemsChange={onSelectionItemsChange}
      onToggleSelection={onToggleSelection}
      openPlaylistPickerForItems={openPlaylistPickerForItems}
      remoteSurface={remoteSurface}
      customDisplayFont={customDisplayFont}
    />
  );
}
