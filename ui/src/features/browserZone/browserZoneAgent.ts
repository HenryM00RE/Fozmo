import { browserZoneAgentId } from '../../shared/lib/browserZone';
import { sourceRefToQueueItem } from '../../shared/lib/queue';
import type { SourceRef } from '../../shared/types';
import {
  type BrowserStreamPrefs,
  browserStreamSelectionForItem,
  defaultAudioFormatProbe
} from './browserPlaybackSupport';

/**
 * The in-page playback agent behind this browser's private zone.
 *
 * On app load, this module registers over the agent WebSocket as a
 * disabled-by-default remote-agent zone (`browser: true`) and renders the
 * audio the core routes to it once the normal zone flow enables it. The zone
 * is then driven end-to-end by the normal zone APIs — queue, transport and
 * volume arrive as `CoreToAgentCommand`s — so the standard now-playing UI
 * works unchanged. Playback deliberately uses a bare `<audio>` element with
 * no WebAudio graph: on iOS, audio routed through an AudioContext stops when
 * the page is backgrounded or the screen locks, so all DSP (EQ) is applied
 * server-side in the stream itself.
 */

const PLAYING_STATUS_REPORT_INTERVAL_MS = 1_000;
const PAUSED_STATUS_REPORT_INTERVAL_MS = 2_500;
const IDLE_STATUS_REPORT_INTERVAL_MS = 5_000;
const HIDDEN_IDLE_STATUS_REPORT_INTERVAL_MS = 15_000;
const SERVER_ACTIVITY_STALE_MS = 15_000;
const RECONNECT_DELAYS_MS = [1_000, 2_000, 5_000, 10_000, 30_000];
const MAX_CONSECUTIVE_STREAM_ERRORS = 3;
// A healthy socket drains each ~600-byte status report immediately, so a
// send buffer that keeps growing means the TCP connection silently died
// (iOS background/network switch) and `close` may never fire on its own.
const STUCK_SOCKET_BUFFERED_BYTES = 16_384;

type ConnectionState = 'idle' | 'connecting' | 'connected' | 'error';
type PlayState = 'Stopped' | 'Starting' | 'Playing' | 'Paused';

interface EqBandConfig {
  enabled: boolean;
  type: string;
  freq_hz: number;
  gain_db: number;
  q: number;
}

interface EqConfigPayload {
  enabled: boolean;
  preamp_db: number;
  bands: EqBandConfig[];
}

interface PlaybackConfigPayload {
  volume?: number;
  eq?: EqConfigPayload;
}

interface CoreCommand {
  type: string;
  source_ref?: SourceRef;
  queue?: SourceRef[];
  playback_config?: PlaybackConfigPayload;
  seconds?: number;
  repeat_one?: boolean;
}

export interface BrowserZoneSnapshot {
  connection: ConnectionState;
  /** Zone id (equals the agent id) once registered. */
  zoneId: string;
  zoneName: string;
  notice: string | null;
  playback: {
    state: PlayState;
    currentSource: SourceRef | null;
    fileName: string | null;
    trackTitle: string | null;
    trackArtist: string | null;
    trackAlbum: string | null;
    positionSecs: number;
    durationSecs: number;
    volume: number;
  };
}

export const BROWSER_ZONE_REGISTERED_EVENT = 'fozmo:browser-zone-registered';

function defaultZoneName() {
  if (typeof navigator === 'undefined') return 'Browser';
  const ua = navigator.userAgent;
  const ipadDesktopMode = /Macintosh/.test(ua) && navigator.maxTouchPoints > 1;
  const browser = /Edg\//.test(ua)
    ? 'Edge'
    : /OPR\//.test(ua)
      ? 'Opera'
      : /Firefox\//.test(ua)
        ? 'Firefox'
        : /Chrome\//.test(ua)
          ? 'Chrome'
          : /Safari\//.test(ua)
            ? 'Safari'
            : 'Browser';
  const device = /iPhone|iPod/.test(ua)
    ? 'iPhone'
    : /iPad/.test(ua) || ipadDesktopMode
      ? 'iPad'
      : /Android/.test(ua)
        ? 'Android'
        : /Macintosh/.test(ua)
          ? 'Mac'
          : /Windows/.test(ua)
            ? 'Windows'
            : /Linux/.test(ua)
              ? 'Linux'
              : 'device';
  return `${browser} on ${device}`;
}

