import {
  type CustomDisplayFontSettings,
  getActiveCustomDisplayFontSettings,
  normalizedSupportedRanges,
  rangesIncludeCodepoint
} from './theme';

export function displayTitleUsesFallbackFont(
  title: string,
  settings: CustomDisplayFontSettings | null | undefined = getActiveCustomDisplayFontSettings()
) {
  if (/\d/.test(title)) return true;
  if (!settings?.custom_display_font_enabled || !settings.custom_display_font_url) return false;

  const ranges = normalizedSupportedRanges(settings.custom_display_font_supported_ranges);
  if (!ranges.length) return false;

  return Array.from(title).some((character) => {
    const codepoint = character.codePointAt(0);
    return codepoint === undefined || !rangesIncludeCodepoint(ranges, codepoint);
  });
}
