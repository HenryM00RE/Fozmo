import type { RouteState, ViewId } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { PlaylistCover } from './components/PlaylistCover';
import { mostRecentPlaylists } from './model/playlistModel';
import type { PlaylistShellState } from './model/playlistShellState';

type PlaylistSidebarSectionProps = {
  onNavigate: (next: RouteState) => void;
  playlistShell: PlaylistShellState;
  route: RouteState;
};

function playlistRouteActive(view: ViewId) {
  return view === 'playlists' || view === 'playlist';
}

export function PlaylistSidebarSection({
  onNavigate,
  playlistShell,
  route
}: PlaylistSidebarSectionProps) {
  const { playlists, sidebarPlaylistsOpen, toggleSidebarPlaylists } = playlistShell;
  const visiblePlaylists = mostRecentPlaylists(playlists);

  return (
    <section
      className={`sidebar-playlist-section${sidebarPlaylistsOpen ? ' is-open' : ''}${playlistRouteActive(route.view) ? ' is-active' : ''}`}
      aria-label="Playlists"
    >
      <div className="sidebar-playlist-head">
        <button
          className="sidebar-playlist-toggle"
          type="button"
          aria-expanded={sidebarPlaylistsOpen}
          aria-controls="sidebar-playlist-list"
          onClick={toggleSidebarPlaylists}
        >
          <span>Playlists</span>
          <Icon path="m9 18 6-6-6-6" />
        </button>
        <div className="sidebar-playlist-actions">
          <button
            className="sidebar-playlist-action"
            type="button"
            aria-label="View playlists"
            title="View playlists"
            onClick={() => onNavigate({ view: 'playlists' })}
          >
            <svg className="sidebar-playlist-more-icon" viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="5" cy="12" r="1.5" />
              <circle cx="12" cy="12" r="1.5" />
              <circle cx="19" cy="12" r="1.5" />
            </svg>
          </button>
        </div>
      </div>
      <div
        className="sidebar-playlist-list"
        id="sidebar-playlist-list"
        hidden={!sidebarPlaylistsOpen}
      >
        {visiblePlaylists.length ? (
          visiblePlaylists.map((playlist) => (
            <button
              className={`sidebar-playlist-item${route.view === 'playlist' && route.id === playlist.id ? ' is-active' : ''}`}
              type="button"
              key={playlist.id}
              title={playlist.name}
              onClick={() => onNavigate({ view: 'playlist', id: playlist.id })}
            >
              <span className="sidebar-playlist-cover" aria-hidden="true">
                <PlaylistCover playlist={playlist} />
              </span>
              <span className="sidebar-playlist-name">{playlist.name}</span>
            </button>
          ))
        ) : (
          <div className="sidebar-playlist-empty">No playlists yet</div>
        )}
      </div>
    </section>
  );
}
