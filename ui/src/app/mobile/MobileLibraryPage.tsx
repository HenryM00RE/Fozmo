import type { RouteState } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import { libraryNavItems } from '../navigation';

type MobileLibraryPageProps = {
  onNavigate: (next: RouteState) => void;
};

const mobileLibraryItems = [
  ...libraryNavItems.map((item) => ({
    label: item.label,
    path: item.path,
    route: { view: item.view } as RouteState
  })),
  {
    label: 'Playlists',
    path: 'M4 6.5h10M4 12h10M4 17.5h6M18 8v8M15 13l3 3 3-3',
    route: { view: 'playlists' } as RouteState
  },
  {
    label: 'History',
    path: 'M12 8v5l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z',
    route: { view: 'history' } as RouteState
  }
];

export function MobileLibraryPage({ onNavigate }: MobileLibraryPageProps) {
  return (
    <section className="view mobile-library-view">
      <div className="library-page-heading">
        <div>
          <div className="section-label">Collection</div>
          <h1>Library</h1>
        </div>
      </div>
      <div className="mobile-library-list" aria-label="Library sections">
        {mobileLibraryItems.map((item) => (
          <button
            className="mobile-library-row"
            type="button"
            key={item.label}
            onClick={() => onNavigate(item.route)}
          >
            <span className="mobile-library-row-icon">
              <Icon path={item.path} />
            </span>
            <span>{item.label}</span>
          </button>
        ))}
      </div>
    </section>
  );
}
