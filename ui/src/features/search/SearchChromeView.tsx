import { GlobalSearch } from './GlobalSearch';
import type { SearchChromeState } from './model/searchChromeState';

type SearchChromeViewProps = {
  searchChrome: SearchChromeState;
};

export function SearchChromeView({ searchChrome }: SearchChromeViewProps) {
  const {
    albums,
    globalSearch,
    onOpenAlbum,
    onOpenArtist,
    onOpenQobuzAlbum,
    onPlayQobuzTrack,
    onPlayTrack,
    onQueueAlbum,
    onQueueTrack
  } = searchChrome;

  if (!globalSearch.open) return null;

  return (
    <GlobalSearch
      query={globalSearch.query}
      recentSearches={globalSearch.recentSearches}
      results={globalSearch.results}
      onQuery={globalSearch.setQuery}
      onClose={() => globalSearch.setOpen(false)}
      onRememberSearch={globalSearch.rememberSearch}
      onRemoveRecentSearch={globalSearch.removeRecentSearch}
      onOpenAlbum={(id) => {
        globalSearch.setOpen(false);
        onOpenAlbum(id);
      }}
      onOpenQobuzAlbum={(id) => {
        globalSearch.setOpen(false);
        onOpenQobuzAlbum(id);
      }}
      onPlayTrack={(track) => {
        globalSearch.setOpen(false);
        onPlayTrack(track);
      }}
      onPlayQobuz={(track) => {
        globalSearch.setOpen(false);
        onPlayQobuzTrack(track);
      }}
      onOpenArtist={(name) => {
        globalSearch.setOpen(false);
        onOpenArtist(name);
      }}
      onQueueTrack={onQueueTrack}
      onQueueAlbum={onQueueAlbum}
      albums={albums}
    />
  );
}
