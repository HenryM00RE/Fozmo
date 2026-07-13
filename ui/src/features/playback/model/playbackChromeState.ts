import type { Dispatch, SetStateAction } from 'react';
import type { JsonRecord, LibraryAlbum, QueueState, ZoneProfile } from '../../../shared/types';

export type PlaybackAlbumTarget = {
  source: 'local' | 'qobuz';
  id: string | number;
};

export type PlaybackChromeState = {
  activeZoneId: string;
  albums: LibraryAlbum[];
  nowPlayingOpen: boolean;
  onClearQueue: () => void;
  onOpenAlbum: (target: PlaybackAlbumTarget) => void;
  onSelectZone: (zoneId: string) => Promise<void>;
  onShuffleQueue: () => void;
  onToggleLoop: () => void;
  queue: QueueState;
  setNowPlayingOpen: Dispatch<SetStateAction<boolean>>;
  setSignalOpen: Dispatch<SetStateAction<boolean>>;
  signalOpen: boolean;
  status: JsonRecord;
  zones: ZoneProfile[];
};
