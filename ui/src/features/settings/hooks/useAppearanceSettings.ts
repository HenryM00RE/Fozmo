import { useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  applyCustomDisplayFont,
  applyWalnutTheme,
  type CustomDisplayFontSettings,
  customDisplayFontCoverageMessage,
  getActiveCustomDisplayFontSettings,
  isWalnutTheme,
  normalizedCustomDisplayFontScale,
  persistWalnutTheme,
  readStoredWalnutTheme,
  subscribeToCustomDisplayFontSettings,
  type WalnutTheme
} from '../../../shared/lib/theme';

export function useAppearanceSettings() {
  const [theme, setTheme] = useState<WalnutTheme>(() => {
    const currentTheme = document.documentElement.dataset.theme;
    return isWalnutTheme(currentTheme) ? currentTheme : readStoredWalnutTheme();
  });
  const [customDisplayFont, setCustomDisplayFont] = useState<CustomDisplayFontSettings | null>(() =>
    getActiveCustomDisplayFontSettings()
  );
  const [appearanceStatus, setAppearanceStatus] = useState('');
  const scaleSaveTimerRef = useRef<number | null>(null);
  const scaleSaveTokenRef = useRef(0);

  useEffect(
    () => subscribeToCustomDisplayFontSettings((settings) => setCustomDisplayFont(settings)),
    []
  );

  useEffect(() => {
    applyWalnutTheme(theme);
    persistWalnutTheme(theme);
  }, [theme]);

  useEffect(() => {
    let cancelled = false;
    endpoints
      .appearance()
      .then((settings) => {
        if (cancelled) return;
        setCustomDisplayFont(settings);
        applyCustomDisplayFont(settings);
      })
      .catch(() => {
        if (!cancelled) setAppearanceStatus('Display font settings are unavailable.');
      });
    return () => {
      cancelled = true;
      if (scaleSaveTimerRef.current !== null) window.clearTimeout(scaleSaveTimerRef.current);
    };
  }, []);

  const clearPendingScaleSave = () => {
    scaleSaveTokenRef.current += 1;
    if (scaleSaveTimerRef.current !== null) {
      window.clearTimeout(scaleSaveTimerRef.current);
      scaleSaveTimerRef.current = null;
    }
  };

  const saveCustomDisplayFontSettings = async (next: CustomDisplayFontSettings) => {
    clearPendingScaleSave();
    setCustomDisplayFont(next);
    await applyCustomDisplayFont(next);
    const saved = await endpoints.saveAppearance({
      custom_display_font_enabled: Boolean(next.custom_display_font_enabled),
      custom_display_font_scale_percent: normalizedCustomDisplayFontScale(
        next.custom_display_font_scale_percent
      )
    });
    setCustomDisplayFont(saved);
    await applyCustomDisplayFont(saved);
  };

  const setCustomDisplayFontEnabled = (enabled: boolean) => {
    if (!customDisplayFont?.custom_display_font_url) return;
    saveCustomDisplayFontSettings({
      ...customDisplayFont,
      custom_display_font_enabled: enabled
    }).catch((error) =>
      setAppearanceStatus(
        error instanceof Error ? error.message : 'Display font could not be saved.'
      )
    );
  };

  const setCustomDisplayFontScale = (scale: number) => {
    if (!customDisplayFont?.custom_display_font_url) return;
    const next = {
      ...customDisplayFont,
      custom_display_font_scale_percent: normalizedCustomDisplayFontScale(scale)
    };
    const token = scaleSaveTokenRef.current + 1;
    scaleSaveTokenRef.current = token;
    setCustomDisplayFont(next);
    applyCustomDisplayFont(next).catch(() => undefined);
    if (scaleSaveTimerRef.current !== null) window.clearTimeout(scaleSaveTimerRef.current);
    scaleSaveTimerRef.current = window.setTimeout(() => {
      endpoints
        .saveAppearance({
          custom_display_font_enabled: Boolean(next.custom_display_font_enabled),
          custom_display_font_scale_percent: normalizedCustomDisplayFontScale(
            next.custom_display_font_scale_percent
          )
        })
        .then((saved) => {
          if (scaleSaveTokenRef.current !== token) return;
          setCustomDisplayFont(saved);
          return applyCustomDisplayFont(saved);
        })
        .catch((error) => {
          if (scaleSaveTokenRef.current !== token) return;
          setAppearanceStatus(
            error instanceof Error ? error.message : 'Display font scale could not be saved.'
          );
        });
    }, 250);
  };

  const uploadCustomDisplayFont = async (file: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith('.ttf')) {
      setAppearanceStatus('Choose a .ttf display font.');
      return;
    }
    setAppearanceStatus('Uploading display font...');
    try {
      const uploaded = await endpoints.uploadDisplayFont(file);
      setCustomDisplayFont(uploaded);
      await applyCustomDisplayFont(uploaded);
      const coverageMessage = customDisplayFontCoverageMessage(uploaded);
      setAppearanceStatus(
        coverageMessage || `${uploaded.custom_display_font_name || file.name} is ready.`
      );
    } catch (error) {
      setAppearanceStatus(error instanceof Error ? error.message : 'Display font upload failed.');
    }
  };

  return {
    appearanceStatus,
    customDisplayFont,
    setTheme,
    setCustomDisplayFontEnabled,
    setCustomDisplayFontScale,
    uploadCustomDisplayFont,
    theme
  };
}
