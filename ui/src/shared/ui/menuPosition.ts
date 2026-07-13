const defaultMenuWidth = 206;
const defaultMenuHeight = 188;
const defaultEdgeGap = 12;
const defaultVerticalGap = 8;
const defaultAboveVerticalGap = 3;

type ActionMenuPositionOptions = {
  menuHeight?: number;
  menuWidth?: number;
};

function appViewportScale() {
  if (typeof document === 'undefined') return 1;
  const app = document.querySelector('.react-app') as HTMLElement | null;
  if (!app?.offsetWidth) return 1;
  const scale = app.getBoundingClientRect().width / app.offsetWidth;
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

function clampActionMenuX(x: number, scale: number, menuWidth: number) {
  const edgeGap = defaultEdgeGap / scale;
  if (typeof window === 'undefined') return Math.max(edgeGap, x);
  const viewportRight = (window.innerWidth - defaultEdgeGap) / scale;
  return Math.min(Math.max(edgeGap, x), viewportRight - menuWidth);
}

function bottomChromeTopBoundary() {
  if (typeof document === 'undefined') return null;
  const chromeElements = document.querySelectorAll<HTMLElement>('.player-bar, .mobile-mini-player');
  for (const element of chromeElements) {
    const rect = element.getBoundingClientRect();
    const style = window.getComputedStyle(element);
    if (style.display !== 'none' && rect.width > 0 && rect.height > 0) return rect.top;
  }
  return null;
}

export function actionMenuPosition(rect: DOMRect, options: ActionMenuPositionOptions = {}) {
  const menuWidth = options.menuWidth ?? defaultMenuWidth;
  const menuHeight = options.menuHeight ?? defaultMenuHeight;
  const scale = appViewportScale();
  const bottomBoundary =
    bottomChromeTopBoundary() ??
    (typeof window === 'undefined'
      ? Number.POSITIVE_INFINITY
      : window.innerHeight - defaultEdgeGap);
  const belowY = rect.bottom + defaultVerticalGap;
  const aboveY = rect.top - defaultAboveVerticalGap - menuHeight;
  const y = belowY + menuHeight > bottomBoundary ? Math.max(defaultEdgeGap, aboveY) : belowY;

  return {
    x: clampActionMenuX(rect.right / scale - menuWidth, scale, menuWidth),
    y: y / scale
  };
}
