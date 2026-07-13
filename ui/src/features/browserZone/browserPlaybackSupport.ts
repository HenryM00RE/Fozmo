import type { QueueItem } from '../../shared/types';

/** Minimal `canPlayType` surface so support checks are testable off-DOM. */
export interface AudioFormatProbe {
  canPlayType(type: string): string;
}

/** Mirrors the server's `audio_content_type_for_path` extension mapping. */
const EXTENSION_MIME: Record<string, string> = {
  flac: 'audio/flac',
  mp3: 'audio/mpeg',
  wav: 'audio/wav',
  wave: 'audio/wav',
  m4a: 'audio/mp4',
  mp4: 'audio/mp4',
  ogg: 'audio/ogg',
  oga: 'audio/ogg',
  opus: 'audio/opus',
  aif: 'audio/aiff',
  aiff: 'audio/aiff'
};

export const FLAC_UNSUPPORTED_NOTICE =
  'This browser cannot play original-quality FLAC or the Opus fallback stream. Use Chrome, Firefox, or Edge for browser playback.';

/**
 * The user's per-device stream delivery choice from the output settings
 * modal. `null` means nothing chosen yet, in which case playback defaults to
 * best/lossless quality on every surface: the Remote Access surface affects
 * auth and routing but never downgrades audio quality on its own. `'opus'` is
 * the explicit data-saver mode.
 */
export interface BrowserStreamPrefs {
  format: 'flac' | 'opus';
  opusKbps: number;
}

interface BrowserStreamOptions {
  /**
   * Zone (= agent) id attached to local stream URLs so the server can bake
   * that zone's parametric EQ into the stream. EQ is applied server-side
   * because client-side WebAudio kills background playback on iOS.
   */
  zoneId?: string;
  streamPrefs?: BrowserStreamPrefs | null;
  eqActive?: boolean;
  eqSignature?: string | null;
}

/**
 * Whether the caller has opted into the data-saver stream. Driven purely by
 * the per-device output preference, never by the remote-access surface: local
 * lossless files map to the Opus derivative and Qobuz maps to the lossy MP3
 * proxy only in this mode. With no saved preference, playback stays lossless.
 */
function isDataSaver(options: BrowserStreamOptions): boolean {
  return options.streamPrefs?.format === 'opus';
}

/**
 * MIME strings probed for the server's Ogg Opus playback derivative. Safari
 * support has historically been container-sensitive, so several spellings of
 * the Ogg Opus type are probed before the variant is selected; the server
 * serves the stream as `audio/ogg` (Ogg container, Opus codec).
 */
export const OPUS_STREAM_MIME_CANDIDATES = [
  'audio/ogg; codecs=opus',
  'audio/ogg; codecs="opus"'
] as const;

export function defaultAudioFormatProbe(): AudioFormatProbe | null {
  if (typeof document === 'undefined') return null;
  try {
    return document.createElement('audio');
  } catch {
    return null;
  }
}

function qobuzStreamTrackId(item: QueueItem) {
  const id = Number(item.qobuzTrack?.id ?? item.qobuzTrack?.track_id);
  return Number.isFinite(id) && id > 0 ? id : 0;
}

function localStreamTrackId(item: QueueItem) {
  const id = Number(item.ref?.track_id);
  return Number.isFinite(id) && id > 0 ? id : 0;
}

/**
 * Same-origin authenticated stream URL for a queue item. Cookies carry auth
 * on both the LAN and remote surfaces, so URLs never embed tokens, and Qobuz
 * items always map to the server proxy — never a CDN URL.
 */
export function browserStreamUrlForItem(
  item: QueueItem | null | undefined,
  options: BrowserStreamOptions = {}
): string | null {
  if (!item) return null;
  const qobuzId = qobuzStreamTrackId(item);
  if (qobuzId) {
    const query = isDataSaver(options) ? '?quality=lossy' : '';
    return `/api/stream/qobuz/${qobuzId}${query}`;
  }
  const localId = localStreamTrackId(item);
  if (localId) return `/api/stream/local/${localId}`;
  return null;
}

/** Expected stream mime type for the selected browser playback profile. */
export function expectedStreamMime(
  item: QueueItem,
  options: BrowserStreamOptions = {}
): string | null {
  if (qobuzStreamTrackId(item)) {
    return isDataSaver(options) ? 'audio/mpeg' : 'audio/flac';
  }
  const name = String(item.ref?.file_name || item.filename || '');
  const dot = name.lastIndexOf('.');
  if (dot < 0) return null;
  return EXTENSION_MIME[name.slice(dot + 1).toLowerCase()] ?? null;
}

/** First Ogg Opus mime string this browser reports it can play, if any. */
export function opusStreamMime(probe: AudioFormatProbe | null): string | null {
  if (!probe) return null;
  for (const candidate of OPUS_STREAM_MIME_CANDIDATES) {
    if (probe.canPlayType(candidate)) return candidate;
  }
  return null;
}

