import {
  type DragEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useLayoutEffect,
  useRef,
  useState
} from 'react';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import {
  getNowPlayingQueueActions,
  type NowPlayingQueueItemSnapshot,
  useNowPlayingQueueSnapshot
} from '../model/nowPlayingQueueStore';

type DropPlacement = 'above' | 'below';

interface DropTarget {
  index: number;
  placement: DropPlacement;
}

interface NowPlayingQueueIslandProps {
  reorderable?: boolean;
  scrollToCurrentOnMount?: boolean;
}

const queueReturnDelayMs = 6_000;
const queueScrollSettleDelayMs = 180;
const queueDragEdgeSizePx = 72;
const queueDragMaxScrollPx = 18;

function scrollQueueItemIntoView(
  queue: HTMLOListElement,
  current: HTMLElement,
  behavior: ScrollBehavior
) {
  const { scrollContainer, top } = queueItemScrollTarget(queue, current);
  scrollContainer.scrollTo({ top, behavior });
}

function queueItemScrollTarget(queue: HTMLOListElement, current: HTMLElement) {
  const scrollContainer = findQueueScrollContainer(queue);
  const queueRect = scrollContainer.getBoundingClientRect();
  const currentRect = current.getBoundingClientRect();
  const topInset = scrollContainer === queue ? 0 : queueHeaderOffset(scrollContainer);
  const top = scrollContainer.scrollTop + currentRect.top - queueRect.top - topInset;
  return { scrollContainer, top: Math.max(0, top) };
}

function findQueueScrollContainer(queue: HTMLOListElement) {
  if (isVerticallyScrollable(queue)) return queue;

  let parent = queue.parentElement;
  while (parent && parent !== document.body) {
    if (isVerticallyScrollable(parent)) return parent;
    parent = parent.parentElement;
  }

  return queue;
}

function isVerticallyScrollable(element: HTMLElement) {
  const { overflowY } = window.getComputedStyle(element);
  if (!/(auto|scroll|overlay)/.test(overflowY)) return false;
  return element.scrollHeight > element.clientHeight + 1;
}

function queueHeaderOffset(scrollContainer: HTMLElement) {
  const header = Array.from(scrollContainer.children).find((child) =>
    child.classList.contains('now-playing-queue-header')
  );
  return header instanceof HTMLElement ? header.offsetHeight + 8 : 0;
}

function queueItemClassName(
  item: NowPlayingQueueItemSnapshot,
  dragFrom: number | null,
  dropTarget: DropTarget | null
) {
  const classes = ['queue-item'];
  if (item.isCurrent) classes.push('is-current');
  if (item.isPast) classes.push('is-past');
  if (item.removable) classes.push('is-removable');
  if (dragFrom === item.index) classes.push('is-dragging');
  if (dropTarget?.index === item.index)
    classes.push(dropTarget.placement === 'above' ? 'drop-above' : 'drop-below');
  return classes.join(' ');
}

function QueueArt({ item }: { item: NowPlayingQueueItemSnapshot }) {
  if (item.artSrc) {
    return <img alt="" src={item.artSrc} loading="lazy" />;
  }

  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="2" />
    </svg>
  );
}

