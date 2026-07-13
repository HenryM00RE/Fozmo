export const WALNUT_THEME_STORAGE_KEY = 'walnut-theme';
const CUSTOM_DISPLAY_FONT_FAMILY = 'Fozmo Custom Display';

export const walnutThemes = ['light', 'neutral', 'dark'] as const;

export type WalnutTheme = (typeof walnutThemes)[number];

export type CustomDisplayFontSettings = {
  custom_display_font_enabled?: boolean;
  custom_display_font_scale_percent?: number;
  custom_display_font_name?: string | null;
  custom_display_font_url?: string | null;
  custom_display_font_version?: number;
  custom_display_font_supported_ranges?: [number, number][];
};

const CUSTOM_DISPLAY_FONT_REQUIRED_TEXT =
  'Fozmo Library Settings Playback Queue Output 0123456789 .,;:!?-()[]/&+% "\' éñöü';

export type CustomDisplayFontCoverage = {
  adequate: boolean;
  missing: string[];
  unicodeRange: string;
};

export function normalizedSupportedRanges(ranges?: [number, number][]) {
  return (ranges || [])
    .map(([start, end]) => [Number(start), Number(end)] as const)
    .filter(
      ([start, end]) =>
        Number.isInteger(start) &&
        Number.isInteger(end) &&
        start >= 0 &&
        end >= start &&
        end <= 0x10ffff
    )
    .sort(([left], [right]) => left - right);
}

function formatUnicodeRangeCodepoint(value: number) {
  return value.toString(16).toUpperCase().padStart(4, '0');
}

export function supportedRangesToUnicodeRange(ranges?: [number, number][]) {
  return normalizedSupportedRanges(ranges)
    .map(([start, end]) =>
      start === end
        ? `U+${formatUnicodeRangeCodepoint(start)}`
        : `U+${formatUnicodeRangeCodepoint(start)}-${formatUnicodeRangeCodepoint(end)}`
    )
    .join(', ');
}

export function rangesIncludeCodepoint(
  ranges: readonly (readonly [number, number])[],
  codepoint: number
) {
  return ranges.some(([start, end]) => codepoint >= start && codepoint <= end);
}

export function customDisplayFontCoverage(
  settings: CustomDisplayFontSettings | null | undefined
): CustomDisplayFontCoverage {
  const ranges = normalizedSupportedRanges(settings?.custom_display_font_supported_ranges);
  const missing = Array.from(new Set(Array.from(CUSTOM_DISPLAY_FONT_REQUIRED_TEXT))).filter(
    (character) => {
      const codepoint = character.codePointAt(0);
      return codepoint === undefined || !rangesIncludeCodepoint(ranges, codepoint);
    }
  );
  return {
    adequate: ranges.length > 0 && missing.length === 0,
    missing,
    unicodeRange: supportedRangesToUnicodeRange(settings?.custom_display_font_supported_ranges)
  };
}

export function customDisplayFontCoverageMessage(
  settings: CustomDisplayFontSettings | null | undefined
) {
  const coverage = customDisplayFontCoverage(settings);
  if (coverage.adequate) return '';
  if (!settings?.custom_display_font_url) return '';
  const preview = coverage.missing.slice(0, 8).join(' ');
  const extra = coverage.missing.length > 8 ? ` +${coverage.missing.length - 8} more` : '';
  return preview
    ? `Missing glyphs will use DM Sans: ${preview}${extra}.`
    : 'Missing glyphs will use DM Sans.';
}

export function isWalnutTheme(value: unknown): value is WalnutTheme {
  return typeof value === 'string' && (walnutThemes as readonly string[]).includes(value);
}

export function readStoredWalnutTheme(fallback: WalnutTheme = 'dark'): WalnutTheme {
  try {
    const stored = localStorage.getItem(WALNUT_THEME_STORAGE_KEY);
    if (isWalnutTheme(stored)) return stored;
  } catch {
    return fallback;
  }

  return fallback;
}

export function applyWalnutTheme(theme: WalnutTheme) {
  document.documentElement.dataset.theme = theme;
}

export function persistWalnutTheme(theme: WalnutTheme) {
  try {
    localStorage.setItem(WALNUT_THEME_STORAGE_KEY, theme);
  } catch {
    // Theme persistence is best-effort; still apply the theme for this session.
  }
}

let activeCustomDisplayFont: FontFace | null = null;
let activeCustomDisplayFontUrl = '';
let activeCustomDisplayFontSettings: CustomDisplayFontSettings | null = null;
const customDisplayFontSettingsListeners = new Set<
  (settings: CustomDisplayFontSettings | null) => void
>();

export function getActiveCustomDisplayFontSettings() {
  return activeCustomDisplayFontSettings;
}

export function subscribeToCustomDisplayFontSettings(
  listener: (settings: CustomDisplayFontSettings | null) => void
) {
  customDisplayFontSettingsListeners.add(listener);
  return () => {
    customDisplayFontSettingsListeners.delete(listener);
  };
}

function updateActiveCustomDisplayFontSettings(settings: CustomDisplayFontSettings | null) {
  activeCustomDisplayFontSettings = settings;
  customDisplayFontSettingsListeners.forEach((listener) => listener(settings));
}

export async function applyCustomDisplayFont(settings: CustomDisplayFontSettings | null) {
  updateActiveCustomDisplayFontSettings(settings);
  const root = document.documentElement;
  const enabled = Boolean(settings?.custom_display_font_enabled);
  const url = String(settings?.custom_display_font_url || '').trim();
  const scalePercent = normalizedCustomDisplayFontScale(
    settings?.custom_display_font_scale_percent
  );

  root.style.setProperty('--custom-display-font-scale', String(scalePercent / 100));

  if (!enabled || !url) {
    clearCustomDisplayFont();
    return;
  }
  const coverage = customDisplayFontCoverage(settings);

  root.style.setProperty(
    '--font-custom-display',
    `"${CUSTOM_DISPLAY_FONT_FAMILY}", var(--font-ui, "DM Sans", Inter, system-ui, sans-serif)`
  );
  root.dataset.customDisplayFont = 'on';

  if (activeCustomDisplayFont && activeCustomDisplayFontUrl === url) return;
  clearLoadedCustomDisplayFont();

  const descriptors: FontFaceDescriptors = { display: 'swap' };
  if (coverage.unicodeRange) descriptors.unicodeRange = coverage.unicodeRange;
  const font = new FontFace(
    CUSTOM_DISPLAY_FONT_FAMILY,
    `url("${url}") format("truetype")`,
    descriptors
  );
  activeCustomDisplayFont = font;
  activeCustomDisplayFontUrl = url;
  try {
    await font.load();
    document.fonts.add(font);
  } catch {
    if (activeCustomDisplayFont === font) clearCustomDisplayFont();
  }
}

export function normalizedCustomDisplayFontScale(value: unknown) {
  const numberValue = Number(value);
  if (!Number.isFinite(numberValue)) return 100;
  return Math.min(140, Math.max(70, Math.round(numberValue)));
}

function clearCustomDisplayFont() {
  const root = document.documentElement;
  root.style.removeProperty('--font-custom-display');
  delete root.dataset.customDisplayFont;
  clearLoadedCustomDisplayFont();
}

function clearLoadedCustomDisplayFont() {
  if (activeCustomDisplayFont) {
    document.fonts.delete(activeCustomDisplayFont);
  }
  activeCustomDisplayFont = null;
  activeCustomDisplayFontUrl = '';
}
