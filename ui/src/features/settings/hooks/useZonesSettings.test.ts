import { describe, expect, it } from 'vitest';
import {
  airPlayDefaultVolumeDraft,
  airPlayMaxVolumeDraft,
  normalizeAirPlayDefaultVolumePercent,
  normalizeAirPlayMaxVolumePercent
} from './useZonesSettings';

describe('AirPlay default volume settings', () => {
  it('starts unset outputs at 40 percent and preserves saved defaults', () => {
    expect(airPlayDefaultVolumeDraft(undefined)).toBe('40');
    expect(airPlayDefaultVolumeDraft(null)).toBe('40');
    expect(airPlayDefaultVolumeDraft(0.63)).toBe('63');
    expect(airPlayMaxVolumeDraft(undefined)).toBe('100');
    expect(airPlayMaxVolumeDraft(0.72)).toBe('72');
  });

  it('normalizes text input to a safe percentage', () => {
    expect(normalizeAirPlayDefaultVolumePercent('')).toBe(40);
    expect(normalizeAirPlayDefaultVolumePercent('72')).toBe(72);
    expect(normalizeAirPlayDefaultVolumePercent('120')).toBe(100);
    expect(normalizeAirPlayDefaultVolumePercent('-5')).toBe(0);
    expect(normalizeAirPlayMaxVolumePercent('')).toBe(100);
    expect(normalizeAirPlayMaxVolumePercent('85')).toBe(85);
  });
});
