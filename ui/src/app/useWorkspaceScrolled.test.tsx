// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { createRef } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
import { useWorkspaceScrolled } from './useWorkspaceScrolled';

function mountWorkspace(scrollTop = 0) {
  const workspace = document.createElement('main');
  workspace.className = 'workspace';
  const view = document.createElement('section');
  view.className = 'view albums-view';
  workspace.appendChild(view);
  document.body.appendChild(workspace);
  Object.defineProperty(view, 'scrollTop', { value: scrollTop, writable: true });
  return { view, workspace };
}

function scrollTo(view: HTMLElement, top: number) {
  act(() => {
    (view as HTMLElement & { scrollTop: number }).scrollTop = top;
    view.dispatchEvent(new Event('scroll'));
  });
}

afterEach(() => {
  document.body.replaceChildren();
});

describe('useWorkspaceScrolled', () => {
  it('follows the scroller mounted inside the workspace', () => {
    const { view, workspace } = mountWorkspace();
    const ref = createRef<HTMLElement>();
    (ref as { current: HTMLElement | null }).current = workspace;

    const { result } = renderHook(() => useWorkspaceScrolled(ref, 'albums:'));
    expect(result.current).toBe(false);

    scrollTo(view, 120);
    expect(result.current).toBe(true);

    scrollTo(view, 0);
    expect(result.current).toBe(false);
  });

  it('ignores sub-pixel overscroll so the divider does not flicker at rest', () => {
    const { view, workspace } = mountWorkspace();
    const ref = createRef<HTMLElement>();
    (ref as { current: HTMLElement | null }).current = workspace;

    const { result } = renderHook(() => useWorkspaceScrolled(ref, 'albums:'));
    scrollTo(view, 1.5);
    expect(result.current).toBe(false);
  });

  it('re-reads the live scroll position when the route changes', () => {
    const { view, workspace } = mountWorkspace();
    const ref = createRef<HTMLElement>();
    (ref as { current: HTMLElement | null }).current = workspace;

    const { rerender, result } = renderHook(({ routeKey }) => useWorkspaceScrolled(ref, routeKey), {
      initialProps: { routeKey: 'albums:' }
    });
    scrollTo(view, 400);
    expect(result.current).toBe(true);

    // A new page mounts its own `.view` at the top and fires no scroll event.
    view.remove();
    const fresh = document.createElement('section');
    fresh.className = 'view songs-view';
    workspace.appendChild(fresh);
    act(() => {
      rerender({ routeKey: 'songs:' });
    });
    expect(result.current).toBe(false);
  });
});
