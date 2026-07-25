import { useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { sourceRefToQueueItem } from '../../../shared/lib/queue';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { JsonRecord, QueueItem, SourceRef } from '../../../shared/types';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import type { AlbumSelectionItem } from '../model/albumModel';
import {
  appleMusicAlbumToLibraryDetail,
  appleMusicSourceFromAlbumTrack
} from '../model/appleMusicAlbum';
import { AlbumDetailPage } from './AlbumDetailPage';

const ignoreLocalAlbumPlay = async () => undefined;

export function AppleMusicAlbumPage({
  id,
  storefront,
  onOpenArtist,
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
  const [loadError, setLoadError] = useState('');

  useEffect(() => {
    if (id === null || id === undefined || String(id).trim() === '') {
      setCatalogAlbum(null);
      setLoadError('This Apple Music album URL does not contain a catalog ID.');
      return;
    }
    let cancelled = false;
    setCatalogAlbum(null);
    setLoadError('');
    endpoints
      .appleMusicCatalogAlbum(String(id), storefront || undefined)
      .then((album) => {
        if (!cancelled) setCatalogAlbum(album);
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

  const detail = useMemo(
    () => (catalogAlbum ? appleMusicAlbumToLibraryDetail(catalogAlbum) : null),
    [catalogAlbum]
  );

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
      playAlbum={ignoreLocalAlbumPlay}
      onPlayAppleMusicTracks={(tracks, startIndex = 0) => {
        const items = tracks
          .map(appleMusicSourceFromAlbumTrack)
          .filter((source): source is SourceRef => source !== null)
          .map(sourceRefToQueueItem)
          .filter((item): item is QueueItem => item !== null);
        if (items.length) playItems(items, startIndex);
      }}
      onOpenArtist={onOpenArtist}
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
