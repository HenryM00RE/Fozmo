import { describe, expect, it } from 'vitest';
import type { ZoneProfile } from '../types';
import { resolvedOutputIcon } from './ZoneOutputIcon';

function browserZone(name: string): ZoneProfile {
  return {
    id: 'browser-test',
    name,
    protocol: 'remote_agent',
    browser: true
  };
}

describe('browser output icons', () => {
  it.each([
    'Safari on iPhone',
    'Safari on iPad',
    'Browser on iOS'
  ])('uses the iPhone icon for %s', (name) => {
    expect(resolvedOutputIcon(browserZone(name))).toBe('browser_ios');
  });

  it.each(['Chrome on Mac'])('uses the Chrome icon for desktop browser %s', (name) => {
    expect(resolvedOutputIcon(browserZone(name))).toBe('browser_chrome');
  });

  it.each([
    'Edge on Windows',
    'Firefox on Linux',
    'Safari on Mac'
  ])('uses the globe icon for non-Chrome desktop browser %s', (name) => {
    expect(resolvedOutputIcon(browserZone(name))).toBe('browser_other');
  });
});
