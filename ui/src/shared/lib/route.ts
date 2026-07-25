import type { RouteState } from '../types';

export function routeToHash(route: RouteState) {
  const id =
    route.id === undefined || route.id === null ? '' : `/${encodeURIComponent(String(route.id))}`;
  const search = new URLSearchParams();
  if (route.view === 'album' && route.provider === 'apple_music') {
    search.set('provider', 'apple_music');
    if (route.storefront) search.set('storefront', route.storefront);
  }
  const query = search.toString();
  return `#/${route.view}${id}${query ? `?${query}` : ''}`;
}

export function routeFromHash(hash: string): RouteState {
  const raw = hash.replace(/^#\/?/, '');
  if (!raw) return { view: 'home' };
  const [path, query = ''] = raw.split('?', 2);
  const [view, id] = path.split('/');
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
  const resolvedView = known.has(view) ? (view as RouteState['view']) : 'home';
  const search = new URLSearchParams(query);
  const provider =
    resolvedView === 'album' && search.get('provider') === 'apple_music'
      ? ('apple_music' as const)
      : null;
  const storefront = provider ? search.get('storefront') : null;
  return {
    view: resolvedView,
    id: id ? decodeURIComponent(id) : null,
    ...(provider ? { provider } : {}),
    ...(storefront ? { storefront } : {})
  };
}
