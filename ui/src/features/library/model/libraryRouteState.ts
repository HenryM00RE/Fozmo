import type { JsonRecord, LibraryAlbum, LibraryTrack } from '../../../shared/types';

export type LibraryRouteState = {
  albums: LibraryAlbum[];
  artists: JsonRecord[];
  historyStats: JsonRecord | null;
  historyStatsLoading: boolean;
  recentHistory: JsonRecord[];
  recentHistoryLoading: boolean;
  tracks: LibraryTrack[];
};
