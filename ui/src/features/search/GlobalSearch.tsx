import { type KeyboardEvent, useCallback, useEffect, useMemo } from 'react';
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
    onClose: closeSearch,
    onOpenAlbum,
    onOpenArtist,
    onOpenQobuzAlbum,
    onPlayQobuz,
    onPlayTrack,
    onQueueAlbum,
    onQueueTrack,
    query,
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
          <div className="global-search-results" onScroll={closeMenu}>
            {!searchView.hasQuery ? (
              <section
                className="global-search-section global-search-recent-section"
                aria-label="Recently searched"
              >
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
                        <Icon path="M10.5 17a6.5 6.5 0 1 1 0-13 6.5 6.5 0 0 1 0 13Z M16 16l4 4" />
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
                    <div className="global-search-pending">
                      {searchView.isPartial ? 'Still checking Qobuz.' : 'Reading index.'}
                    </div>
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
