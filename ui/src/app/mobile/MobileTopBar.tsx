import type { PlaylistShellState } from '../../features/playlists/model/playlistShellState';
import type { ProfileShellState } from '../../features/settings/model/profileShellState';
import { ProfileMenu } from '../../features/settings/ProfileMenu';
import type { RouteState } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { SelectionActionsToolbar } from '../../shared/ui/SelectionActionsToolbar';
import type { SelectionToolbarState } from '../../shared/ui/selectionToolbar';
import type { ToolbarAction } from '../../shared/ui/toolbar';

type MobileTopBarProps = {
  notice: string;
  noticeKey: number;
  globalSearchOpen: boolean;
  onNavigate: (next: RouteState) => void;
  onNotice: (message: string) => void;
  onOpenMenu: () => void;
  onOpenSearch: () => void;
  playlistShell: PlaylistShellState;
  profileShell: ProfileShellState;
  route: RouteState;
  selectionToolbar: SelectionToolbarState;
  toolbarAction: ToolbarAction | null;
};

export function MobileTopBar({
  notice,
  noticeKey,
  globalSearchOpen,
  onNavigate,
  onNotice,
  onOpenMenu,
  onOpenSearch,
  profileShell,
  selectionToolbar,
  toolbarAction
}: MobileTopBarProps) {
  const { activeProfileId, profiles, refreshProfileScopedData, selectProfile } = profileShell;
  const { activeSelectionType, clearAlbumTrackSelection, clearRecentSelection } = selectionToolbar;

  return (
    <header className="mobile-top-bar" aria-label="Page controls">
      <div className={`mobile-top-bar-main${activeSelectionType ? ' is-selection-mode' : ''}`}>
        <button
          className="btn-ghost mobile-menu-trigger"
          type="button"
          title="Navigation"
          aria-label="Open navigation"
          onClick={onOpenMenu}
        >
          <Icon path="M4 7h16M4 12h16M4 17h16" />
        </button>
        {activeSelectionType ? (
          <div className="mobile-selection-toolbar">
            <SelectionActionsToolbar selectionToolbar={selectionToolbar} />
          </div>
        ) : (
          <>
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
          </>
        )}
        <div className="mobile-top-actions">
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
            </button>
          ) : null}
          <button
            className={`global-search-trigger${globalSearchOpen ? ' is-active' : ''}`}
            type="button"
            title="Search"
            aria-label="Search"
            aria-haspopup="dialog"
            onClick={onOpenSearch}
          >
            <Icon path="M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Zm9 2-3.5-3.5" />
          </button>
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
      </div>
      {notice ? (
        <span className="mobile-notice" key={noticeKey}>
          {notice}
        </span>
      ) : null}
    </header>
  );
}
