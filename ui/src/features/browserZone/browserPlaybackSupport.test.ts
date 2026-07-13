import { describe, expect, it } from 'vitest';
import type { QueueItem } from '../../shared/types';
import {
  type BrowserStreamPrefs,
  browserStreamSelectionForItem,
  browserStreamUrlForItem,
  expectedStreamMime,
  FLAC_UNSUPPORTED_NOTICE,
  opusStreamMime,
  unsupportedFormatNotice
} from './browserPlaybackSupport';

const localFlac: QueueItem = {
  title: 'Local Song',
  artist: 'Artist',
  album: 'Album',
  durationSecs: 180,
  filename: 'song.flac',
  ref: { track_id: 42, file_name: 'song.flac' }
};

const qobuzTrack: QueueItem = {
  title: 'Qobuz Song',
  artist: 'Artist',
  album: 'Album',
  durationSecs: 200,
  qobuzTrack: { id: 987654, title: 'Qobuz Song' }
};

// The per-device output preference is the single source of truth for quality:
// FLAC (or no saved preference) is best/lossless, Opus is data-saver.
const bestPrefs: BrowserStreamPrefs = { format: 'flac', opusKbps: 256 };
const dataSaverPrefs: BrowserStreamPrefs = { format: 'opus', opusKbps: 256 };

const probe = (supported: boolean) => ({
  canPlayType: () => (supported ? 'probably' : '')
});

const selectiveProbe = (supportedTypes: string[]) => ({
  canPlayType: (type: string) => (supportedTypes.includes(type) ? 'probably' : '')
});

describe('browserStreamUrlForItem', () => {
  it('maps local tracks to the same-origin local stream endpoint', () => {
    expect(browserStreamUrlForItem(localFlac)).toBe('/api/stream/local/42');
  });

  it('maps Qobuz tracks to the server proxy, never a CDN URL', () => {
    expect(browserStreamUrlForItem(qobuzTrack)).toBe('/api/stream/qobuz/987654');
  });

  it('maps data-saver Qobuz tracks to the lossy same-origin server proxy', () => {
    expect(browserStreamUrlForItem(qobuzTrack, { streamPrefs: dataSaverPrefs })).toBe(
      '/api/stream/qobuz/987654?quality=lossy'
    );
  });

  it('keeps Qobuz original quality for the best preference', () => {
    expect(browserStreamUrlForItem(qobuzTrack, { streamPrefs: bestPrefs })).toBe(
      '/api/stream/qobuz/987654'
    );
  });

  it('prefers the Qobuz mapping when both identifiers exist', () => {
    expect(browserStreamUrlForItem({ ...localFlac, qobuzTrack: { id: 7 } })).toBe(
      '/api/stream/qobuz/7'
    );
  });

  it('returns null for items without a streamable identifier', () => {
    expect(browserStreamUrlForItem(null)).toBeNull();
    expect(
      browserStreamUrlForItem({
        title: 'File only',
        artist: '',
        album: '',
        durationSecs: 0,
        ref: { file_name: 'x.flac' }
      })
    ).toBeNull();
  });

  it('never embeds tokens in stream URLs', () => {
    expect(browserStreamUrlForItem(localFlac)).not.toContain('token');
    expect(browserStreamUrlForItem(qobuzTrack)).not.toContain('token');
  });
});

describe('expectedStreamMime', () => {
  it('treats Qobuz best quality as FLAC', () => {
    expect(expectedStreamMime(qobuzTrack)).toBe('audio/flac');
    expect(expectedStreamMime(qobuzTrack, { streamPrefs: bestPrefs })).toBe('audio/flac');
  });

  it('treats data-saver Qobuz streams as MP3', () => {
    expect(expectedStreamMime(qobuzTrack, { streamPrefs: dataSaverPrefs })).toBe('audio/mpeg');
  });

  it('derives local mime types from the file extension', () => {
    expect(expectedStreamMime(localFlac)).toBe('audio/flac');
    expect(
      expectedStreamMime({
        ...localFlac,
        filename: 'x.mp3',
        ref: { track_id: 1, file_name: 'x.mp3' }
      })
    ).toBe('audio/mpeg');
  });

  it('returns null for unknown extensions', () => {
    expect(
      expectedStreamMime({ ...localFlac, filename: 'x', ref: { track_id: 1, file_name: 'x' } })
    ).toBeNull();
  });
});

