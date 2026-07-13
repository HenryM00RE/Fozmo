import { useSyncExternalStore } from 'react';

export type PlaybackControlAction = () => void | Promise<unknown>;

export interface PlaybackControlActions {
  next?: PlaybackControlAction;
  playPause?: PlaybackControlAction;
  previous?: PlaybackControlAction;
  seek?: (seconds: number) => void;
  shuffle?: PlaybackControlAction;
  stop?: PlaybackControlAction;
  toggleLoop?: PlaybackControlAction;
  toggleSignalPath?: () => void;
}

export interface PlaybackControlSnapshot {
  playbackLoading: boolean;
  pendingPlaybackIntent: PendingPlaybackIntentSnapshot | null;
  pendingArtSrc: string | null;
  transportPending: TransportPendingSnapshot | null;
}

export interface PendingPlaybackIntentSnapshot {
  artist: string;
  fileName: string;
  title: string;
}

export type TransportPendingKind = 'play' | 'next' | 'previous' | 'seek' | 'auto-advance';

export interface TransportPendingSnapshot {
  kind: TransportPendingKind;
  requestedAt: number;
  expectedPosition?: number | null;
  expectedTrackKey?: string | null;
}

export function isSettledPlaybackState(state: unknown) {
  return ['Playing', 'Paused', 'Stopped'].includes(String(state));
}

export function backendPlayControlIsLoading(state: unknown, pending: unknown) {
  if (state === 'Starting' || state === 'Transitioning') return true;
  if (isSettledPlaybackState(state)) return false;
  return Boolean(pending && pending !== 'none' && pending !== 'seeking');
}

let actions: PlaybackControlActions = {};
let explicitPlaybackLoading = false;
let snapshot: PlaybackControlSnapshot = {
  playbackLoading: false,
  pendingPlaybackIntent: null,
  pendingArtSrc: null,
  transportPending: null
};
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((listener) => listener());
}

export function setPlaybackControlActions(nextActions: PlaybackControlActions) {
  actions = nextActions || {};
}

export function getPlaybackControlActions() {
  return actions;
}

function effectivePlaybackLoading(transportPending = snapshot.transportPending) {
  return explicitPlaybackLoading || Boolean(transportPending);
}

export function setPlaybackLoading(playbackLoading: boolean) {
  explicitPlaybackLoading = playbackLoading;
  const nextLoading = effectivePlaybackLoading();
  if (snapshot.playbackLoading === nextLoading) return;
  snapshot = { ...snapshot, playbackLoading: nextLoading };
  emit();
}

export function setTransportPending(transportPending: TransportPendingSnapshot | null) {
  const nextLoading = effectivePlaybackLoading(transportPending);
  if (
    snapshot.transportPending?.kind === transportPending?.kind &&
    snapshot.transportPending?.requestedAt === transportPending?.requestedAt &&
    snapshot.transportPending?.expectedPosition === transportPending?.expectedPosition &&
    snapshot.transportPending?.expectedTrackKey === transportPending?.expectedTrackKey &&
    snapshot.playbackLoading === nextLoading
  ) {
    return;
  }
  snapshot = { ...snapshot, transportPending, playbackLoading: nextLoading };
  emit();
}

export function clearTransportPending(kind?: TransportPendingKind) {
  if (kind && snapshot.transportPending?.kind !== kind) return;
  setTransportPending(null);
}

export function setPendingPlaybackIntent(
  pendingPlaybackIntent: PendingPlaybackIntentSnapshot | null
) {
  if (
    snapshot.pendingPlaybackIntent?.fileName === pendingPlaybackIntent?.fileName &&
    snapshot.pendingPlaybackIntent?.title === pendingPlaybackIntent?.title &&
    snapshot.pendingPlaybackIntent?.artist === pendingPlaybackIntent?.artist
  )
    return;
  snapshot = { ...snapshot, pendingPlaybackIntent };
  emit();
}

export function setPendingPlaybackArt(pendingArtSrc: string | null) {
  if (snapshot.pendingArtSrc === pendingArtSrc) return;
  snapshot = { ...snapshot, pendingArtSrc };
  emit();
}

function getPlaybackControlSnapshot() {
  return snapshot;
}

function subscribePlaybackControl(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function usePlaybackControlSnapshot() {
  return useSyncExternalStore(
    subscribePlaybackControl,
    getPlaybackControlSnapshot,
    getPlaybackControlSnapshot
  );
}