let started = false;
let connection: ConnectionState = 'idle';
let notice: string | null = null;
let ws: WebSocket | null = null;
let reconnectAttempt = 0;
let reconnectTimer: number | null = null;
let reportTimer: number | null = null;
let lastServerActivityAt = 0;

let audio: HTMLAudioElement | null = null;
let pendingGesturePlay = false;

let playState: PlayState = 'Stopped';
let currentSource: SourceRef | null = null;
let queue: SourceRef[] = [];
let repeatOne = false;
let volume = 1;
let eqActive = false;
let eqSignature: string | null = null;
let consecutiveStreamErrors = 0;
let prefetchedKey: string | null = null;

const listeners = new Set<() => void>();
let snapshot = buildSnapshot();

function buildSnapshot(): BrowserZoneSnapshot {
  const position = audio && Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
  const elementDuration = audio && Number.isFinite(audio.duration) ? audio.duration : 0;
  const duration = elementDuration || Number(currentSource?.duration_secs) || 0;
  return {
    connection,
    zoneId: browserZoneAgentId(),
    zoneName: defaultZoneName(),
    notice,
    playback: {
      state: playState,
      currentSource,
      fileName: currentSource
        ? String(currentSource.file_name || currentSource.title || '') || null
        : null,
      trackTitle: currentSource?.title ?? null,
      trackArtist: currentSource?.artist ?? null,
      trackAlbum: currentSource?.album ?? null,
      positionSecs: position,
      durationSecs: duration,
      volume
    }
  };
}

function emit() {
  snapshot = buildSnapshot();
  listeners.forEach((listener) => listener());
}

export function subscribeBrowserZone(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function getBrowserZoneSnapshot(): BrowserZoneSnapshot {
  return snapshot;
}

let streamPrefs: BrowserStreamPrefs | null = null;

/** The per-device FLAC/Opus choice from the zone's output settings. */
export function setBrowserZoneStreamPrefs(prefs: BrowserStreamPrefs | null) {
  streamPrefs = prefs;
}

/** Starts this browser's private playback agent. Safe to call once at boot. */
export function initBrowserZoneAgent() {
  if (started || typeof window === 'undefined') return;
  started = true;
  window.addEventListener('online', () => {
    if (!ws) connect();
    else if (serverActivityIsStale()) recycleSocket(ws);
  });
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') return;
    if (!ws) {
      connect();
      return;
    }
    // Returning to the foreground after a background stretch is the moment a
    // half-dead socket surfaces: reports queued while suspended never
    // drained. Recycle it so the server sees fresh status immediately.
    if (socketLooksDead(ws) || serverActivityIsStale()) {
      recycleSocket(ws);
      return;
    }
    reportNow();
    emit();
  });
  connect();
  emit();
}

function agentWebSocketUrl() {
  const scheme = window.location.protocol === 'https:' ? 'wss' : 'ws';
  return `${scheme}://${window.location.host}/api/agent/browser/ws`;
}