export function NowPlayingQueueIsland({
  reorderable = true,
  scrollToCurrentOnMount = false
}: NowPlayingQueueIslandProps) {
  const snapshot = useNowPlayingQueueSnapshot();
  const queueRef = useRef<HTMLOListElement | null>(null);
  const hasScrolledOnMountRef = useRef(false);
  const returnToCurrentTimerRef = useRef<number | null>(null);
  const programmaticScrollRef = useRef(false);
  const programmaticScrollSettleTimerRef = useRef<number | null>(null);
  const settleProgrammaticScrollRef = useRef<() => void>(() => undefined);
  const programmaticScrollAnimationFrameRef = useRef<number | null>(null);
  const dragActiveRef = useRef(false);
  const dragAutoScrollFrameRef = useRef<number | null>(null);
  const dragAutoScrollSpeedRef = useRef(0);
  const scheduleReturnToCurrentRef = useRef<() => void>(() => undefined);
  const pointerDragRef = useRef<{ from: number; pointerId: number } | null>(null);
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);

  const animateQueueScrollTo = (scrollContainer: HTMLElement, top: number) => {
    if (programmaticScrollAnimationFrameRef.current) {
      window.cancelAnimationFrame(programmaticScrollAnimationFrameRef.current);
      programmaticScrollAnimationFrameRef.current = null;
    }
    const startTop = scrollContainer.scrollTop;
    const distance = top - startTop;
    if (Math.abs(distance) <= 1) {
      scrollContainer.scrollTop = top;
      settleProgrammaticScrollRef.current();
      return;
    }
    const duration = Math.min(520, Math.max(220, Math.abs(distance) * 0.32));
    let startedAt: number | null = null;
    const step = (timestamp: number) => {
      startedAt ??= timestamp;
      const progress = Math.min(1, (timestamp - startedAt) / duration);
      const eased = 1 - (1 - progress) ** 3;
      scrollContainer.scrollTop = startTop + distance * eased;
      if (progress < 1) {
        programmaticScrollAnimationFrameRef.current = window.requestAnimationFrame(step);
        return;
      }
      scrollContainer.scrollTop = top;
      programmaticScrollAnimationFrameRef.current = null;
      settleProgrammaticScrollRef.current();
    };
    programmaticScrollAnimationFrameRef.current = window.requestAnimationFrame(step);
  };

  const animateQueueItemIntoView = (queue: HTMLOListElement, current: HTMLElement) => {
    programmaticScrollRef.current = true;
    const target = queueItemScrollTarget(queue, current);
    animateQueueScrollTo(target.scrollContainer, target.top);
  };

  useLayoutEffect(() => {
    const shouldForceInitialScroll = scrollToCurrentOnMount && !hasScrolledOnMountRef.current;
    if (!shouldForceInitialScroll) return;
    if (snapshot.cursor < 0) return;
    const queue = queueRef.current;
    const current = queue?.querySelector<HTMLElement>(
      `.queue-item[data-queue-index="${snapshot.cursor}"]`
    );
    if (!queue || !current) return;

    programmaticScrollRef.current = true;
    scrollQueueItemIntoView(queue, current, 'auto');
    programmaticScrollRef.current = false;
    hasScrolledOnMountRef.current = true;
  }, [
    scrollToCurrentOnMount,
    snapshot.cursor,
    snapshot.structuralKey
  ]);

  useEffect(() => {
    const queue = queueRef.current;
    if (!queue) return undefined;
    const scrollContainer = findQueueScrollContainer(queue);
    const settleProgrammaticScroll = () => {
      if (programmaticScrollSettleTimerRef.current) {
        window.clearTimeout(programmaticScrollSettleTimerRef.current);
      }
      programmaticScrollSettleTimerRef.current = window.setTimeout(() => {
        programmaticScrollSettleTimerRef.current = null;
        const current = queue.querySelector<HTMLElement>('.queue-item.is-current');
        if (current) {
          const target = queueItemScrollTarget(queue, current);
          if (Math.abs(target.scrollContainer.scrollTop - target.top) > 1) {
            animateQueueScrollTo(target.scrollContainer, target.top);
            return;
          }
        }
        programmaticScrollRef.current = false;
      }, queueScrollSettleDelayMs);
    };
    settleProgrammaticScrollRef.current = settleProgrammaticScroll;
    const returnToCurrent = () => {
      if (dragActiveRef.current) {
        if (returnToCurrentTimerRef.current) {
          window.clearTimeout(returnToCurrentTimerRef.current);
          returnToCurrentTimerRef.current = null;
        }
        return;
      }
      if (programmaticScrollRef.current) {
        settleProgrammaticScroll();
        return;
      }
      if (returnToCurrentTimerRef.current) {
        window.clearTimeout(returnToCurrentTimerRef.current);
      }
      returnToCurrentTimerRef.current = window.setTimeout(() => {
        returnToCurrentTimerRef.current = null;
        if (dragActiveRef.current) return;
        const current = queue.querySelector<HTMLElement>('.queue-item.is-current');
        if (current) {
          if (programmaticScrollSettleTimerRef.current) {
            window.clearTimeout(programmaticScrollSettleTimerRef.current);
          }
          animateQueueItemIntoView(queue, current);
        }
      }, queueReturnDelayMs);
    };
    scheduleReturnToCurrentRef.current = returnToCurrent;
    scrollContainer.addEventListener('scroll', returnToCurrent, { passive: true });
    return () => {
      settleProgrammaticScrollRef.current = () => undefined;
      scheduleReturnToCurrentRef.current = () => undefined;
      scrollContainer.removeEventListener('scroll', returnToCurrent);
      if (returnToCurrentTimerRef.current) {
        window.clearTimeout(returnToCurrentTimerRef.current);
        returnToCurrentTimerRef.current = null;
      }
      if (programmaticScrollSettleTimerRef.current) {
        window.clearTimeout(programmaticScrollSettleTimerRef.current);
        programmaticScrollSettleTimerRef.current = null;
      }
      programmaticScrollRef.current = false;
      if (programmaticScrollAnimationFrameRef.current) {
        window.cancelAnimationFrame(programmaticScrollAnimationFrameRef.current);
        programmaticScrollAnimationFrameRef.current = null;
      }
      if (dragAutoScrollFrameRef.current) {
        window.cancelAnimationFrame(dragAutoScrollFrameRef.current);
        dragAutoScrollFrameRef.current = null;
      }
    };
  }, []);

  const stopDragAutoScroll = () => {
    dragAutoScrollSpeedRef.current = 0;
    if (!dragAutoScrollFrameRef.current) return;
    window.cancelAnimationFrame(dragAutoScrollFrameRef.current);
    dragAutoScrollFrameRef.current = null;
  };

  const updateDragAutoScroll = (clientY: number) => {
    const queue = queueRef.current;
    if (!queue || !dragActiveRef.current) return;
    const scrollContainer = findQueueScrollContainer(queue);
    const rect = scrollContainer.getBoundingClientRect();
    const edgeSize = Math.min(queueDragEdgeSizePx, rect.height / 4);
    let speed = 0;
    if (clientY < rect.top + edgeSize) {
      const strength = Math.min(1, (rect.top + edgeSize - clientY) / edgeSize);
      speed = -queueDragMaxScrollPx * strength;
    } else if (clientY > rect.bottom - edgeSize) {
      const strength = Math.min(1, (clientY - (rect.bottom - edgeSize)) / edgeSize);
      speed = queueDragMaxScrollPx * strength;
    }
    dragAutoScrollSpeedRef.current = speed;
    if (!speed) {
      stopDragAutoScroll();
      return;
    }
    if (dragAutoScrollFrameRef.current) return;
    const step = () => {
      if (!dragActiveRef.current || !dragAutoScrollSpeedRef.current) {
        dragAutoScrollFrameRef.current = null;
        return;
      }
      scrollContainer.scrollTop += dragAutoScrollSpeedRef.current;
      dragAutoScrollFrameRef.current = window.requestAnimationFrame(step);
    };
    dragAutoScrollFrameRef.current = window.requestAnimationFrame(step);
  };

  const beginDrag = () => {
    dragActiveRef.current = true;
    if (programmaticScrollAnimationFrameRef.current) {
      window.cancelAnimationFrame(programmaticScrollAnimationFrameRef.current);
      programmaticScrollAnimationFrameRef.current = null;
    }
    programmaticScrollRef.current = false;
    if (programmaticScrollSettleTimerRef.current) {
      window.clearTimeout(programmaticScrollSettleTimerRef.current);
      programmaticScrollSettleTimerRef.current = null;
    }
    if (returnToCurrentTimerRef.current) {
      window.clearTimeout(returnToCurrentTimerRef.current);
      returnToCurrentTimerRef.current = null;
    }
  };

  const armDrag = (event: ReactPointerEvent<HTMLButtonElement>, index: number) => {
    if (!reorderable) return;
    if (event.button !== 0) return;
    if (event.pointerType === 'mouse') return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    beginDrag();
    pointerDragRef.current = { from: index, pointerId: event.pointerId };
    setDragFrom(index);
  };

  const pointerDropTarget = (clientX: number, clientY: number): DropTarget | null => {
    const row = document.elementFromPoint(clientX, clientY)?.closest<HTMLElement>('.queue-item');
    const index = Number(row?.dataset.queueIndex);
    if (!row || !Number.isInteger(index)) return null;
    const rect = row.getBoundingClientRect();
    return {
      index,
      placement: clientY - rect.top < rect.height / 2 ? 'above' : 'below'
    };
  };

  const handlePointerMove = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const drag = pointerDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    event.preventDefault();
    updateDragAutoScroll(event.clientY);
    const target = pointerDropTarget(event.clientX, event.clientY);
    if (target) setDropTarget(target);
  };

  const finishPointerDrag = (event: ReactPointerEvent<HTMLButtonElement>, cancelled = false) => {
    const drag = pointerDragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    const target = pointerDropTarget(event.clientX, event.clientY) ?? dropTarget;
    if (!cancelled && target) {
      const to = target.placement === 'above' ? target.index : target.index + 1;
      getNowPlayingQueueActions().reorderQueue?.(drag.from, to);
    }
    pointerDragRef.current = null;
    dragActiveRef.current = false;
    stopDragAutoScroll();
    setDragFrom(null);
    setDropTarget(null);
    scheduleReturnToCurrentRef.current();
  };

  const handleDragStart = (event: DragEvent<HTMLLIElement>, index: number) => {
    if (!reorderable) {
      event.preventDefault();
      return;
    }
    if (!event.currentTarget.draggable) {
      event.preventDefault();
      return;
    }
    beginDrag();
    setDragFrom(index);
    event.dataTransfer.effectAllowed = 'move';
    try {
      event.dataTransfer.setData('text/plain', String(index));
    } catch {
      // Safari can reject drag data in some local contexts; the state above is enough.
    }
  };

  const handleDragOver = (event: DragEvent<HTMLLIElement>, index: number) => {
    if (!reorderable) return;
    if (dragFrom === null) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'move';
    updateDragAutoScroll(event.clientY);
    const rect = event.currentTarget.getBoundingClientRect();
    const placement: DropPlacement = event.clientY - rect.top < rect.height / 2 ? 'above' : 'below';
    setDropTarget({ index, placement });
  };

  const resetDrag = () => {
    dragActiveRef.current = false;
    pointerDragRef.current = null;
    stopDragAutoScroll();
    setDragFrom(null);
    setDropTarget(null);
    scheduleReturnToCurrentRef.current();
  };

  const handleQueueDragOver = (event: DragEvent<HTMLOListElement>) => {
    if (!dragActiveRef.current) return;
    event.preventDefault();
    updateDragAutoScroll(event.clientY);
  };

  const handleDrop = (event: DragEvent<HTMLLIElement>, index: number) => {
    event.preventDefault();
    if (dragFrom === null) {
      resetDrag();
      return;
    }
    const placement = dropTarget?.index === index ? dropTarget.placement : 'above';
    const to = placement === 'above' ? index : index + 1;
    getNowPlayingQueueActions().reorderQueue?.(dragFrom, to);
    resetDrag();
  };

  const handleRowClick = (event: MouseEvent<HTMLLIElement>, index: number) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest('.queue-item-play, .queue-item-remove, .queue-item-grip')) return;
    getNowPlayingQueueActions().jumpToIndex?.(index);
  };

  if (snapshot.items.length === 0) {
    return (
      <ol
        className="now-playing-queue react-now-playing-queue"
        data-testid="queue-list"
        ref={queueRef}
      >
        <li className="now-playing-empty">Queue is empty. Start a track to see it here.</li>
      </ol>
    );
  }

  return (
    <ol
      className="now-playing-queue react-now-playing-queue"
      data-testid="queue-list"
      ref={queueRef}
      onDragLeave={(event) => {
        const nextTarget = event.relatedTarget;
        if (!(nextTarget instanceof Node) || !event.currentTarget.contains(nextTarget)) {
          stopDragAutoScroll();
        }
      }}
      onDragOver={handleQueueDragOver}
    >
      {snapshot.items.map((item) => (
        <li
          className={queueItemClassName(item, dragFrom, dropTarget)}
          data-queue-index={item.index}
          data-testid="queue-row"
          draggable={reorderable && item.removable}
          key={item.key}
          onClick={(event) => handleRowClick(event, item.index)}
          onDragEnd={resetDrag}
          onDragOver={(event) => handleDragOver(event, item.index)}
          onDragStart={(event) => handleDragStart(event, item.index)}
          onDrop={(event) => handleDrop(event, item.index)}
        >
          <span className="track-row-hover-surface" aria-hidden="true" />
          <button
            className="queue-item-grip"
            data-testid="queue-grip"
            type="button"
            title={
              !reorderable
                ? 'Queue order is fixed in this view'
                : item.removable
                  ? 'Drag to reorder'
                  : 'Current and past tracks stay fixed'
            }
            aria-label={
              !reorderable
                ? 'Queue order is fixed in this view'
                : item.removable
                  ? 'Drag to reorder'
                  : 'Current and past tracks stay fixed'
            }
            disabled={!reorderable || !item.removable}
            onPointerCancel={(event) => finishPointerDrag(event, true)}
            onPointerDown={(event) => armDrag(event, item.index)}
            onPointerMove={handlePointerMove}
            onPointerUp={finishPointerDrag}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="9" cy="6" r="1.4" />
              <circle cx="9" cy="12" r="1.4" />
              <circle cx="9" cy="18" r="1.4" />
              <circle cx="15" cy="6" r="1.4" />
              <circle cx="15" cy="12" r="1.4" />
              <circle cx="15" cy="18" r="1.4" />
            </svg>
          </button>
          <span className="queue-item-index-control">
            <span className="queue-item-index">{item.index + 1}</span>
            <button
              className="queue-item-play"
              type="button"
              title="Play"
              aria-label={`Play ${item.title}`}
              onClick={(event) => {
                event.stopPropagation();
                getNowPlayingQueueActions().jumpToIndex?.(item.index);
              }}
            >
              <PlaybarPlayIcon className="queue-item-play-icon" />
            </button>
          </span>
          <div className="queue-item-art">
            <QueueArt item={item} />
          </div>
          <div className="queue-item-text">
            <span className="queue-item-title">{item.title}</span>
            {item.featureHtml ? (
              <span dangerouslySetInnerHTML={{ __html: item.featureHtml }} />
            ) : null}
            <span className="queue-item-subtitle">{item.subtitle}</span>
          </div>
          <span className="queue-item-duration">{item.durationLabel}</span>
          <button
            className="queue-item-remove"
            data-testid="queue-remove"
            type="button"
            title={item.removable ? 'Remove from queue' : 'Currently playing'}
            aria-label={item.removable ? 'Remove from queue' : 'Currently playing'}
            disabled={!item.removable}
            onClick={(event) => {
              event.stopPropagation();
              if (item.removable) getNowPlayingQueueActions().removeIndex?.(item.index);
            }}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </li>
      ))}
    </ol>
  );
}
