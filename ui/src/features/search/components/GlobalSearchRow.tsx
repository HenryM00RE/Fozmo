import { endpoints } from '../../../shared/lib/api';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { PlayNextIcon } from '../../../shared/ui/PlayNextIcon';
import type { GlobalSearchRowModel } from '../globalSearchModel';

function GlobalSearchArt({ row }: { row: GlobalSearchRowModel }) {
  const artUrl = row.imageUrl || endpoints.artUrl(row.artId);
  const icon =
    row.kind === 'album' ? (
      <Icon path="M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4Z" />
    ) : row.kind === 'artist' ? (
      <Icon path="M16 11a4 4 0 1 0-8 0M4 21a8 8 0 0 1 16 0" />
    ) : (
      <Icon path="M9 18V5l11-2v13M6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM17 19a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z" />
    );
  return (
    <span className="global-search-art">
      {artUrl ? <img alt="" src={artUrl} loading="lazy" /> : icon}
    </span>
  );
}

export function GlobalSearchRow({
  row,
  active = false,
  featured = false,
  menuOpen,
  onToggleMenu,
  onMoveActive,
  onRun,
  onRequestClose,
  onSelect
}: {
  row: GlobalSearchRowModel;
  active?: boolean;
  featured?: boolean;
  menuOpen: boolean;
  onToggleMenu: (buttonRect: DOMRect) => void;
  onMoveActive: (delta: number) => void;
  onRun?: () => void;
  onRequestClose: () => void;
  onSelect: () => void;
}) {
  const runRow = () => {
    onRun?.();
    Promise.resolve(row.run()).catch(() => undefined);
  };
  return (
    <div
      className={`global-search-row${active ? ' is-active' : ''}${featured ? ' is-featured' : ''}`}
      tabIndex={0}
      role="button"
      aria-current={active ? 'true' : undefined}
      onMouseEnter={onSelect}
      onFocus={onSelect}
      onClick={(event) => {
        if ((event.target as HTMLElement).closest('button')) return;
        runRow();
      }}
      onKeyDown={(event) => {
        if (event.key === 'ArrowDown') {
          event.preventDefault();
          onMoveActive(1);
          return;
        }
        if (event.key === 'ArrowUp') {
          event.preventDefault();
          onMoveActive(-1);
          return;
        }
        if (event.key === 'Escape') {
          event.preventDefault();
          onRequestClose();
          return;
        }
        if (event.key !== 'Enter' && event.key !== ' ') return;
        if ((event.target as HTMLElement).closest('button')) return;
        event.preventDefault();
        runRow();
      }}
    >
      <GlobalSearchArt row={row} />
      <span className="global-search-copy">
        <span className="global-search-name" title={row.title}>
          <span>{row.title}</span>
          {row.titleBadge ? (
            <span className="global-search-title-badge">{row.titleBadge}</span>
          ) : null}
        </span>
        {row.subtitle ? <span className="global-search-meta">{row.subtitle}</span> : null}
      </span>
      <span className="global-search-badges">
        <span className={`global-search-kind is-${row.kind}`}>{row.kindLabel}</span>
      </span>
      {row.actions?.length ? (
        <span className="global-search-menu-wrap">
          <button
            className="global-search-menu-button"
            type="button"
            aria-label={`Queue options for ${row.title}`}
            title="Queue options"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={(event) => {
              event.stopPropagation();
              onToggleMenu(event.currentTarget.getBoundingClientRect());
            }}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="12" cy="12" r="1" />
              <circle cx="12" cy="5" r="1" />
              <circle cx="12" cy="19" r="1" />
            </svg>
          </button>
        </span>
      ) : row.hideAction ? null : (
        <span className="global-search-action">{row.actionLabel}</span>
      )}
    </div>
  );
}

export function GlobalSearchActionsMenu({
  row,
  x,
  y,
  onCloseMenu,
  onRun
}: {
  row: GlobalSearchRowModel;
  x: number;
  y: number;
  onCloseMenu: () => void;
  onRun?: () => void;
}) {
  if (!row.actions?.length) return null;
  return (
    <Menu
      className="track-actions-menu track-actions-menu-wide is-open"
      ariaLabel={`Queue options for ${row.title}`}
      style={{ left: Math.max(12, x), top: y }}
      onClick={(event) => event.stopPropagation()}
    >
      {row.actions.map((action) => (
        <button
          className={`track-action-item${action.filled ? ' has-filled-icon' : ''}`}
          type="button"
          role="menuitem"
          key={action.id}
          onClick={() => {
            onCloseMenu();
            onRun?.();
            Promise.resolve(action.run()).catch(() => undefined);
          }}
        >
          {action.icon === 'play-next' ? (
            <PlayNextIcon />
          ) : action.path ? (
            <Icon path={action.path} />
          ) : null}
          <span>{action.label}</span>
        </button>
      ))}
    </Menu>
  );
}
