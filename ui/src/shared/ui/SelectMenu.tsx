import {
  type CSSProperties,
  type ReactNode,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState
} from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './Icon';

export type SelectMenuOption = {
  value: string;
  label: string;
  after?: ReactNode;
  color?: string;
  disabled?: boolean;
};

const MENU_GAP = 8;
const VIEWPORT_MARGIN = 12;
const MIN_MENU_HEIGHT = 80;

export function SelectMenu({
  ariaLabel,
  className = '',
  disabled = false,
  menuClassName = '',
  menuMinWidth = 0,
  onChange,
  options,
  triggerIconPath,
  value
}: {
  ariaLabel: string;
  className?: string;
  disabled?: boolean;
  menuClassName?: string;
  menuMinWidth?: number;
  onChange: (value: string) => void;
  options: SelectMenuOption[];
  triggerIconPath?: string;
  value: string;
}) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [open, setOpen] = useState(false);
  const [placement, setPlacement] = useState<'below' | 'above'>('below');
  const [menuStyle, setMenuStyle] = useState<CSSProperties>({});
  const selectedIndex = Math.max(
    0,
    options.findIndex((option) => option.value === value)
  );
  const selected = options[selectedIndex] || options[0];
  const enabledOptions = useMemo(() => options.filter((option) => !option.disabled), [options]);

  const updatePlacement = () => {
    const root = rootRef.current;
    const menu = menuRef.current;
    if (!root || !menu) return;

    const rootRect = root.getBoundingClientRect();
    const bottomChromeTop = bottomChromeTopBoundary();
    const bottomLimit = Number.isFinite(bottomChromeTop)
      ? Math.min(window.innerHeight, bottomChromeTop as number)
      : window.innerHeight;
    const spaceBelow = Math.max(0, bottomLimit - rootRect.bottom - MENU_GAP - VIEWPORT_MARGIN);
    const spaceAbove = Math.max(0, rootRect.top - MENU_GAP - VIEWPORT_MARGIN);
    const menuHeight = menu.scrollHeight;
    const nextPlacement = menuHeight > spaceBelow && spaceAbove > spaceBelow ? 'above' : 'below';
    const availableSpace = nextPlacement === 'above' ? spaceAbove : spaceBelow;
    const renderedHeight = Math.min(menuHeight, Math.max(MIN_MENU_HEIGHT, availableSpace));
    const nextMaxHeight = Math.max(MIN_MENU_HEIGHT, Math.floor(availableSpace));
    const menuWidth = Math.min(
      Math.max(rootRect.width, menuMinWidth),
      window.innerWidth - VIEWPORT_MARGIN * 2
    );
    const menuLeft = Math.min(
      Math.max(VIEWPORT_MARGIN, rootRect.left + (rootRect.width - menuWidth) / 2),
      window.innerWidth - VIEWPORT_MARGIN - menuWidth
    );
    const nextStyle: CSSProperties = {
      left: menuLeft,
      width: menuWidth,
      maxHeight: nextMaxHeight
    };

    if (nextPlacement === 'above') {
      nextStyle.top = rootRect.top - MENU_GAP - renderedHeight;
    } else {
      nextStyle.top = rootRect.bottom + MENU_GAP;
    }

    setPlacement((current) => (current === nextPlacement ? current : nextPlacement));
    setMenuStyle(nextStyle);
  };

  useLayoutEffect(() => {
    if (open) updatePlacement();
  }, [open, options.length, value, menuMinWidth]);

  useEffect(() => {
    if (!open) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!rootRef.current?.contains(target) && !menuRef.current?.contains(target)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    const onReposition = () => updatePlacement();
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    window.addEventListener('resize', onReposition);
    window.addEventListener('scroll', onReposition, true);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('resize', onReposition);
      window.removeEventListener('scroll', onReposition, true);
    };
  }, [open]);

  const selectRelativeOption = (direction: 1 | -1) => {
    if (!enabledOptions.length) return;
    const enabledIndex = Math.max(
      0,
      enabledOptions.findIndex((option) => option.value === value)
    );
    const next =
      enabledOptions[(enabledIndex + direction + enabledOptions.length) % enabledOptions.length];
    onChange(next.value);
  };

  const menu = open ? (
    <div
      className={`app-select-menu${placement === 'above' ? ' is-above' : ''}${menuClassName ? ` ${menuClassName}` : ''}`}
      role="listbox"
      aria-label={ariaLabel}
      ref={menuRef}
      style={menuStyle}
    >
      {options.map((option) => (
        <button
          className={`app-select-option${option.value === value ? ' is-selected' : ''}`}
          type="button"
          role="option"
          aria-selected={option.value === value}
          disabled={option.disabled}
          key={option.value}
          onClick={() => {
            onChange(option.value);
            setOpen(false);
          }}
        >
          <span>
            {option.color ? (
              <i
                className="app-select-swatch"
                style={{ '--select-swatch': option.color } as CSSProperties}
              />
            ) : null}
            {option.label}
            {option.after}
          </span>
          {option.value === value ? <Icon path="M20 6 9 17l-5-5" /> : null}
        </button>
      ))}
    </div>
  ) : null;

  return (
    <>
      <div
        className={`app-select${open ? ' is-open' : ''}${placement === 'above' ? ' is-above' : ''}${disabled ? ' is-disabled' : ''}${triggerIconPath ? ' is-icon-trigger' : ''}${className ? ` ${className}` : ''}`}
        ref={rootRef}
      >
        <button
          className="app-select-trigger"
          type="button"
          aria-label={ariaLabel}
          aria-haspopup="listbox"
          aria-expanded={open}
          disabled={disabled}
          title={triggerIconPath ? selected?.label : undefined}
          onClick={() => setOpen((current) => !current)}
          onKeyDown={(event) => {
            if (event.key === 'ArrowDown') {
              event.preventDefault();
              if (open) selectRelativeOption(1);
              else setOpen(true);
            } else if (event.key === 'ArrowUp') {
              event.preventDefault();
              if (open) selectRelativeOption(-1);
              else setOpen(true);
            }
          }}
        >
          {triggerIconPath ? (
            <Icon path={triggerIconPath} />
          ) : (
            <>
              <span>
                {selected?.color ? (
                  <i
                    className="app-select-swatch"
                    style={{ '--select-swatch': selected.color } as CSSProperties}
                  />
                ) : null}
                {selected?.label || ''}
              </span>
              <Icon path="m6 9 6 6 6-6" />
            </>
          )}
        </button>
      </div>
      {menu ? createPortal(menu, document.body) : null}
    </>
  );
}

function bottomChromeTopBoundary() {
  const chromeElements = document.querySelectorAll<HTMLElement>('.player-bar, .mobile-mini-player');
  for (const element of chromeElements) {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    if (style.display !== 'none' && rect.width > 0 && rect.height > 0) return rect.top;
  }
  return Number.POSITIVE_INFINITY;
}
