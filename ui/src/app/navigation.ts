import type { RouteState, ViewId } from '../shared/types';

export type SidebarNavItem = {
  view: ViewId;
  label: string;
  path: string;
  activeViews?: ViewId[];
};

export type MobileTabId = 'home' | 'discover' | 'library' | 'search';

export const primaryNavItems: SidebarNavItem[] = [
  {
    view: 'home',
    label: 'Home',
    path: 'M3 10a2 2 0 0 1 .71-1.53l7-6a2 2 0 0 1 2.58 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z'
  },
  {
    view: 'discover',
    label: 'Discover',
    path: 'M10.5 17a6.5 6.5 0 1 1 0-13 6.5 6.5 0 0 1 0 13Z M16 16l4 4',
    activeViews: ['qobuz-playlist']
  },
  { view: 'history', label: 'History', path: 'M12 8v5l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z' }
];

export const libraryNavItems: SidebarNavItem[] = [
  {
    view: 'albums',
    label: 'Albums',
    path: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4Z',
    activeViews: ['album', 'qobuz-album']
  },
  {
    view: 'songs',
    label: 'Songs',
    path: 'M21 15V6M18.5 18a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5ZM12 12H3M16 6H3M12 18H3'
  },
  {
    view: 'artists',
    label: 'Artists',
    path: 'M18 21a8 8 0 0 0-16 0M10 13a5 5 0 1 0 0-10 5 5 0 0 0 0 10ZM22 20c0-3.37-2-6.5-4-8a5 5 0 0 0-.45-8.3',
    activeViews: ['artist']
  }
];

export const settingsNavItem: SidebarNavItem = {
  view: 'settings',
  label: 'Settings',
  path: 'M9.67 4.14a2.34 2.34 0 0 1 4.66 0 2.34 2.34 0 0 0 3.32 1.91 2.34 2.34 0 0 1 2.33 4.03 2.34 2.34 0 0 0 0 3.84 2.34 2.34 0 0 1-2.33 4.03 2.34 2.34 0 0 0-3.32 1.91 2.34 2.34 0 0 1-4.66 0 2.34 2.34 0 0 0-3.32-1.91 2.34 2.34 0 0 1-2.33-4.03 2.34 2.34 0 0 0 0-3.84 2.34 2.34 0 0 1 2.33-4.03 2.34 2.34 0 0 0 3.32-1.91Z M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z'
};

export const mobileLibraryViews = new Set<ViewId>([
  'library',
  'history',
  'albums',
  'album',
  'songs',
  'artists',
  'artist',
  'playlists',
  'playlist',
  'qobuz-album'
]);

export function mobileTabForRoute(route: RouteState, searchOpen: boolean): MobileTabId {
  if (searchOpen) return 'search';
  if (route.view === 'home') return 'home';
  if (route.view === 'discover' || route.view === 'qobuz-playlist') return 'discover';
  if (mobileLibraryViews.has(route.view)) return 'library';
  return 'home';
}

export function navItemIsActive(routeView: ViewId, item: SidebarNavItem) {
  return routeView === item.view || Boolean(item.activeViews?.includes(routeView));
}

export function routeForNavItem(item: SidebarNavItem): RouteState {
  return item.view === 'settings' ? { view: 'settings', id: 'general' } : { view: item.view };
}

export function playlistRouteActive(view: ViewId) {
  return view === 'playlists' || view === 'playlist';
}