function connect() {
  if (ws) return;
  clearReconnectTimer();
  connection = 'connecting';
  emit();
  let socket: WebSocket;
  try {
    socket = new WebSocket(agentWebSocketUrl());
  } catch {
    connection = 'error';
    scheduleReconnect();
    emit();
    return;
  }
  ws = socket;
  socket.onopen = () => {
    if (ws !== socket) return;
    reconnectAttempt = 0;
    connection = 'connected';
    lastServerActivityAt = Date.now();
    socket.send(
      JSON.stringify({
        type: 'register',
        agent_id: browserZoneAgentId(),
        name: defaultZoneName(),
        capabilities: {
          output_devices: [],
          output_device_capabilities: [],
          max_sample_rate: 48_000,
          max_bit_depth: 24,
          exclusive_supported: false,
          supports_dsd128: false,
          supports_dsd256: false,
          browser: true
        }
      })
    );
    startReporting();
    reportNow();
    window.dispatchEvent(new Event(BROWSER_ZONE_REGISTERED_EVENT));
    emit();
  };
  socket.onmessage = (event) => {
    if (ws !== socket || typeof event.data !== 'string') return;
    lastServerActivityAt = Date.now();
    try {
      handleCommand(JSON.parse(event.data) as CoreCommand);
    } catch {
      // Ignore malformed frames.
    }
  };
  socket.onclose = () => {
    if (ws !== socket) return;
    ws = null;
    stopReporting();
    connection = 'error';
    scheduleReconnect();
    emit();
  };
  socket.onerror = () => {
    socket.close();
  };
}

function scheduleReconnect() {
  if (reconnectTimer !== null) return;
  const delay =
    RECONNECT_DELAYS_MS[Math.min(reconnectAttempt, RECONNECT_DELAYS_MS.length - 1)] ?? 30_000;
  reconnectAttempt += 1;
  reconnectTimer = window.setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, delay);
}

