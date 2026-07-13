import { describe, expect, it } from 'vitest';
import {
  isLoopbackHostname,
  pairingTokenFromHash,
  remoteAuthStateFromStatusProbe,
  remoteLinkCodeFromHash
} from './useRemoteLinkExchange';

describe('remote link exchange hash parsing', () => {
  it('extracts only explicit remote link codes', () => {
    expect(remoteLinkCodeFromHash('#link=abc123')).toBe('abc123');
    expect(remoteLinkCodeFromHash('#?link=abc%2F123')).toBe('abc/123');
    expect(remoteLinkCodeFromHash('#/settings/remote')).toBe('');
  });
});

describe('LAN pairing hash parsing', () => {
  it('extracts only the dedicated pair route and decodes it', () => {
    expect(pairingTokenFromHash('#/pair/abc-123_')).toBe('abc-123_');
    expect(pairingTokenFromHash('#/pair/abc%2F123')).toBe('abc/123');
    expect(pairingTokenFromHash('#/settings/remote')).toBe('');
    expect(pairingTokenFromHash('#?pair=abc')).toBe('');
  });

  it('recognises loopback host spellings', () => {
    expect(isLoopbackHostname('localhost')).toBe(true);
    expect(isLoopbackHostname('127.0.0.1')).toBe(true);
    expect(isLoopbackHostname('[::1]')).toBe(true);
    expect(isLoopbackHostname('fozmo-studio.local')).toBe(false);
  });
});

describe('remote auth status probe classification', () => {
  it('blocks https auth failures as a remote unauthorised surface', () => {
    expect(
      remoteAuthStateFromStatusProbe({ protocol: 'https:', status: 401, surface: undefined })
    ).toBe('unauthorised');
    expect(remoteAuthStateFromStatusProbe({ protocol: 'https:', status: 403 })).toBe(
      'unauthorised'
    );
  });

  it('lets local http auth failures fall through to the existing pairing flow', () => {
    expect(
      remoteAuthStateFromStatusProbe({ protocol: 'http:', status: 401, hostname: 'localhost' })
    ).toBe('authorised');
  });

  it('blocks unauthorised LAN http browsers until a pairing link is exchanged', () => {
    expect(
      remoteAuthStateFromStatusProbe({
        protocol: 'http:',
        status: 401,
        hostname: 'fozmo-studio.local'
      })
    ).toBe('unauthorised');
  });

  it('allows successful local and remote probes', () => {
    expect(
      remoteAuthStateFromStatusProbe({ protocol: 'https:', status: 200, surface: 'remote' })
    ).toBe('authorised');
    expect(
      remoteAuthStateFromStatusProbe({ protocol: 'http:', status: 200, surface: 'local' })
    ).toBe('authorised');
  });
});
