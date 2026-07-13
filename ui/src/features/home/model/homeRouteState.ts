import type { JsonRecord } from '../../../shared/types';

export type HomeRouteState = {
  openRecentItem: (item: JsonRecord) => Promise<void>;
  playRecentItem: (item: JsonRecord) => Promise<void>;
  recentlyPlayedLoading: boolean;
  recentlyPlayedItems: JsonRecord[];
  recentSelectionActive: boolean;
  recentSelectionKeys: Set<string>;
  toggleAlbumSelection: (album: JsonRecord) => void;
  toggleRecentSelection: (item: JsonRecord) => void;
};