function clearReconnectTimer() {
  if (reconnectTimer !== null) {
    window.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

function handleCommand(cmd: CoreCommand) {
  switch (cmd.type) {
    case 'play_source':
      if (cmd.playback_config) applyPlaybackConfig(cmd.playback_config);
      queue = Array.isArray(cmd.queue) ? cmd.queue.slice() : [];
      consecutiveStreamErrors = 0;
      if (cmd.source_ref) playSource(cmd.source_ref);
      break;
    case 'pre_fetch':
      if (cmd.source_ref) prefetchSource(cmd.source_ref);
      break;
    case 'pause':
      audio?.pause();
      break;
    case 'resume':
      resumePlayback();
      break;
    case 'stop':
      stopPlayback();
      reportNow();
      break;
    case 'next':
      advanceQueue(true);
      break;
    case 'seek':
      if (audio && Number.isFinite(cmd.seconds)) {
        audio.currentTime = Math.max(0, Number(cmd.seconds));
        reportNow();
      }
      break;
    case 'set_queue':
      queue = Array.isArray(cmd.queue) ? cmd.queue.slice() : [];
      break;
    case 'set_loop_mode':
      repeatOne = Boolean(cmd.repeat_one);
      break;
    case 'set_playback_config':
      if (cmd.playback_config) applyPlaybackConfig(cmd.playback_config);
      break;
    case 'heartbeat':
      break;
    default:
      break;
  }
}

function streamSelectionForSource(source: SourceRef) {
  const item = sourceRefToQueueItem(source);
  if (!item) return null;
  return browserStreamSelectionForItem(item, defaultAudioFormatProbe(), {
    zoneId: browserZoneAgentId(),
    streamPrefs,
    eqActive,
    eqSignature
  });
}

function playSource(source: SourceRef) {
  const selection = streamSelectionForSource(source);
  currentSource = source;
  if (prefetchedKey === sourceKey(source)) prefetchedKey = null;
  if (!selection) {
    notice = 'This track cannot be streamed to a browser.';
    handleStreamFailure();
    return;
  }
  const element = ensureAudio();
  if (!element) {
    notice = 'Browser playback is not available in this environment.';
    playState = 'Stopped';
    reportNow();
    emit();
    return;
  }
  notice = null;
  playState = 'Starting';
  element.src = selection.url;
  startElementPlayback(element);
  updateMediaSession(source);
  reportNow();
  emit();
}

function startElementPlayback(element: HTMLAudioElement) {
  const result = element.play();
  if (result && typeof result.catch === 'function') {
    result.catch((error: unknown) => {
      // Replacing the stream for a live EQ change intentionally aborts the
      // old media element. Its pending play promise must not be allowed to
      // turn that expected abort into a queue advance.
      if (audio !== element) return;
      if (isNotAllowedError(error)) {
        // Autoplay blocked: wait for any user gesture in this tab, then retry.
        pendingGesturePlay = true;
        notice = 'Tap anywhere in this tab to allow audio playback.';
        playState = 'Paused';
        installGestureRetry();
      } else {
        handleStreamFailure();
      }
      reportNow();
      emit();
    });
  }
}

function isNotAllowedError(error: unknown) {
  return error instanceof DOMException && error.name === 'NotAllowedError';
}

function installGestureRetry() {
  const retry = () => {
    if (!pendingGesturePlay || !audio) return;
    pendingGesturePlay = false;
    notice = null;
    startElementPlayback(audio);
    emit();
  };
  window.addEventListener('pointerdown', retry, { once: true });
}

function resumePlayback() {
  const element = audio;
  if (!element) return;
  if (!element.src && currentSource) {
    playSource(currentSource);
    return;
  }
  startElementPlayback(element);
}

function stopPlayback() {
  pendingGesturePlay = false;
  if (audio) {
    audio.pause();
    audio.removeAttribute('src');
  }
  playState = 'Stopped';
  updateMediaSessionPlaybackState('none');
  currentSource = null;
  queue = [];
  prefetchedKey = null;
  notice = null;
  emit();
}

function advanceQueue(skipRequested: boolean) {
  if (repeatOne && !skipRequested && currentSource) {
    playSource(currentSource);
    return;
  }
  const next = queue.shift();
  if (next) {
    playSource(next);
  } else {
    playState = 'Stopped';
    currentSource = null;
    if (audio) audio.removeAttribute('src');
    updateMediaSessionPlaybackState('none');
    reportNow();
    emit();
  }
}

function handleStreamFailure() {
  consecutiveStreamErrors += 1;
  if (consecutiveStreamErrors < MAX_CONSECUTIVE_STREAM_ERRORS && queue.length) {
    notice = notice ?? 'Playback failed for this track; skipping.';
    advanceQueue(true);
    return;
  }
  playState = 'Stopped';
  notice = notice ?? 'Playback failed for this track.';
  reportNow();
  emit();
}

function prefetchSource(source: SourceRef) {
  const selection = streamSelectionForSource(source);
  if (!selection || typeof fetch === 'undefined') return;
  const key = sourceKey(source);
  // `prefetch=1` keeps the server from treating the warm-up as the currently
  // playing stream in the zone's signal path.
  const url = `${selection.url}${selection.url.includes('?') ? '&' : '?'}prefetch=1`;
  fetch(url, {
    credentials: 'same-origin',
    headers: { Range: 'bytes=0-262143' }
  })
    .then((response) => {
      if (response.ok || response.status === 206) prefetchedKey = key;
    })
    .catch(() => undefined);
}

function sourceKey(source: SourceRef) {
  const id = Number(source.track_id) || 0;
  return source.kind === 'qobuz_track' ? `qobuz:${id}` : `local:${id}`;
}

function ensureAudio(): HTMLAudioElement | null {
  if (audio) return audio;
  if (typeof Audio === 'undefined') return null;
  const element = createAudioElement();
  audio = element;
  applyVolume();
  return element;
}

function createAudioElement(): HTMLAudioElement {
  const element = new Audio();
  element.preload = 'auto';
  element.setAttribute('playsinline', '');
  element.style.position = 'fixed';
  element.style.width = '1px';
  element.style.height = '1px';
  element.style.opacity = '0';
  element.style.pointerEvents = 'none';
  element.style.left = '-9999px';
  document.body?.appendChild(element);
  element.addEventListener('loadedmetadata', () => {
    if (audio !== element) return;
    if (currentSource) updateMediaSession(currentSource);
  });
  element.addEventListener('playing', () => {
    if (audio !== element) return;
    consecutiveStreamErrors = 0;
    playState = 'Playing';
    notice = null;
    updateMediaSessionPlaybackState('playing');
    if (currentSource) updateMediaSession(currentSource);
    reportNow();
    emit();
  });
  element.addEventListener('pause', () => {
    if (audio !== element) return;
    if (playState === 'Playing' || playState === 'Starting') {
      playState = element.ended || !element.src ? playState : 'Paused';
      updateMediaSessionPlaybackState(playState === 'Paused' ? 'paused' : 'none');
      reportNow();
      emit();
    }
  });
  element.addEventListener('waiting', () => {
    if (audio !== element) return;
    if (playState === 'Playing') {
      playState = 'Starting';
      reportNow();
    }
  });
  element.addEventListener('ended', () => {
    if (audio !== element) return;
    advanceQueue(false);
  });
  element.addEventListener('error', () => {
    if (audio !== element) return;
    if (element.src) handleStreamFailure();
  });
  return element;
}

/**
 * Volume rides the media element directly (0..1). EQ payloads in
 * `playback_config` are ignored here: EQ is applied server-side in the
 * stream, because any client-side WebAudio processing kills background
 * playback on iOS.
 */
function applyPlaybackConfig(config: PlaybackConfigPayload) {
  if (Number.isFinite(config.volume)) {
    volume = Math.max(0, Math.min(1, Number(config.volume)));
  }
  if (config.eq) {
    const nextEqActive = playbackConfigHasActiveEq(config);
    const nextEqSignature = nextEqActive ? eqSignatureForConfig(config.eq) : null;
    const eqChanged = nextEqSignature !== eqSignature;
    eqActive = nextEqActive;
    eqSignature = nextEqSignature;
    if (eqChanged) {
      restartCurrentStreamForServerDsp();
    }
  }
  applyVolume();
}

function applyVolume() {
  if (audio) audio.volume = volume;
}

function restartCurrentStreamForServerDsp() {
  const source = currentSource;
  const previousElement = audio;
  if (!source || !previousElement?.src) return;
  const wasActive = playState === 'Playing' || playState === 'Starting';
  const position = Number.isFinite(previousElement.currentTime) ? previousElement.currentTime : 0;
  const selection = streamSelectionForSource(source);
  if (!selection) return;

  // Use a fresh media element for the replacement stream. Reassigning `src`
  // on the active element can emit delayed error/ended events for the stream
  // that was deliberately aborted; those events used to be mistaken for a
  // real playback failure and skip to the next queued song.
  const element = createAudioElement();
  audio = element;
  applyVolume();
  element.src = selection.url;
  if (position > 0) {
    element.addEventListener(
      'loadedmetadata',
      () => {
        try {
          element.currentTime = position;
        } catch {
          // Seeking the replacement stream is best-effort.
        }
      },
      { once: true }
    );
  }
  if (wasActive) startElementPlayback(element);

  previousElement.pause();
  previousElement.removeAttribute('src');
  previousElement.remove();
}

function updateMediaSession(source: SourceRef) {
  if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return;
  if (typeof MediaMetadata !== 'undefined') {
    try {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: String(source.title || source.file_name || 'Unknown track'),
        artist: String(source.artist || ''),
        album: String(source.album || ''),
        artwork: mediaSessionArtwork(source)
      });
    } catch {
      // Metadata is progressive enhancement only.
    }
  }
  setMediaSessionAction('play', () => resumePlayback());
  setMediaSessionAction('pause', () => audio?.pause());
  setMediaSessionAction('nexttrack', () => advanceQueue(true));
  setMediaSessionAction('previoustrack', () => {
    // No queue history in the agent; restart the current track instead.
    if (audio) {
      audio.currentTime = 0;
      reportNow();
    }
  });
  setMediaSessionAction('seekto', (details) => {
    if (audio && Number.isFinite(details.seekTime)) {
      audio.currentTime = Math.max(0, Number(details.seekTime));
      reportNow();
    }
  });
}

