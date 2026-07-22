// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SelectionActionsToolbar } from './SelectionActionsToolbar';
import type { SelectionToolbarState } from './selectionToolbar';

function playlistSelectionToolbar(
  overrides: Partial<SelectionToolbarState> = {}
): SelectionToolbarState {
  return {
    activeSelectionBusy: false,
    activeSelectionCount: 2,
    activeSelectionType: 'playlists',
    addSelectedAlbumTracksToPlaylist: vi.fn(),
    addSelectedRecentlyPlayedToPlaylist: vi.fn(),
    albumSelectionMenuOpen: false,
    clearAlbumTrackSelection: vi.fn(),
    clearPlaylistSelection: vi.fn(),
    clearRecentSelection: vi.fn(),
    playSelectedAlbumTracks: vi.fn(),
    playSelectedPlaylists: vi.fn(),
    playSelectedRecentlyPlayed: vi.fn(),
    playlistSelectionMenuOpen: true,
    queueSelectedAlbumTracks: vi.fn(),
    queueSelectedPlaylists: vi.fn(),
    queueSelectedRecentlyPlayed: vi.fn(),
    recentSelectionMenuOpen: false,
    setAlbumSelectionMenuOpen: vi.fn(),
    setPlaylistSelectionMenuOpen: vi.fn(),
    setRecentSelectionMenuOpen: vi.fn(),
    ...overrides
  };
}

describe('SelectionActionsToolbar', () => {
  afterEach(cleanup);

  it('uses the shared split-button menu for selected playlists', () => {
    const playSelectedPlaylists = vi.fn();
    const queueSelectedPlaylists = vi.fn();
    render(
      <SelectionActionsToolbar
        selectionToolbar={playlistSelectionToolbar({
          playSelectedPlaylists,
          queueSelectedPlaylists
        })}
      />
    );

    expect(screen.getByText('2 selected')).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Add selected to playlist' })).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Play now' }));
    expect(playSelectedPlaylists).toHaveBeenCalledOnce();

    fireEvent.click(screen.getByRole('menuitem', { name: 'Queue next' }));
    expect(queueSelectedPlaylists).toHaveBeenCalledWith('next');

    fireEvent.click(screen.getByRole('menuitem', { name: 'Add to queue' }));
    expect(queueSelectedPlaylists).toHaveBeenCalledWith('end');
  });
});
