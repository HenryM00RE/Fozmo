import type { LibraryTrack, QobuzTrack, QueueItem } from '../../../shared/types';

export type QueuePlacement = 'next' | 'end';

export type PlaybackRouteActions = {
  addItemsToQueue: (items: QueueItem[], placement: QueuePlacement) => void;
  playAlbum: (
    albumId: string | number,
    startIndex?: number,
    shuffle?: boolean,
    versionId?: number
  ) => Promise<void>;
  playArtistRadio: (artistName: string) => Promise<void>;
  playItems: (items: QueueItem[], startIndex?: number) => void;
  playQobuzAlbum: (albumId: string | number) => Promise<void>;
  playQobuzPlaylist: (playlistId: string | number) => Promise<void>;
  playQobuzTrack: (track: QobuzTrack, related?: QobuzTrack[]) => void;
  playSingleTrack: (track: LibraryTrack) => void;
  playTrack: (track: LibraryTrack) => void;
};
