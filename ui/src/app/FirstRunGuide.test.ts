import { describe, expect, it } from 'vitest';
import {
  markGettingStartedGuideComplete,
  qobuzEnabledForGettingStarted,
  shouldShowGettingStartedGuide,
  shouldShowGettingStartedGuideOnHost
} from './FirstRunGuide';

function memoryStorage(initial?: string) {
  let value = initial ?? null;
  return {
    getItem: () => value,
    setItem: (_key: string, next: string) => {
      value = next;
    }
  };
}

describe('first-run guide persistence', () => {
  it('shows until the guide is completed', () => {
    const storage = memoryStorage();

    expect(shouldShowGettingStartedGuide(storage)).toBe(true);
    markGettingStartedGuideComplete(storage);
    expect(shouldShowGettingStartedGuide(storage)).toBe(false);
  });

  it('fails open when browser storage is unavailable', () => {
    const unavailable = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      }
    };

    expect(shouldShowGettingStartedGuide(unavailable)).toBe(true);
    expect(() => markGettingStartedGuideComplete(unavailable)).not.toThrow();
  });

  it('only opens on the Mac hosting Fozmo', () => {
    const storage = memoryStorage();

    expect(shouldShowGettingStartedGuideOnHost(storage, 'localhost')).toBe(true);
    expect(shouldShowGettingStartedGuideOnHost(storage, '127.0.0.1')).toBe(true);
    expect(shouldShowGettingStartedGuideOnHost(storage, 'fozmo-studio.local')).toBe(false);
    expect(shouldShowGettingStartedGuideOnHost(storage, '192.168.1.20')).toBe(false);
  });

  it('treats initialized or authenticated Qobuz accounts as already set up', () => {
    expect(qobuzEnabledForGettingStarted(null)).toBe(false);
    expect(qobuzEnabledForGettingStarted({ initialized: false, logged_in: false })).toBe(false);
    expect(qobuzEnabledForGettingStarted({ initialized: true, logged_in: false })).toBe(true);
    expect(qobuzEnabledForGettingStarted({ authenticated: true })).toBe(true);
    expect(qobuzEnabledForGettingStarted({ logged_in: true })).toBe(true);
  });
});
