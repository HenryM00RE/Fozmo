import { describe, expect, it } from 'vitest';
import { displayTitleUsesFallbackFont } from './displayTitle';
import type { CustomDisplayFontSettings } from './theme';

function settingsWithRanges(ranges: [number, number][]): CustomDisplayFontSettings {
  return {
    custom_display_font_enabled: true,
    custom_display_font_url: '/user-fonts/custom-display.ttf',
    custom_display_font_supported_ranges: ranges
  };
}

const basicLatinWithPunctuation = settingsWithRanges([[0x20, 0x7e]]);

describe('displayTitleUsesFallbackFont', () => {
  it('keeps punctuation on the custom display face', () => {
    expect(displayTitleUsesFallbackFont('Play it loud?!', basicLatinWithPunctuation)).toBe(false);
  });

  it('falls back for titles with numbers', () => {
    expect(displayTitleUsesFallbackFont('Selected Ambient Works 85-92')).toBe(true);
  });

  it('falls back when an accented character is outside the custom font coverage', () => {
    expect(displayTitleUsesFallbackFont('MEDÚLLA', basicLatinWithPunctuation)).toBe(true);
    expect(displayTitleUsesFallbackFont('Björk', basicLatinWithPunctuation)).toBe(true);
  });

  it('keeps accented titles on the custom display face when the glyphs are covered', () => {
    const settings = settingsWithRanges([
      [0x20, 0x7e],
      [0xda, 0xda],
      [0xf6, 0xf6]
    ]);

    expect(displayTitleUsesFallbackFont('MEDÚLLA', settings)).toBe(false);
    expect(displayTitleUsesFallbackFont('Björk', settings)).toBe(false);
  });
});
