import { type ReactNode, useState } from 'react';
import type { PlaylistShellState } from '../features/playlists/model/playlistShellState';
import { PlaylistSidebarSection } from '../features/playlists/PlaylistSidebarSection';
import type { ProfileShellState } from '../features/settings/model/profileShellState';
import { ProfileMenu } from '../features/settings/ProfileMenu';
import { SettingsSidebarNav } from '../features/settings/SettingsSidebarNav';
import { storageKey } from '../shared/identity';
import { capabilityEnabled } from '../shared/lib/capabilities';
import type { JsonRecord, RouteState } from '../shared/types';
import { Icon } from '../shared/ui/Icon';
import { SelectionActionsToolbar } from '../shared/ui/SelectionActionsToolbar';
import type { SelectionToolbarState } from '../shared/ui/selectionToolbar';
import type { ToolbarAction } from '../shared/ui/toolbar';
import { MobileNavDrawer } from './mobile/MobileNavDrawer';
import { MobileTopBar } from './mobile/MobileTopBar';
import {
  libraryNavItems,
  navItemIsActive,
  primaryNavItems,
  routeForNavItem,
  settingsNavItem
} from './navigation';

const SIDEBAR_LIBRARY_OPEN_KEY = storageKey('SidebarLibraryOpen');
type MobileNavMode = 'main' | 'settings';

type AppShellProps = {
  children: ReactNode;
  chrome?: ReactNode;
  globalSearchOpen: boolean;
  notice: string;
  noticeKey: number;
  onNavigate: (next: RouteState) => void;
  onOpenSearch: () => void;
  onNotice: (message: string) => void;
  playlistShell: PlaylistShellState;
  profileShell: ProfileShellState;
  route: RouteState;
  selectionToolbar: SelectionToolbarState;
  status: JsonRecord;
  toolbarAction: ToolbarAction | null;
};

export function AppShell({
  children,
  chrome,
  globalSearchOpen,
  notice,
  noticeKey,
  onNavigate,
  onOpenSearch,
  onNotice,
  playlistShell,
  profileShell,
  route,
  selectionToolbar,
  status,
  toolbarAction
}: AppShellProps) {
  const { activeProfileId, profiles, refreshProfileScopedData, selectProfile } = profileShell;
  const { activeSelectionType, clearAlbumTrackSelection, clearRecentSelection } = selectionToolbar;
  const isSettingsRoute = route.view === 'settings';
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [mobileNavMode, setMobileNavMode] = useState<MobileNavMode>('main');
  const [sidebarLibraryOpen, setSidebarLibraryOpen] = useState(() => {
    try {
      return window.localStorage.getItem(SIDEBAR_LIBRARY_OPEN_KEY) !== 'false';
    } catch {
      return true;
    }
  });

  const toggleSidebarLibrary = () => {
    setSidebarLibraryOpen((open) => {
      const nextOpen = !open;
      try {
        window.localStorage.setItem(SIDEBAR_LIBRARY_OPEN_KEY, nextOpen ? 'true' : 'false');
      } catch {
        // Ignore storage failures; the visual state still updates for this session.
      }
      return nextOpen;
    });
  };

  return (
    <div className="react-app app-shell">
      <MobileTopBar
        globalSearchOpen={globalSearchOpen}
        notice={notice}
        noticeKey={noticeKey}
        onNavigate={onNavigate}
        onNotice={onNotice}
        onOpenMenu={() => {
          setMobileNavMode(isSettingsRoute ? 'settings' : 'main');
          setMobileNavOpen(true);
        }}
        onOpenSearch={onOpenSearch}
        playlistShell={playlistShell}
        profileShell={profileShell}
        route={route}
        selectionToolbar={selectionToolbar}
        toolbarAction={toolbarAction}
      />
      <MobileNavDrawer
        mode={mobileNavMode}
        open={mobileNavOpen}
        onClose={() => setMobileNavOpen(false)}
      >
        {mobileNavMode === 'settings' ? (
          <SettingsSidebarNav
            globalSearchOpen={globalSearchOpen}
            onOpenSearch={onOpenSearch}
            route={route}
            status={status}
            onNavigate={(next) => {
              setMobileNavOpen(false);
              onNavigate(next);
            }}
          />
        ) : (
          <MainSidebarContent
            onNavigate={(next) => {
              setMobileNavOpen(false);
              onNavigate(next);
            }}
            playlistShell={playlistShell}
            route={route}
            status={status}
            sidebarLibraryListId="mobile-sidebar-library-list"
            sidebarLibraryOpen={sidebarLibraryOpen}
            toggleSidebarLibrary={toggleSidebarLibrary}
            onOpenSettingsNav={() => setMobileNavMode('settings')}
            globalSearchOpen={globalSearchOpen}
            onOpenSearch={onOpenSearch}
          />
        )}
      </MobileNavDrawer>
      <aside
        className={`app-sidebar${isSettingsRoute ? ' app-sidebar-settings-mode' : ' app-sidebar-main-mode'}`}
      >
        {isSettingsRoute ? (
          <SettingsSidebarNav
            globalSearchOpen={globalSearchOpen}
            onOpenSearch={onOpenSearch}
            route={route}
            status={status}
            onNavigate={onNavigate}
          />
        ) : (
          <MainSidebarContent
            onNavigate={onNavigate}
            playlistShell={playlistShell}
            route={route}
            status={status}
            sidebarLibraryListId="sidebar-library-list"
            sidebarLibraryOpen={sidebarLibraryOpen}
            toggleSidebarLibrary={toggleSidebarLibrary}
            globalSearchOpen={globalSearchOpen}
            onOpenSearch={onOpenSearch}
          />
        )}
      </aside>

      <main className="workspace">
        <header className="app-toolbar" aria-label="Page controls">
          <div className="toolbar-left">
            <button
              className="btn-ghost"
              type="button"
              title="Go back"
              aria-label="Go back"
              onClick={() => window.history.back()}
            >
              <Icon path="m15 18-6-6 6-6" />
            </button>
            <button
              className="btn-ghost"
              type="button"
              title="Go forward"
              aria-label="Go forward"
              onClick={() => window.history.forward()}
            >
              <Icon path="m9 18 6-6-6-6" />
            </button>
            <SelectionActionsToolbar selectionToolbar={selectionToolbar} />
          </div>
          <div className="toolbar-right">
            {notice ? (
              <span className="react-notice" data-testid="app-notice" key={noticeKey}>
                {notice}
              </span>
            ) : null}
            {toolbarAction ? (
              <button
                className="toolbar-action-button"
                type="button"
                title={toolbarAction.title || toolbarAction.label}
                aria-label={toolbarAction.label}
                onClick={() => {
                  Promise.resolve(toolbarAction.onClick()).catch((error) =>
                    onNotice(error instanceof Error ? error.message : 'Action failed')
                  );
                }}
              >
                <Icon path="M21 12a9 9 0 1 1-2.64-6.36M21 4v7h-7" />
                <span>{toolbarAction.label}</span>
              </button>
            ) : null}
            <ProfileMenu
              profiles={profiles}
              activeProfileId={activeProfileId}
              selectionActive={Boolean(activeSelectionType)}
              onExitSelection={() => {
                if (activeSelectionType === 'album-tracks') clearAlbumTrackSelection();
                else if (activeSelectionType === 'recently-played') clearRecentSelection();
              }}
              onSelectProfile={selectProfile}
              onRefresh={refreshProfileScopedData}
              onOpenSettings={() => onNavigate({ view: 'settings', id: 'general' })}
              onNotice={onNotice}
            />
          </div>
        </header>
        {children}
      </main>
      {chrome}
    </div>
  );
}

