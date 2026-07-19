// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';
import { browserZoneReportIntervalMs } from './browserZoneAgent';

describe('browserZoneReportIntervalMs', () => {
  it('keeps active playback responsive while reducing idle network wakeups', () => {
    expect(browserZoneReportIntervalMs('Playing', 'visible')).toBe(1_000);
    expect(browserZoneReportIntervalMs('Starting', 'visible')).toBe(1_000);
    expect(browserZoneReportIntervalMs('Paused', 'visible')).toBe(2_500);
    expect(browserZoneReportIntervalMs('Stopped', 'visible')).toBe(5_000);
    expect(browserZoneReportIntervalMs('Stopped', 'hidden')).toBe(15_000);
  });
});