function updateMediaSessionPlaybackState(state: MediaSessionPlaybackState) {
  if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return;
  try {
    navigator.mediaSession.playbackState = state;
  } catch {
    // Media Session is progressive enhancement only.
  }
}

function setMediaSessionAction(
  action: MediaSessionAction,
  handler: MediaSessionActionHandler | null
) {
  if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return;
  try {
    navigator.mediaSession.setActionHandler(action, handler);
  } catch {
    // Some Safari versions expose only a subset of handlers.
  }
}

function playbackConfigHasActiveEq(config: PlaybackConfigPayload) {
  const eq = config.eq;
  return Boolean(eq?.enabled && eq.bands?.some((band) => band.enabled));
}

function eqSignatureForConfig(eq: EqConfigPayload) {
  const payload = JSON.stringify({
    enabled: eq.enabled,
    preamp_db: eq.preamp_db,
    bands: eq.bands
      .filter((band) => band.enabled)
      .map((band) => ({
        type: band.type,
        freq_hz: band.freq_hz,
        gain_db: band.gain_db,
        q: band.q
      }))
  });
  let hash = 2166136261;
  for (let index = 0; index < payload.length; index += 1) {
    hash ^= payload.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}

function mediaSessionArtwork(source: SourceRef): MediaImage[] {
  const src = mediaSessionArtworkUrl(source);
  if (!src) return [];
  const type = mediaSessionArtworkType(src);
  return [96, 128, 192, 256, 384, 512].map((size) => ({
    src,
    sizes: `${size}x${size}`,
    ...(type ? { type } : {})
  }));
}

function mediaSessionArtworkUrl(source: SourceRef) {
  const qobuzUrl = typeof source.image_url === 'string' ? source.image_url.trim() : '';
  if (qobuzUrl) return absoluteUrl(qobuzSizedCoverUrl(qobuzUrl, 600));
  const artId = source.art_id;
  const path =
    artId !== null && artId !== undefined && artId !== ''
      ? `/api/library/art/${encodeURIComponent(String(artId))}?size=512`
      : mediaSessionFallbackArtworkPath(source);
  if (!path) return null;
  return absoluteUrl(path);
}

function mediaSessionFallbackArtworkPath(source: SourceRef) {
  const key = sourceKey(source);
  return key
    ? `/api/zones/${encodeURIComponent(browserZoneAgentId())}/now-playing-art?source=${encodeURIComponent(key)}`
    : null;
}

function absoluteUrl(src: string) {
  if (!src) return null;
  try {
    return new URL(src, window.location.href).href;
  } catch {
    return src;
  }
}

function qobuzSizedCoverUrl(url: string, size: number) {
  return url.replace(/_(?:org|max|\d+)\.(jpg|jpeg|png|webp)(?=$|\?)/i, `_${size}.$1`);
}

function mediaSessionArtworkType(src: string) {
  const path = src.split('?')[0]?.toLowerCase() || '';
  if (path.endsWith('.png')) return 'image/png';
  if (path.endsWith('.webp')) return 'image/webp';
  if (path.endsWith('.jpg') || path.endsWith('.jpeg') || path.includes('/api/library/art/')) {
    return 'image/jpeg';
  }
  return '';
}

/** Lock-screen position/scrubber state; best-effort like the metadata. */
function updateMediaSessionPosition(position: number, duration: number) {
  if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return;
  if (!navigator.mediaSession.setPositionState) return;
  try {
    if (duration > 0 && Number.isFinite(duration)) {
      navigator.mediaSession.setPositionState({
        duration,
        playbackRate: audio?.playbackRate || 1,
        position: Math.min(Math.max(0, position), duration)
      });
    } else {
      navigator.mediaSession.setPositionState();
    }
  } catch {
    // Media Session is progressive enhancement only.
  }
}

function startReporting() {
  stopReporting();
  scheduleNextReport();
}

function scheduleNextReport() {
  if (reportTimer !== null) window.clearTimeout(reportTimer);
  reportTimer = window.setTimeout(
    () => {
      reportTimer = null;
      reportNow();
      scheduleNextReport();
    },
    browserZoneReportIntervalMs(playState, document.visibilityState)
  );
}

function stopReporting() {
  if (reportTimer !== null) {
    window.clearTimeout(reportTimer);
    reportTimer = null;
  }
}

export function browserZoneReportIntervalMs(
  state: PlayState,
  visibilityState: DocumentVisibilityState
) {
  if (visibilityState === 'hidden' && state === 'Stopped') {
    return HIDDEN_IDLE_STATUS_REPORT_INTERVAL_MS;
  }
  if (state === 'Playing' || state === 'Starting') return PLAYING_STATUS_REPORT_INTERVAL_MS;
  if (state === 'Paused') return PAUSED_STATUS_REPORT_INTERVAL_MS;
  return IDLE_STATUS_REPORT_INTERVAL_MS;
}

function socketLooksDead(socket: WebSocket) {
  return (
    socket.readyState === WebSocket.CLOSING ||
    socket.readyState === WebSocket.CLOSED ||
    (socket.readyState === WebSocket.OPEN && socket.bufferedAmount > STUCK_SOCKET_BUFFERED_BYTES)
  );
}

function serverActivityIsStale() {
  return lastServerActivityAt > 0 && Date.now() - lastServerActivityAt > SERVER_ACTIVITY_STALE_MS;
}

/**
 * Drop a socket the browser still considers usable but whose peer is gone,
 * and reconnect. `close()` triggers the normal `onclose` reconnect path; the
 * explicit fallback covers sockets already past CLOSING where `onclose`
 * already ran (or never will).
 */
function recycleSocket(socket: WebSocket) {
  try {
    socket.close();
  } catch {
    // Already closed.
  }
  if (ws === socket) {
    ws = null;
    stopReporting();
    connection = 'error';
    clearReconnectTimer();
    connect();
  }
}

function reportNow() {
  const socket = ws;
  if (!socket || socket.readyState !== WebSocket.OPEN) return;
  if (document.visibilityState !== 'hidden' && serverActivityIsStale()) {
    recycleSocket(socket);
    return;
  }
  if (socket.bufferedAmount > STUCK_SOCKET_BUFFERED_BYTES) {
    // The peer stopped reading: reports are piling up client-side while the
    // server keeps serving the last state it saw (stale song/position in the
    // UI even though local playback moved on). Reconnect and re-register.
    recycleSocket(socket);
    return;
  }
  const source = currentSource;
  const position = audio && Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
  const elementDuration = audio && Number.isFinite(audio.duration) ? audio.duration : 0;
  const duration = elementDuration || Number(source?.duration_secs) || 0;
  if (playState === 'Playing' || playState === 'Paused') {
    updateMediaSessionPosition(position, duration);
  }
  socket.send(
    JSON.stringify({
      type: 'playback_state',
      current_source: source,
      state: playState,
      file_name: source ? String(source.file_name || source.title || '') || null : null,
      track_title: source?.title ?? null,
      track_artist: source?.artist ?? null,
      track_album: source?.album ?? null,
      position_secs: position,
      duration_secs: duration,
      source_rate: 0,
      target_rate: 0,
      source_bits: 0,
      target_bits: 0,
      volume
    })
  );
  socket.send(
    JSON.stringify({
      type: 'buffer_state',
      buffered_next: prefetchedKey,
      prefetching: false,
      buffered_bytes: 0
    })
  );
}