describe('opusStreamMime', () => {
  it('returns the first supported Ogg Opus candidate', () => {
    expect(opusStreamMime(selectiveProbe(['audio/ogg; codecs="opus"']))).toBe(
      'audio/ogg; codecs="opus"'
    );
  });

  it('returns null when no Opus candidate is supported', () => {
    expect(opusStreamMime(probe(false))).toBeNull();
  });
});

describe('browserStreamSelectionForItem', () => {
  it('uses the lossless flac variant when the original is playable and no preference is set', () => {
    expect(
      browserStreamSelectionForItem(
        localFlac,
        selectiveProbe(['audio/flac', 'audio/ogg; codecs=opus'])
      )
    ).toEqual({
      url: '/api/stream/local/42?variant=flac',
      mime: 'audio/flac',
      variant: 'flac'
    });
  });

  it('attaches the zone id so the server can bake in that zone EQ', () => {
    expect(
      browserStreamSelectionForItem(
        localFlac,
        selectiveProbe(['audio/flac', 'audio/ogg; codecs=opus']),
        { zoneId: 'browser-abc' }
      )?.url
    ).toBe('/api/stream/local/42?variant=flac&zone=browser-abc');
    expect(
      browserStreamSelectionForItem(localFlac, selectiveProbe(['audio/ogg; codecs=opus']), {
        zoneId: 'browser-abc'
      })?.url
    ).toBe('/api/stream/local/42?variant=opus&zone=browser-abc');
  });

  it('uses the Opus variant when the original is unsupported and Opus is playable', () => {
    expect(
      browserStreamSelectionForItem(localFlac, selectiveProbe(['audio/ogg; codecs=opus']))
    ).toEqual({
      url: '/api/stream/local/42?variant=opus',
      mime: 'audio/ogg; codecs=opus',
      variant: 'opus'
    });
  });

  // Acceptance: remote browser + local FLAC + best quality keeps the lossless
  // route. Remote access no longer implies a data-saver downgrade.
  it('keeps local FLAC lossless with the best preference, even when Opus is playable', () => {
    expect(
      browserStreamSelectionForItem(
        localFlac,
        selectiveProbe(['audio/flac', 'audio/ogg; codecs=opus']),
        { streamPrefs: bestPrefs }
      )
    ).toEqual({
      url: '/api/stream/local/42?variant=flac',
      mime: 'audio/flac',
      variant: 'flac'
    });
  });

  // Acceptance: remote browser + local FLAC + data-saver uses the Opus
  // derivative at the chosen bitrate.
  it('uses the Opus variant for local lossless playback in data-saver mode', () => {
    expect(
      browserStreamSelectionForItem(
        localFlac,
        selectiveProbe(['audio/flac', 'audio/ogg; codecs=opus']),
        { streamPrefs: dataSaverPrefs }
      )
    ).toEqual({
      url: '/api/stream/local/42?variant=opus&kbps=256',
      mime: 'audio/ogg; codecs=opus',
      variant: 'opus'
    });
  });

  // Acceptance: remote browser + Qobuz + best quality uses the original route,
  // not quality=lossy.
  it('uses the original Qobuz route with the best preference', () => {
    expect(
      browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/flac']), {
        streamPrefs: bestPrefs
      })
    ).toEqual({
      url: '/api/stream/qobuz/987654',
      mime: 'audio/flac',
      variant: 'original'
    });
  });

  it('uses the original Qobuz route by default when no preference is set', () => {
    expect(browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/flac']))).toEqual({
      url: '/api/stream/qobuz/987654',
      mime: 'audio/flac',
      variant: 'original'
    });
  });

  // Acceptance: remote browser + Qobuz + data-saver uses the lossy proxy.
  it('uses lossy Qobuz streams in data-saver mode', () => {
    expect(
      browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/mpeg']), {
        streamPrefs: dataSaverPrefs
      })
    ).toEqual({
      url: '/api/stream/qobuz/987654?quality=lossy',
      mime: 'audio/mpeg',
      variant: 'lossy'
    });
  });

  it('uses a server-side FLAC derivative for Qobuz when EQ is active and FLAC is playable', () => {
    expect(
      browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/mpeg', 'audio/flac']), {
        streamPrefs: dataSaverPrefs,
        zoneId: 'browser-abc',
        eqActive: true,
        eqSignature: 'eq123'
      })
    ).toEqual({
      url: '/api/stream/qobuz/987654?quality=lossy&zone=browser-abc&eq_sig=eq123&variant=flac&eq=1',
      mime: 'audio/flac',
      variant: 'flac'
    });
  });

  it('keeps data-saver Qobuz lossy playback when EQ is active but no derivative is playable', () => {
    expect(
      browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/mpeg']), {
        streamPrefs: dataSaverPrefs,
        zoneId: 'browser-abc',
        eqActive: true
      })
    ).toEqual({
      url: '/api/stream/qobuz/987654?quality=lossy&zone=browser-abc',
      mime: 'audio/mpeg',
      variant: 'lossy'
    });
  });

  it('uses an Opus Qobuz EQ derivative in data-saver mode when the browser supports it', () => {
    expect(
      browserStreamSelectionForItem(qobuzTrack, selectiveProbe(['audio/ogg; codecs=opus']), {
        streamPrefs: dataSaverPrefs,
        zoneId: 'browser-abc',
        eqActive: true,
        eqSignature: 'eq456'
      })
    ).toEqual({
      url: '/api/stream/qobuz/987654?quality=lossy&zone=browser-abc&eq_sig=eq456&variant=opus&eq=1',
      mime: 'audio/ogg; codecs=opus',
      variant: 'opus'
    });
  });

  it('adds an EQ signature to local derivative URLs so changed bands refetch the stream', () => {
    expect(
      browserStreamSelectionForItem(
        localFlac,
        selectiveProbe(['audio/flac', 'audio/ogg; codecs=opus']),
        { zoneId: 'browser-abc', eqSignature: 'eq789' }
      )?.url
    ).toBe('/api/stream/local/42?variant=flac&zone=browser-abc&eq_sig=eq789');
  });
});

describe('unsupportedFormatNotice', () => {
  it('allows playback when canPlayType reports support', () => {
    expect(unsupportedFormatNotice(localFlac, probe(true))).toBeNull();
    expect(unsupportedFormatNotice(qobuzTrack, probe(true))).toBeNull();
  });

  it('warns with a Safari-oriented notice when FLAC is unsupported', () => {
    expect(unsupportedFormatNotice(localFlac, probe(false))).toBe(FLAC_UNSUPPORTED_NOTICE);
    expect(unsupportedFormatNotice(qobuzTrack, probe(false))).toBe(FLAC_UNSUPPORTED_NOTICE);
  });

  it('does not treat data-saver Qobuz streams as FLAC', () => {
    expect(
      unsupportedFormatNotice(qobuzTrack, selectiveProbe(['audio/mpeg']), {
        streamPrefs: dataSaverPrefs
      })
    ).toBeNull();
  });

  it('allows unknown formats to attempt playback', () => {
    expect(
      unsupportedFormatNotice(
        { ...localFlac, filename: 'x', ref: { track_id: 1, file_name: 'x' } },
        probe(false)
      )
    ).toBeNull();
  });
});
