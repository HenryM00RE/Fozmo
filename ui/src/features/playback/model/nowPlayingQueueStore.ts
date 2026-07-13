import { useSyncExternalStore } from 'react';
import type { LoopMode } from '../../../shared/types';

export interface NowPlayingQueueItemSnapshot {
  index: number;
  key: string;
  title: string;
  subtitle: string;
  featureHtml: string;
  durationLabel: string;
  artSrc: string | null;
  isCurrent: boolean;
  isPast: boolean;
  removable: boolean;
}

export interface NowPlayingQueueSnapshot {
  kind: string | null;
  cursor: number;
  loopMode: LoopMode;
  upcomingCount: number;
  items: NowPlayingQueueItemSnapshot[];
  structuralKey: string;
  preserveQueueScroll: boolean;
  updatedAt: number;
}

export interface NowPlayingQueueActions {
  jumpToIndex?: (index: number) => void;
  removeIndex?: (index: number) => void;
  reorderQueue?: (from: number, to: number) => void;
  requestSnapshot?: () => void;
}

type Listener = () => void;

const listeners = new Set<Listener>();

let snapshot: NowPlayingQueueSnapshot = {
  kind: null,
  cursor: -1,
  loopMode: 'off',
  upcomingCount: 0,
  items: [],
  structuralKey: '',
  preserveQueueScroll: false,
  updatedAt: 0
};

let actions: NowPlayingQueueActions = {};

function emit() {
  listeners.forEach((listener) => listener());
}

export function updateNowPlayingQueue(next: Omit<NowPlayingQueueSnapshot, 'updatedAt'>) {
  snapshot = {
    ...next,
    updatedAt: Date.now()
  };
  emit();
}

export function setNowPlayingQueueActions(nextActions: NowPlayingQueueActions) {
  actions = nextActions || {};
}

export function getNowPlayingQueueSnapshot() {
  return snapshot;
}

export function getNowPlayingQueueActions() {
  return actions;
}

export function requestNowPlayingQueueSnapshot() {
  actions.requestSnapshot?.();
}

export function subscribeNowPlayingQueue(listener: Listener) {
  listeners.add(listener);
  actions.requestSnapshot?.();
  return () => {
    listeners.delete(listener);
  };
}

export function useNowPlayingQueueSnapshot() {
  return useSyncExternalStore(
    subscribeNowPlayingQueue,
    getNowPlayingQueueSnapshot,
    getNowPlayingQueueSnapshot
  );
}
