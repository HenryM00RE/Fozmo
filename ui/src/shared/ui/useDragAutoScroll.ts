import { useCallback, useEffect, useRef } from 'react';

const MIN_EDGE_SIZE = 48;
const MAX_EDGE_SIZE = 96;
const MAX_SCROLL_PER_FRAME = 22;

export function dragEdgeScrollDelta(clientY: number, top: number, bottom: number) {
  const height = Math.max(0, bottom - top);
  if (!height) return 0;
  const edgeSize = Math.min(MAX_EDGE_SIZE, Math.max(MIN_EDGE_SIZE, height * 0.18));
  if (clientY < top + edgeSize) {
    const intensity = Math.min(1, Math.max(0, (top + edgeSize - clientY) / edgeSize));
    return -Math.max(1, Math.round(MAX_SCROLL_PER_FRAME * intensity));
  }
  if (clientY > bottom - edgeSize) {
    const intensity = Math.min(1, Math.max(0, (clientY - (bottom - edgeSize)) / edgeSize));
    return Math.max(1, Math.round(MAX_SCROLL_PER_FRAME * intensity));
  }
  return 0;
}

type DragPosition = {
  clientX: number;
  clientY: number;
  frame: number | null;
  scroller: HTMLElement | null;
};

/** Keeps a captured/native drag moving through the page scroller at either edge. */
export function useDragAutoScroll(onScroll?: (clientX: number, clientY: number) => void) {
  const onScrollRef = useRef(onScroll);
  onScrollRef.current = onScroll;
  const positionRef = useRef<DragPosition>({
    clientX: 0,
    clientY: 0,
    frame: null,
    scroller: null
  });
  const animateRef = useRef<() => void>(() => undefined);

  const stop = useCallback(() => {
    const position = positionRef.current;
    if (position.frame !== null) cancelAnimationFrame(position.frame);
    position.frame = null;
    position.scroller = null;
  }, []);

  animateRef.current = () => {
    const position = positionRef.current;
    const scroller = position.scroller;
    if (!scroller) {
      position.frame = null;
      return;
    }
    const rect = scroller.getBoundingClientRect();
    const delta = dragEdgeScrollDelta(position.clientY, rect.top, rect.bottom);
    if (delta) {
      const maximum = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
      const next = Math.min(maximum, Math.max(0, scroller.scrollTop + delta));
      if (next !== scroller.scrollTop) {
        scroller.scrollTop = next;
        onScrollRef.current?.(position.clientX, position.clientY);
      }
    }
    position.frame = requestAnimationFrame(animateRef.current);
  };

  const update = useCallback((source: Element, clientX: number, clientY: number) => {
    const position = positionRef.current;
    position.scroller = source.closest<HTMLElement>('.view');
    position.clientX = clientX;
    position.clientY = clientY;
    if (position.scroller && position.frame === null) {
      position.frame = requestAnimationFrame(animateRef.current);
    }
  }, []);

  useEffect(() => stop, [stop]);

  return { stop, update };
}
