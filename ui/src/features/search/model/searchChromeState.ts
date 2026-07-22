import type { Dispatch, SetStateAction } from 'react';
import type {
  GlobalSearchPlacement,
  GlobalSearchSource,
  GlobalSearchState
} from '../../../shared/lib/appSupport';
import type { LibraryAlbum, LibraryTrack, QobuzTrack } from '../../../shared/types';

export type GlobalSearchController = {
  open: boolean;
  query: string;
  recentSearches: string[];
  rememberSearch: (query: string) => void;
  removeRecentSearch: (query: string) => void;
  results: GlobalSearchState;
  setOpen: Dispatch<SetStateAction<boolean>>;
  setQuery: Dispatch<SetStateAction<string>>;
};

export type SearchChromeState = {
  albums: LibraryAlbum[];
  globalSearch: GlobalSearchController;
  onAddTrackToPlaylist: (track: LibraryTrack | QobuzTrack, source: GlobalSearchSource) => void;
  onOpenAlbum: (id: string | number) => void;
  onOpenArtist: (rawName: unknown) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayQobuzTrack: (track: QobuzTrack) => void;
  onPlayTrack: (track: LibraryTrack) => void;
  onQueueAlbum: (
    album: LibraryAlbum,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => Promise<void>;
  onQueueTrack: (
    track: LibraryTrack | QobuzTrack,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => void;
};
