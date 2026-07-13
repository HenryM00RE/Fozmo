import type { RouteState } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { settingsTabFromValue, visibleSettingsSections } from './settingsModel';

type SettingsSidebarNavProps = {
  globalSearchOpen: boolean;
  onNavigate: (next: RouteState) => void;
  onOpenSearch: () => void;
  route: RouteState;
  status: Record<string, unknown>;
};

export function SettingsSidebarNav({
  globalSearchOpen,
  onNavigate,
  onOpenSearch,
  route,
  status
}: SettingsSidebarNavProps) {
  const sections = visibleSettingsSections(status);
  const activeSettingsTab =
    route.view === 'settings' ? settingsTabFromValue(route.id, 'general', status) : null;

  return (
    <>
      <div className="app-sidebar-header">
        <button
          className="app-sidebar-brand"
          type="button"
          onClick={() => onNavigate({ view: 'home' })}
        >
          Settings
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
      <nav className="main-nav settings-main-nav" aria-label="Settings sections">
        {sections.map((section) => (
          <button
            className={`settings-nav-item${activeSettingsTab === section.id ? ' is-active' : ''}`}
            type="button"
            key={section.id}
            onClick={() => onNavigate({ view: 'settings', id: section.id })}
          >
            <Icon path={section.path} />
            {section.label}
          </button>
        ))}
      </nav>
      <button
        className="settings-sidebar-back settings-nav-item"
        type="button"
        onClick={() => onNavigate({ view: 'home' })}
      >
        <Icon path="m15 18-6-6 6-6" />
        Home
      </button>
    </>
  );
}
