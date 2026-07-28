import {
  type KeyboardEvent,
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef
} from 'react';
import {
  type GlobalSearchPlacement,
  type GlobalSearchSource,
  type GlobalSearchState
} from '../../shared/lib/appSupport';
import type { LibraryAlbum, LibraryTrack, QobuzTrack } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { Modal } from '../../shared/ui/Modal';
import { actionMenuPosition } from '../../shared/ui/menuPosition';
import { GlobalSearchActionsMenu, GlobalSearchRow } from './components/GlobalSearchRow';
import { buildGlobalSearchView } from './globalSearchModel';
import { useGlobalSearchDialogState } from './hooks/useGlobalSearchDialogState';

/* Matches T3 Code's --fade-size of 1.5rem. */
const RESULTS_FADE_SIZE = 24;

function GlobalSearchSkeletonRows({ count }: { count: number }) {
  return (
    <div className="global-search-skeleton" role="status" aria-label="Loading search results">
      {Array.from({ length: count }, (_, index) => (
        <div
          className="global-search-row global-search-skeleton-row"
          aria-hidden="true"
          key={index}
        >
          <span className="global-search-skeleton-art skeleton-shimmer" />
          <span className="global-search-copy">
            <span className="global-search-skeleton-title skeleton-shimmer" />
            <span className="global-search-skeleton-meta skeleton-shimmer" />
          </span>
          <span className="global-search-skeleton-kind skeleton-shimmer" />
          <span className="global-search-skeleton-menu skeleton-shimmer" />
        </div>
      ))}
    </div>
  );
}

