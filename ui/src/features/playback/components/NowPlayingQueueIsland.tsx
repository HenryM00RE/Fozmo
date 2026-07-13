import {
  type DragEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState
} from 'react';
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

const queueReturnDelayMs = 1_000;

function scrollQueueItemIntoView(
  queue: HTMLOListElement,
  current: HTMLElement,
  behavior: ScrollBehavior
) {
  const scrollContainer = findQueueScrollContainer(queue);
  const queueRect = scrollContainer.getBoundingClientRect();
  const currentRect = current.getBoundingClientRect();
  const topInset = scrollContainer === queue ? 0 : queueHeaderOffset(scrollContainer);
  const top = scrollContainer.scrollTop + currentRect.top - queueRect.top - topInset;
  scrollContainer.scrollTo({ top: Math.max(0, top), behavior });
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
  const pointerDragRef = useRef<{ from: number; pointerId: number } | null>(null);
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);

  useEffect(() => {
    const shouldForceInitialScroll = scrollToCurrentOnMount && !hasScrolledOnMountRef.current;
    if (snapshot.cursor < 0) return;
    if (snapshot.preserveQueueScroll && !shouldForceInitialScroll) return;
    const queue = queueRef.current;
    const current = queue?.querySelector<HTMLElement>(
      `.queue-item[data-queue-index="${snapshot.cursor}"]`
    );
    if (!queue || !current) return;

    let scrollFrame = 0;
    let settleTimer = 0;
    const performScroll = () => {
      scrollQueueItemIntoView(queue, current, shouldForceInitialScroll ? 'auto' : 'smooth');
      hasScrolledOnMountRef.current = true;
    };
    const layoutFrame = window.requestAnimationFrame(() => {
      scrollFrame = window.requestAnimationFrame(performScroll);
    });
    if (shouldForceInitialScroll) settleTimer = window.setTimeout(performScroll, 220);

    return () => {
      window.cancelAnimationFrame(layoutFrame);
      if (scrollFrame) window.cancelAnimationFrame(scrollFrame);
      if (settleTimer) window.clearTimeout(settleTimer);
    };
  }, [
    scrollToCurrentOnMount,
    snapshot.cursor,
    snapshot.preserveQueueScroll,
    snapshot.structuralKey
  ]);

  useEffect(() => {
    const queue = queueRef.current;
    if (!queue) return undefined;
    const scrollContainer = findQueueScrollContainer(queue);
    const returnToCurrent = () => {
      if (returnToCurrentTimerRef.current) {
        window.clearTimeout(returnToCurrentTimerRef.current);
      }
      returnToCurrentTimerRef.current = window.setTimeout(() => {
        returnToCurrentTimerRef.current = null;
        const current = queue.querySelector<HTMLElement>('.queue-item.is-current');
        if (current) scrollQueueItemIntoView(queue, current, 'smooth');
      }, queueReturnDelayMs);
    };
    scrollContainer.addEventListener('scroll', returnToCurrent, { passive: true });
    return () => {
      scrollContainer.removeEventListener('scroll', returnToCurrent);
      if (returnToCurrentTimerRef.current) {
        window.clearTimeout(returnToCurrentTimerRef.current);
        returnToCurrentTimerRef.current = null;
      }
    };
  }, []);

  const armDrag = (event: ReactPointerEvent<HTMLButtonElement>, index: number) => {
    if (!reorderable) return;
    if (event.button !== 0) return;
    const row = event.currentTarget.closest('.queue-item');
    if (!(row instanceof HTMLElement)) return;
    if (event.pointerType === 'mouse') {
      row.draggable = true;
      return;
    }
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    pointerDragRef.current = { from: index, pointerId: event.pointerId };
    setDragFrom(index);
  };

  const disarmDrag = (event: ReactPointerEvent<HTMLLIElement>) => {
    if (pointerDragRef.current) return;
    event.currentTarget.draggable = false;
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
    setDropTarget(pointerDropTarget(event.clientX, event.clientY));
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
    setDragFrom(null);
    setDropTarget(null);
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
    const rect = event.currentTarget.getBoundingClientRect();
    const placement: DropPlacement = event.clientY - rect.top < rect.height / 2 ? 'above' : 'below';
    setDropTarget({ index, placement });
  };

  const resetDrag = () => {
    setDragFrom(null);
    setDropTarget(null);
  };

  const handleDrop = (event: DragEvent<HTMLLIElement>, index: number) => {
    event.preventDefault();
    if (dragFrom === null) {
      event.currentTarget.draggable = false;
      resetDrag();
      return;
    }
    const placement = dropTarget?.index === index ? dropTarget.placement : 'above';
    const to = placement === 'above' ? index : index + 1;
    getNowPlayingQueueActions().reorderQueue?.(dragFrom, to);
    event.currentTarget.draggable = false;
    resetDrag();
  };

  const handleRowClick = (event: MouseEvent<HTMLLIElement>, index: number) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest('.queue-item-remove, .queue-item-grip')) return;
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
    >
      {snapshot.items.map((item) => (
        <li
          className={queueItemClassName(item, dragFrom, dropTarget)}
          data-queue-index={item.index}
          data-testid="queue-row"
          draggable={false}
          key={item.key}
          onClick={(event) => handleRowClick(event, item.index)}
          onDragEnd={(event) => {
            event.currentTarget.draggable = false;
            resetDrag();
          }}
          onDragLeave={() => setDropTarget(null)}
          onDragOver={(event) => handleDragOver(event, item.index)}
          onDragStart={(event) => handleDragStart(event, item.index)}
          onDrop={(event) => handleDrop(event, item.index)}
          onPointerCancel={disarmDrag}
          onPointerUp={disarmDrag}
        >
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
          <span className="queue-item-index">{item.index + 1}</span>
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
