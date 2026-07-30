import { type RefObject, useEffect, useState } from 'react';

/* Anything above this reads as a deliberate scroll rather than a trackpad
   twitch or the elastic overscroll Safari reports as a fractional scrollTop. */
const SCROLL_THRESHOLD = 2;

/**
 * Tracks whether the workspace scroller has moved off its top, so the toolbar
 * can show a divider only once content is passing underneath it.
 *
 * The scroller is `.workspace > .view`, which each page renders itself, so it
 * is remounted on every route change and can never be captured by a ref here.
 * The listener is registered on the workspace in the capture phase instead:
 * scroll events do not bubble, but they do propagate downwards, so one
 * listener on the stable ancestor covers whichever `.view` is currently
 * mounted. Programmatic scrolls fire the event too, so restores are covered.
 *
 * `routeKey` re-reads the live scrollTop after navigation: a fresh view mounts
 * at the top without firing a scroll event, and the flag would otherwise stay
 * stuck on from the page that was left behind.
 */
export function useWorkspaceScrolled(
  workspaceRef: RefObject<HTMLElement | null>,
  routeKey: string
) {
  const [scrolled, setScrolled] = useState(false);

  useEffect(() => {
    const workspace = workspaceRef.current;
    if (!workspace) return;

    const handleScroll = (event: Event) => {
      const target = event.target;
      if (!(target instanceof HTMLElement) || !target.classList.contains('view')) return;
      setScrolled(target.scrollTop > SCROLL_THRESHOLD);
    };

    workspace.addEventListener('scroll', handleScroll, true);
    const view = workspace.querySelector<HTMLElement>(':scope > .view');
    setScrolled((view?.scrollTop ?? 0) > SCROLL_THRESHOLD);

    return () => workspace.removeEventListener('scroll', handleScroll, true);
  }, [routeKey, workspaceRef]);

  return scrolled;
}