export function GlobalSearch({
  query,
  recentSearches,
  results,
  onQuery,
  onClose,
  onOpenAlbum,
  onOpenQobuzAlbum,
  onPlayTrack,
  onPlayQobuz,
  onOpenArtist,
  onAddTrackToPlaylist,
  onQueueTrack,
  onQueueAlbum,
  onRememberSearch,
  onRemoveRecentSearch,
  albums
}: {
  query: string;
  recentSearches: string[];
  results: GlobalSearchState;
  onQuery: (query: string) => void;
  onClose: () => void;
  onOpenAlbum: (id: string | number) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayTrack: (track: LibraryTrack) => void;
  onPlayQobuz: (track: QobuzTrack) => void;
  onOpenArtist: (name: string) => void;
  onAddTrackToPlaylist: (track: LibraryTrack | QobuzTrack, source: GlobalSearchSource) => void;
  onQueueTrack: (
    track: LibraryTrack | QobuzTrack,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => void;
  onQueueAlbum: (
    album: LibraryAlbum,
    source: GlobalSearchSource,
    placement: GlobalSearchPlacement
  ) => void | Promise<void>;
  onRememberSearch: (query: string) => void;
  onRemoveRecentSearch: (query: string) => void;
  albums: LibraryAlbum[];
}) {
  const {
    activeIndex,
    closeMenu,
    inputRef,
    openMenu,
    setActiveIndex,
    showAll,
    toggleShowAll,
    toggleMenu
  } = useGlobalSearchDialogState(query);
  const resultsRef = useRef<HTMLDivElement | null>(null);
  // Scoring runs over the whole album list on every keystroke. Deferring the
  // query keeps the field itself responsive while the list re-renders at lower
  // priority. This is not the fetch debounce in useGlobalSearch — that delays
  // the request; this delays the render. Only the view build reads the
  // deferred value: the input and the recent-search commit stay live.
  const deferredQuery = useDeferredValue(query);
  const commitSearch = useCallback(() => {
    onRememberSearch(query);
  }, [onRememberSearch, query]);
  const closeSearch = useCallback(() => {
    commitSearch();
    onQuery('');
    onClose();
  }, [commitSearch, onClose, onQuery]);
  const searchView = buildGlobalSearchView({
    albums,
    onAddTrackToPlaylist,
    onClose: closeSearch,
    onOpenAlbum,
    onOpenArtist,
    onOpenQobuzAlbum,
    onPlayQobuz,
    onPlayTrack,
    onQueueAlbum,
    onQueueTrack,
    query: deferredQuery,
    results,
    showAll
  });
  const visibleRows = useMemo(
    () => (searchView.topResult ? [searchView.topResult, ...searchView.rows] : searchView.rows),
    [searchView.rows, searchView.topResult]
  );
  const openMenuRow = openMenu
    ? visibleRows.find((row) => row.id === openMenu.rowId) || null
    : null;

  const toggleRowMenu = useCallback(
    (row: (typeof visibleRows)[number], buttonRect: DOMRect) => {
      const actionCount = row.actions?.length || 0;
      const menuHeight = 12 + actionCount * 34 + Math.max(0, actionCount - 1) * 3;
      toggleMenu({
        rowId: row.id,
        ...actionMenuPosition(buttonRect, { menuHeight })
      });
    },
    [toggleMenu]
  );

  useEffect(() => {
    setActiveIndex((current) => {
      if (!visibleRows.length) return -1;
      if (current < 0) return 0;
      return Math.min(current, visibleRows.length - 1);
    });
  }, [setActiveIndex, visibleRows.length]);

  // Each edge fades only as far as there is actually overflow past it, so the
  // list is unmasked at rest and the fade grows in as you scroll. Mirrors T3
  // Code's scroll-area (min(--fade-size, --scroll-area-overflow-y-start)),
  // which gets these figures from its scroll primitive; we measure them here.
  const syncResultsFade = useCallback(() => {
    const results = resultsRef.current;
    if (!results) return;
    const scrollable = results.scrollHeight - results.clientHeight;
    const top = Math.max(0, Math.min(RESULTS_FADE_SIZE, results.scrollTop));
    const bottom = Math.max(0, Math.min(RESULTS_FADE_SIZE, scrollable - results.scrollTop));
    results.style.setProperty('--search-fade-top', `${top}px`);
    results.style.setProperty('--search-fade-bottom', `${bottom}px`);
  }, []);

  const handleResultsScroll = useCallback(() => {
    closeMenu();
    syncResultsFade();
  }, [closeMenu, syncResultsFade]);

  // Scrolling isn't the only thing that changes the overflow: so does the
  // result set and the show-all toggle.
  useEffect(() => {
    syncResultsFade();
  }, [syncResultsFade, visibleRows.length, showAll, searchView.isLoading]);

  // Arrow keys only move the index, so past the fold the highlight walks off
  // the panel and leaves the user navigating blind. 'nearest' keeps this a
  // no-op when the row is already visible, so selecting by mouse doesn't jump.
  useEffect(() => {
    if (activeIndex < 0) return;
    const activeRow = resultsRef.current?.querySelector('.global-search-row.is-active');
    activeRow?.scrollIntoView?.({ block: 'nearest' });
  }, [activeIndex]);

  const runRowAt = useCallback(
    (index: number) => {
      const row = visibleRows[index] || visibleRows[0];
      if (!row) return;
      closeMenu();
      Promise.resolve(row.run()).catch(() => undefined);
    },
    [closeMenu, visibleRows]
  );

  const moveActiveRow = useCallback(
    (delta: number) => {
      if (!visibleRows.length) return;
      closeMenu();
      setActiveIndex((current) => {
        const start = current < 0 ? 0 : current;
        return (start + delta + visibleRows.length) % visibleRows.length;
      });
    },
    [closeMenu, setActiveIndex, visibleRows.length]
  );

  const handleSearchKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        runRowAt(activeIndex);
        return;
      }
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        moveActiveRow(1);
        return;
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault();
        moveActiveRow(-1);
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        closeSearch();
      }
    },
    [activeIndex, closeSearch, moveActiveRow, runRowAt]
  );

  return (
    <Modal
      open
      className="global-search-backdrop"
      ariaLabel="Search library and Qobuz"
      onClose={closeSearch}
    >
      <div className="global-search-panel app-modal-surface">
        <header className="global-search-head">
          <label className="global-search-field">
            <span className="sr-only">Search library and Qobuz</span>
            <input
              ref={inputRef}
              type="search"
              value={query}
              autoComplete="off"
              onKeyDown={handleSearchKeyDown}
              onChange={(event) => onQuery(event.target.value)}
              placeholder="Search songs, albums, or artists"
            />
          </label>
          <button
            className="global-search-close"
            type="button"
            aria-label="Close search"
            onClick={closeSearch}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>
        <div className="global-search-body">
          <div className="global-search-status">{searchView.status}</div>
          <div className="global-search-results" ref={resultsRef} onScroll={handleResultsScroll}>
            {!searchView.hasQuery ? (
              <section
                className="global-search-section global-search-recent-section"
                aria-label="Recently searched"
              >
                <div className="global-search-section-head global-search-recent-head">
                  <span className="section-label">Recently searched</span>
                </div>
                {recentSearches.length ? (
                  recentSearches.map((recentQuery) => (
                    <div className="global-search-recent-row" key={recentQuery}>
                      <button
                        className="global-search-recent-query"
                        type="button"
                        onClick={() => {
                          onQuery(recentQuery);
                          inputRef.current?.focus();
                        }}
                      >
                        <span>{recentQuery}</span>
                      </button>
                      <button
                        className="global-search-recent-remove"
                        type="button"
                        aria-label={`Remove ${recentQuery} from recent searches`}
                        title="Remove"
                        onClick={() => onRemoveRecentSearch(recentQuery)}
                      >
                        <Icon path="M18 6 6 18M6 6l12 12" />
                      </button>
                    </div>
                  ))
                ) : (
                  <div className="global-search-recent-empty">No recent searches yet.</div>
                )}
              </section>
            ) : (
              <>
                {searchView.topResult ? (
                  <section className="global-search-section global-search-top-section">
                    <div className="global-search-section-head">
                      <span className="section-label">Top result</span>
                    </div>
                    <GlobalSearchRow
                      row={searchView.topResult}
                      active={activeIndex === 0}
                      featured
                      menuOpen={openMenu?.rowId === searchView.topResult.id}
                      onToggleMenu={(buttonRect) =>
                        searchView.topResult && toggleRowMenu(searchView.topResult, buttonRect)
                      }
                      onMoveActive={moveActiveRow}
                      onRun={commitSearch}
                      onRequestClose={closeSearch}
                      onSelect={() => setActiveIndex(0)}
                    />
                  </section>
                ) : null}
                <section className="global-search-section global-search-mixed-section">
                  <div className="global-search-section-head">
                    <span className="section-label">
                      {searchView.topResult ? 'All results' : 'Results'}
                    </span>
                    {searchView.total ? (
                      <span className="global-search-count">{searchView.total} matches</span>
                    ) : null}
                  </div>
                  {searchView.rows.map((row, rowIndex) => {
                    const index = searchView.topResult ? rowIndex + 1 : rowIndex;
                    return (
                      <GlobalSearchRow
                        key={row.id}
                        row={row}
                        active={activeIndex === index}
                        menuOpen={openMenu?.rowId === row.id}
                        onToggleMenu={(buttonRect) => toggleRowMenu(row, buttonRect)}
                        onMoveActive={moveActiveRow}
                        onRun={commitSearch}
                        onRequestClose={closeSearch}
                        onSelect={() => setActiveIndex(index)}
                      />
                    );
                  })}
                  {searchView.isLoading ? (
                    <GlobalSearchSkeletonRows count={searchView.total ? 2 : 7} />
                  ) : null}
                  {searchView.hasMore || showAll ? (
                    <div className="global-search-more-row">
                      <button
                        className="global-search-more"
                        type="button"
                        aria-expanded={showAll}
                        onClick={toggleShowAll}
                      >
                        {showAll ? 'Show fewer results' : `Show more results`}
                      </button>
                    </div>
                  ) : null}
                </section>
              </>
            )}
          </div>
        </div>
      </div>
      {openMenu && openMenuRow ? (
        <GlobalSearchActionsMenu
          row={openMenuRow}
          x={openMenu.x}
          y={openMenu.y}
          onCloseMenu={closeMenu}
          onRun={commitSearch}
        />
      ) : null}
    </Modal>
  );
}
