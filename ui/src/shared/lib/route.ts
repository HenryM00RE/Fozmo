import type { RouteState } from '../types';

export function routeToHash(route: RouteState) {
  const id =
    route.id === undefined || route.id === null ? '' : `/${encodeURIComponent(String(route.id))}`;
  return `#/${route.view}${id}`;
}

export function routeFromHash(hash: string): RouteState {
  const raw = hash.replace(/^#\/?/, '');
  if (!raw) return { view: 'home' };
  const [view, id] = raw.split('/');
  const legacyViews: Record<string, RouteState['view']> = {
    'home-view': 'home',
    'discover-view': 'discover',
    'library-view': 'library',
    'history-view': 'history',
    'albums-view': 'albums',
    'songs-view': 'songs',
    'artists-view': 'artists',
    'qobuz-view': 'settings',
    'playlists-view': 'playlists',
    'settings-view': 'settings'
  };
  if (legacyViews[view]) return { view: legacyViews[view], id: id ? decodeURIComponent(id) : null };
  const known = new Set([
    'home',
    'discover',
    'library',
    'history',
    'albums',
    'album',
    'songs',
    'artists',
    'artist',
    'qobuz-album',
    'qobuz-playlist',
    'playlists',
    'playlist',
    'settings'
  ]);
  return {
    view: known.has(view) ? (view as RouteState['view']) : 'home',
    id: id ? decodeURIComponent(id) : null
  };
}
