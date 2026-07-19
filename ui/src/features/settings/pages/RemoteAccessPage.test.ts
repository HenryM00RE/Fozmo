import { describe, expect, it } from 'vitest';
import { remoteLinkGenerationAccess } from './RemoteAccessPage';

describe('remote link-code generation access', () => {
  it('allows the capability reported for the host device', () => {
    expect(
      remoteLinkGenerationAccess('host_local', { hostDevice: true, remoteSurface: false })
    ).toEqual({ allowed: true, reason: '' });
  });

  it('allows an authenticated LAN controller', () => {
    expect(
      remoteLinkGenerationAccess('authenticated_lan', {
        hostDevice: false,
        remoteSurface: false
      })
    ).toEqual({ allowed: true, reason: '' });
  });

  it('keeps trusted-LAN access separate from Remote Access linking', () => {
    const access = remoteLinkGenerationAccess('unavailable', {
      hostDevice: false,
      remoteSurface: false
    });

    expect(access.allowed).toBe(false);
    expect(access.reason).toContain('Pair this LAN browser');
  });

  it('does not let an already-remote browser issue another link code', () => {
    const access = remoteLinkGenerationAccess('unavailable', {
      hostDevice: false,
      remoteSurface: true
    });

    expect(access.allowed).toBe(false);
    expect(access.reason).toContain('Host Device or an authenticated LAN controller');
  });
});