export interface BrowserStreamSelection {
  url: string;
  mime: string | null;
  variant: 'original' | 'flac' | 'opus' | 'lossy';
}

/** Query string for a local derivative stream (`variant` + optional zone). */
function localStreamQuery(variant: 'flac' | 'opus', options: BrowserStreamOptions) {
  const params = new URLSearchParams({ variant });
  if (variant === 'opus' && options.streamPrefs?.format === 'opus') {
    params.set('kbps', String(options.streamPrefs.opusKbps));
  }
  if (options.zoneId) params.set('zone', options.zoneId);
  return `?${params.toString()}`;
}

/**
 * Stream URL and format for a queue item, choosing between the original file
 * and the server's Ogg Opus playback derivative. Quality follows the
 * per-device output preference, never the Remote Access surface: the Opus
 * derivative (local) and the lossy MP3 proxy (Qobuz) are used only when the
 * device has explicitly opted into data-saver mode, or — for local files —
 * when the browser cannot play the original format (e.g. Safari with
 * FLAC/AIFF) and can play Opus. With no saved preference, playback stays
 * lossless on every surface. URLs never embed tokens — cookies carry auth on
 * both surfaces.
 */
export function browserStreamSelectionForItem(
  item: QueueItem | null | undefined,
  probe: AudioFormatProbe | null,
  options: BrowserStreamOptions = {}
): BrowserStreamSelection | null {
  if (!item) return null;
  const dataSaver = isDataSaver(options);
  const qobuzId = qobuzStreamTrackId(item);
  if (qobuzId) {
    const params = new URLSearchParams();
    if (dataSaver) params.set('quality', 'lossy');
    if (options.zoneId) params.set('zone', options.zoneId);
    if (options.eqSignature) params.set('eq_sig', options.eqSignature);
    if (options.eqActive) {
      const opusMime = opusStreamMime(probe);
      const flacPlayable = !probe || !!probe.canPlayType('audio/flac');
      const variant = dataSaver && opusMime ? 'opus' : flacPlayable ? 'flac' : null;
      if (!variant) {
        return {
          url: `/api/stream/qobuz/${qobuzId}${params.toString() ? `?${params.toString()}` : ''}`,
          mime: dataSaver ? 'audio/mpeg' : 'audio/flac',
          variant: dataSaver ? 'lossy' : 'original'
        };
      }
      params.set('variant', variant);
      params.set('eq', '1');
      const query = params.toString() ? `?${params.toString()}` : '';
      return {
        url: `/api/stream/qobuz/${qobuzId}${query}`,
        mime: variant === 'opus' ? opusMime : 'audio/flac',
        variant
      };
    }
    const query = params.toString() ? `?${params.toString()}` : '';
    if (dataSaver) {
      return {
        url: `/api/stream/qobuz/${qobuzId}${query}`,
        mime: 'audio/mpeg',
        variant: 'lossy'
      };
    }
    return { url: `/api/stream/qobuz/${qobuzId}${query}`, mime: 'audio/flac', variant: 'original' };
  }
  const localId = localStreamTrackId(item);
  if (!localId) return null;
  const originalMime = expectedStreamMime(item);
  const opusMime = opusStreamMime(probe);
  const originalUnplayable = !!probe && !!originalMime && !probe.canPlayType(originalMime);
  // Data-saver picks the Opus derivative; otherwise the original bytes are
  // passed through unless the browser cannot play them, where Opus is the
  // fallback. Either way Opus needs the browser to actually play it.
  const wantsOpus = dataSaver || originalUnplayable;
  if (opusMime && wantsOpus) {
    const query = localStreamQuery('opus', options);
    const suffix = options.eqSignature
      ? `${query}&eq_sig=${encodeURIComponent(options.eqSignature)}`
      : query;
    return {
      url: `/api/stream/local/${localId}${suffix}`,
      mime: opusMime,
      variant: 'opus'
    };
  }
  // "flac" is the lossless variant: the server passes the original bytes
  // through untouched unless the zone has active EQ to bake in.
  const query = localStreamQuery('flac', options);
  const suffix = options.eqSignature
    ? `${query}&eq_sig=${encodeURIComponent(options.eqSignature)}`
    : query;
  return {
    url: `/api/stream/local/${localId}${suffix}`,
    mime: originalMime,
    variant: 'flac'
  };
}

/**
 * A user-facing warning when the browser reports it cannot play the item's
 * expected format, or null when playback should proceed. Unknown formats and
 * probe-less environments proceed and rely on runtime error handling.
 */
export function unsupportedFormatNotice(
  item: QueueItem,
  probe: AudioFormatProbe | null,
  options: BrowserStreamOptions = {}
): string | null {
  if (!probe) return null;
  const mime = expectedStreamMime(item, options);
  if (!mime) return null;
  if (probe.canPlayType(mime)) return null;
  if (mime === 'audio/flac') return FLAC_UNSUPPORTED_NOTICE;
  return `This browser cannot play ${mime} streams.`;
}
