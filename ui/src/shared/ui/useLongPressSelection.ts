import type { MouseEventHandler, PointerEventHandler } from 'react';
import { useCallback, useEffect, useRef } from 'react';

const LONG_PRESS_DELAY_MS = 500;
const LONG_PRESS_MOVE_TOLERANCE_PX = 12;
const POST_LONG_PRESS_SUPPRESSION_MS = 900;

type LongPressSelectionOptions<T> = {
  enabled?: boolean;
  onSelect: (selection: T) => void;
  resolveSelection: (target: Element, currentTarget: HTMLElement) => T | null;
};

type PendingLongPress<T> = {
  pointerId: number;
  selection: T;
  startX: number;
  startY: number;
};

export function useLongPressSelection<T>({
  enabled = true,
  onSelect,
  resolveSelection
}: LongPressSelectionOptions<T>) {
  const onSelectRef = useRef(onSelect);
  const resolveSelectionRef = useRef(resolveSelection);
  const pendingRef = useRef<PendingLongPress<T> | null>(null);
  const timerRef = useRef<number | null>(null);
  const suppressActivationUntilRef = useRef(0);
  onSelectRef.current = onSelect;
  resolveSelectionRef.current = resolveSelection;

  const cancelPending = useCallback(() => {
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    window.removeEventListener('scroll', cancelPending, true);
    timerRef.current = null;
    pendingRef.current = null;
  }, []);

  useEffect(() => cancelPending, [cancelPending]);

  const select = useCallback((selection: T, suppressFollowingActivation: boolean) => {
    onSelectRef.current(selection);
    if (suppressFollowingActivation) {
      suppressActivationUntilRef.current = Date.now() + POST_LONG_PRESS_SUPPRESSION_MS;
    }
  }, []);

  const onPointerDown = useCallback<PointerEventHandler<HTMLElement>>(
    (event) => {
      if (!enabled || event.pointerType === 'mouse' || event.button !== 0) return;
      if (!(event.target instanceof Element)) return;
      const selection = resolveSelectionRef.current(event.target, event.currentTarget);
      if (selection === null) return;

      cancelPending();
      pendingRef.current = {
        pointerId: event.pointerId,
        selection,
        startX: event.clientX,
        startY: event.clientY
      };
      window.addEventListener('scroll', cancelPending, true);
      timerRef.current = window.setTimeout(() => {
        const pending = pendingRef.current;
        window.removeEventListener('scroll', cancelPending, true);
        timerRef.current = null;
        pendingRef.current = null;
        if (pending) select(pending.selection, true);
      }, LONG_PRESS_DELAY_MS);
    },
    [cancelPending, enabled, select]
  );

  const onPointerMove = useCallback<PointerEventHandler<HTMLElement>>(
    (event) => {
      const pending = pendingRef.current;
      if (!pending || pending.pointerId !== event.pointerId) return;
      if (
        Math.hypot(event.clientX - pending.startX, event.clientY - pending.startY) >
        LONG_PRESS_MOVE_TOLERANCE_PX
      ) {
        cancelPending();
      }
    },
    [cancelPending]
  );

  const onPointerEnd = useCallback<PointerEventHandler<HTMLElement>>(
    (event) => {
      if (pendingRef.current?.pointerId === event.pointerId) cancelPending();
    },
    [cancelPending]
  );

  const onContextMenu = useCallback<MouseEventHandler<HTMLElement>>(
    (event) => {
      if (!enabled || !(event.target instanceof Element)) return;
      const selection = resolveSelectionRef.current(event.target, event.currentTarget);
      if (selection === null) return;

      const contextMenuCompletedTouchHold = pendingRef.current !== null;
      event.preventDefault();
      event.stopPropagation();
      cancelPending();
      if (Date.now() < suppressActivationUntilRef.current) return;
      select(selection, contextMenuCompletedTouchHold);
    },
    [cancelPending, enabled, select]
  );

  const onClickCapture = useCallback<MouseEventHandler<HTMLElement>>((event) => {
    if (Date.now() >= suppressActivationUntilRef.current) return;
    event.preventDefault();
    event.stopPropagation();
  }, []);

  return {
    'data-selection-press': enabled ? '' : undefined,
    onClickCapture,
    onContextMenu,
    onLostPointerCapture: onPointerEnd,
    onPointerCancel: onPointerEnd,
    onPointerDown,
    onPointerMove,
    onPointerUp: onPointerEnd
  };
}
