import { useSyncExternalStore } from 'react';
import { api } from '../../../shared/lib/api';
import type { SourceRef } from '../../../shared/types';

export type PlaybackStateName = 'Playing' | 'Paused' | 'Stopped' | string;

export interface PlaybackStatus {
  state?: PlaybackStateName;
  file_name?: string | null;
  current_source?: SourceRef | null;
  track_title?: string | null;
  track_artist?: string | null;
  track_album?: string | null;
  position_secs?: number | null;
  duration_secs?: number | null;
  active_zone_id?: string | null;
  active_zone_name?: string | null;
  selected_device?: string | null;
  exclusive?: boolean | null;
  source_rate?: number | null;
  source_bits?: number | null;
  target_rate?: number | null;
  target_bits?: number | null;
  output_mode?: string | null;
  active_output_mode?: string | null;
  transport_pending?: string | null;
  transport_pending_position_secs?: number | null;
}

export type PlaybackConnection = 'idle' | 'connecting' | 'connected' | 'disconnected';

export interface PlaybackSnapshot {
  connection: PlaybackConnection;
  status: PlaybackStatus;
  lastMessageAt: number | null;
  error: string | null;
}

type Listener = () => void;

const listeners = new Set<Listener>();

let socket: WebSocket | null = null;
let reconnectTimer = 0;
let reconnectAttempt = 0;
let started = false;
let lifecycleRefreshStarted = false;
let statusPollTimer = 0;
let socketWatchdogTimer = 0;
let lastStatusFetchStartedAt = 0;
let latestStatusRequestId = 0;
let socketMessageVersion = 0;
let socketOpenedAt = 0;
let lastSocketMessageAt = 0;

const ACTIVE_STATUS_POLL_MS = 10000;
const SOCKET_WATCHDOG_MS = 5000;
const STATUS_FETCH_THROTTLE_MS = 750;
const STALE_SOCKET_MS = 15000;

let snapshot: PlaybackSnapshot = {
  connection: 'idle',
  status: {
    state: 'Stopped',
    file_name: null,
    track_title: null,
    track_artist: null,
    track_album: null,
    position_secs: 0,
    duration_secs: 0,
    transport_pending: 'none',
    transport_pending_position_secs: null
  },
  lastMessageAt: null,
  error: null
};

function emit() {
  listeners.forEach((listener) => listener());
}

function setSnapshot(next: Partial<PlaybackSnapshot>) {
  snapshot = {
    ...snapshot,
    ...next,
    status: next.status ? { ...snapshot.status, ...next.status } : snapshot.status
  };
  emit();
}

function websocketUrl() {
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${protocol}//${window.location.host}/api/ws`;
}

export async function refreshPlaybackStatus({ force = false }: { force?: boolean } = {}) {
  const now = Date.now();
  if (!force && now - lastStatusFetchStartedAt < STATUS_FETCH_THROTTLE_MS) return;
  lastStatusFetchStartedAt = now;
  const requestId = latestStatusRequestId + 1;
  latestStatusRequestId = requestId;
  const socketVersionAtStart = socketMessageVersion;
  try {
    const status = await api.get<PlaybackStatus>('/api/status', undefined, undefined, 'no-store');
    if (requestId !== latestStatusRequestId || socketVersionAtStart !== socketMessageVersion)
      return;
    setSnapshot({ status, lastMessageAt: Date.now(), error: null });
  } catch {
    // WebSocket updates are authoritative; polling is only a resync path.
  }
}

function socketIsStale() {
  if (!socket || socket.readyState !== WebSocket.OPEN) return true;
  const lastActivity = lastSocketMessageAt || socketOpenedAt;
  return lastActivity <= 0 || Date.now() - lastActivity > STALE_SOCKET_MS;
}

function refreshPlaybackWhenActive() {
  if (document.visibilityState === 'hidden') return;
  refreshPlaybackStatus({ force: true }).catch(() => undefined);
  if (socketIsStale()) reconnectPlaybackStore();
}

function startLifecycleStatusRefresh() {
  if (lifecycleRefreshStarted) return;
  lifecycleRefreshStarted = true;

  document.addEventListener('visibilitychange', refreshPlaybackWhenActive);
  window.addEventListener('focus', refreshPlaybackWhenActive);
  window.addEventListener('online', refreshPlaybackWhenActive);
  window.clearInterval(statusPollTimer);
  statusPollTimer = window.setInterval(() => {
    if (document.visibilityState !== 'hidden') {
      refreshPlaybackStatus().catch(() => undefined);
    }
  }, ACTIVE_STATUS_POLL_MS);
  window.clearInterval(socketWatchdogTimer);
  socketWatchdogTimer = window.setInterval(() => {
    if (document.visibilityState !== 'hidden' && socketIsStale()) reconnectPlaybackStore();
  }, SOCKET_WATCHDOG_MS);
}

function scheduleReconnect() {
  window.clearTimeout(reconnectTimer);
  const delay = Math.min(10000, 1000 + reconnectAttempt * 1000);
  reconnectTimer = window.setTimeout(() => {
    reconnectAttempt += 1;
    connectPlaybackSocket();
  }, delay);
}

export function connectPlaybackSocket() {
  if (
    socket &&
    (socket.readyState === WebSocket.CONNECTING || socket.readyState === WebSocket.OPEN)
  ) {
    return;
  }

  window.clearTimeout(reconnectTimer);
  setSnapshot({ connection: 'connecting', error: null });

  const nextSocket = new WebSocket(websocketUrl());
  socket = nextSocket;

  nextSocket.addEventListener('open', () => {
    if (socket !== nextSocket) return;
    reconnectAttempt = 0;
    socketOpenedAt = Date.now();
    lastSocketMessageAt = 0;
    setSnapshot({ connection: 'connected', error: null });
  });

  nextSocket.addEventListener('message', (event) => {
    if (socket !== nextSocket) return;
    try {
      const status = JSON.parse(event.data as string) as PlaybackStatus;
      lastSocketMessageAt = Date.now();
      socketMessageVersion += 1;
      setSnapshot({ connection: 'connected', status, lastMessageAt: Date.now(), error: null });
    } catch (error) {
      setSnapshot({
        error: error instanceof Error ? error.message : 'Unable to parse playback status'
      });
    }
  });

  nextSocket.addEventListener('close', () => {
    if (socket !== nextSocket) return;
    socket = null;
    socketOpenedAt = 0;
    lastSocketMessageAt = 0;
    setSnapshot({ connection: 'disconnected' });
    scheduleReconnect();
  });

  nextSocket.addEventListener('error', () => {
    if (socket !== nextSocket) return;
    setSnapshot({ error: 'Playback WebSocket error' });
  });
}

export function startPlaybackStore() {
  if (started) return;
  started = true;
  startLifecycleStatusRefresh();
  refreshPlaybackStatus({ force: true }).catch(() => undefined);
  connectPlaybackSocket();
}

export function reconnectPlaybackStore() {
  window.clearTimeout(reconnectTimer);
  reconnectAttempt = 0;
  const previous = socket;
  socket = null;
  socketOpenedAt = 0;
  lastSocketMessageAt = 0;
  if (previous) {
    previous.close();
  }
  connectPlaybackSocket();
}

export function getPlaybackSnapshot() {
  return snapshot;
}

export function subscribePlayback(listener: Listener) {
  startPlaybackStore();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function usePlaybackSnapshot() {
  return useSyncExternalStore(subscribePlayback, getPlaybackSnapshot, getPlaybackSnapshot);
}
