import { describe, expect, it } from 'vitest';
import {
  type CustomDisplayFontSettings,
  customDisplayFontCoverage,
  customDisplayFontCoverageMessage
} from './theme';

function settingsWithRanges(ranges: [number, number][]): CustomDisplayFontSettings {
  return {
    custom_display_font_enabled: true,
    custom_display_font_url: '/user-fonts/custom-display.ttf',
    custom_display_font_supported_ranges: ranges
  };
}

describe('custom display font coverage', () => {
  it('detects fonts missing numbers', () => {
    const coverage = customDisplayFontCoverage(
      settingsWithRanges([
        [0x20, 0x2f],
        [0x3a, 0x7e],
        [0xe9, 0xe9],
        [0xf1, 0xf1],
        [0xf6, 0xf6],
        [0xfc, 0xfc]
      ])
    );

    expect(coverage.adequate).toBe(false);
    expect(coverage.missing).toContain('0');
    expect(coverage.missing).toContain('9');
    expect(customDisplayFontCoverageMessage(settingsWithRanges([[0x41, 0x5a]]))).toContain(
      'Missing glyphs will use DM Sans'
    );
  });

  it('detects fonts missing punctuation', () => {
    const coverage = customDisplayFontCoverage(
      settingsWithRanges([
        [0x30, 0x39],
        [0x41, 0x5a],
        [0x61, 0x7a],
        [0xe9, 0xe9],
        [0xf1, 0xf1],
        [0xf6, 0xf6],
        [0xfc, 0xfc]
      ])
    );

    expect(coverage.adequate).toBe(false);
    expect(coverage.missing).toContain('.');
    expect(coverage.missing).toContain('?');
  });

  it('formats unicode ranges for per-glyph browser fallback', () => {
    const coverage = customDisplayFontCoverage(settingsWithRanges([[0x20, 0x7e]]));

    expect(coverage.unicodeRange).toBe('U+0020-007E');
  });
});
