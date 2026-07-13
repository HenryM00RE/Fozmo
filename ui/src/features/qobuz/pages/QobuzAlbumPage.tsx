import { useEffect, useState } from 'react';
import { qobuzTrackToQueueItem } from '../../../shared/lib/queue';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { JsonRecord, LibraryAlbum, QueueItem } from '../../../shared/types';
import type { AlbumSelectionItem } from '../../albums/model/albumModel';
import { AlbumDetailPage } from '../../albums/pages/AlbumDetailPage';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import { loadQobuzAlbumDetail } from '../model/qobuzData';

export function QobuzAlbumPage({
  id,
  albumHint,
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
  albumHint?: LibraryAlbum | null;
  onOpenArtist: (name: string) => void;
  onOpenLocalAlbum?: (id: string | number) => void;
  onOpenQobuzAlbum?: (id: string | number, albumHint?: LibraryAlbum) => void;
  playAlbum: (
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
  const [detail, setDetail] = useState<JsonRecord | null>(null);
  const [kind, setKind] = useState<'local' | 'qobuz'>('qobuz');
  useEffect(() => {
    if (id === null || id === undefined) return;
    let cancelled = false;
    setDetail(null);
    setKind('qobuz');
    const load = async () => {
      const result = await loadQobuzAlbumDetail(id, albumHint);
      if (cancelled) return;
      setDetail(result.detail);
      setKind(result.kind);
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [id, albumHint]);
  return (
    <AlbumDetailPage
      id={id}
      providedDetail={detail}
      kind={kind}
      showQobuzStamp
      onOpenArtist={onOpenArtist}
      onOpenLocalAlbum={onOpenLocalAlbum}
      onOpenQobuzAlbum={onOpenQobuzAlbum}
      playAlbum={playAlbum}
      addItemsToQueue={addItemsToQueue}
      playbackStatus={playbackStatus}
      onPlayQobuzTracks={(tracks, startIndex = 0) =>
        playItems(tracks.map(qobuzTrackToQueueItem), startIndex)
      }
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