type MainSidebarContentProps = {
  globalSearchOpen: boolean;
  onNavigate: (next: RouteState) => void;
  onOpenSearch: () => void;
  onOpenSettingsNav?: () => void;
  playlistShell: PlaylistShellState;
  route: RouteState;
  sidebarLibraryListId: string;
  sidebarLibraryOpen: boolean;
  status: JsonRecord;
  toggleSidebarLibrary: () => void;
};

function MainSidebarContent({
  globalSearchOpen,
  onNavigate,
  onOpenSearch,
  onOpenSettingsNav,
  playlistShell,
  route,
  sidebarLibraryListId,
  sidebarLibraryOpen,
  status,
  toggleSidebarLibrary
}: MainSidebarContentProps) {
  const visiblePrimaryNavItems = primaryNavItems.filter(
    (item) => item.view !== 'discover' || capabilityEnabled(status, 'qobuz')
  );
  return (
    <>
      <div className="app-sidebar-header">
        <button
          className="app-sidebar-brand"
          type="button"
          onClick={() => onNavigate({ view: 'home' })}
        >
          Fozmo
        </button>
        <button
          className={`sidebar-global-search-trigger global-search-trigger${globalSearchOpen ? ' is-active' : ''}`}
          type="button"
          title="Search"
          aria-label="Search"
          aria-haspopup="dialog"
          aria-expanded={globalSearchOpen}
          onClick={onOpenSearch}
        >
          <Icon path="M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Zm9 2-3.5-3.5" />
        </button>
      </div>
      <div className="app-sidebar-section-divider" aria-hidden="true" />
      <nav className="main-nav" aria-label="Library">
        {visiblePrimaryNavItems.map((item) => (
          <button
            className={`settings-nav-item${navItemIsActive(route.view, item) ? ' is-active' : ''}`}
            type="button"
            key={item.view}
            onClick={() => onNavigate(routeForNavItem(item))}
          >
            <Icon path={item.path} />
            {item.label}
          </button>
        ))}
        <section
          className={`sidebar-library-section${sidebarLibraryOpen ? ' is-open' : ''}`}
          aria-label="Library"
        >
          <button
            className="settings-nav-item sidebar-library-toggle"
            type="button"
            aria-expanded={sidebarLibraryOpen}
            aria-controls={sidebarLibraryListId}
            onClick={toggleSidebarLibrary}
          >
            <Icon path="m9 18 6-6-6-6" />
            <span>Library</span>
          </button>
          <div
            className="sidebar-library-list"
            id={sidebarLibraryListId}
            hidden={!sidebarLibraryOpen}
          >
            {libraryNavItems.map((item) => (
              <button
                className={`settings-nav-item sidebar-library-item${navItemIsActive(route.view, item) ? ' is-active' : ''}`}
                type="button"
                key={item.view}
                onClick={() => onNavigate(routeForNavItem(item))}
              >
                <Icon path={item.path} />
                {item.label}
              </button>
            ))}
          </div>
        </section>
        <PlaylistSidebarSection
          playlistShell={playlistShell}
          route={route}
          onNavigate={onNavigate}
        />
      </nav>
      <button
        className={`sidebar-settings-bottom settings-nav-item${navItemIsActive(route.view, settingsNavItem) ? ' is-active' : ''}`}
        type="button"
        onClick={() => {
          if (onOpenSettingsNav) {
            onOpenSettingsNav();
            return;
          }
          onNavigate(routeForNavItem(settingsNavItem));
        }}
      >
        <Icon path={settingsNavItem.path} />
        {settingsNavItem.label}
      </button>
    </>
  );
}
