import { useCallback, useEffect, useRef, useState } from 'react';
import { storageKey } from '../../../shared/identity';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import type { HegelControlActions } from '../model/hegelFormModel';
import {
  type HegelFormState,
  type HegelModelId,
  hegelModels,
  numberValue,
  stringValue
} from '../settingsModel';

export function useHegelSettings() {
  const [hegelSettings, setHegelSettings] = useState<HegelFormState>({
    zoneId: '',
    linkedAirplayZoneId: '',
    host: '',
    port: 50001,
    model: 'h390',
    input: hegelModels.h390.usb,
    defaultVolume: 20,
    maxVolume: 50,
    standbyUsbVisible: false,
    volume: 0
  });
  const [hegelMessage, setHegelMessage] = useState('Enter the amp IP address, then refresh.');
  const autoSaveReadyRef = useRef(false);
  const suppressNextAutoSaveRef = useRef(false);
  const saveTimerRef = useRef<number | null>(null);

  const persistHegelSettings = useCallback(async (settings: HegelFormState) => {
    setHegelMessage('Saving setup...');
    const maxVolume = Math.max(0, Math.min(100, Math.round(settings.maxVolume)));
    const defaultVolume = Math.min(maxVolume, Math.max(0, Math.round(settings.defaultVolume)));
    const saved = await endpoints.saveHegelSettings({
      enabled: Boolean(settings.zoneId && settings.host.trim()),
      zone_id: settings.zoneId || null,
      linked_airplay_zone_id: settings.linkedAirplayZoneId || null,
      host: settings.host.trim() || null,
      port: Math.max(1, Math.min(65535, Math.round(settings.port))),
      input: Math.max(1, Math.min(20, Math.round(settings.input))),
      default_volume: defaultVolume,
      max_volume: maxVolume,
      standby_usb_visible: settings.standbyUsbVisible
    });
    setHegelMessage('Hegel setup saved');
    return saved;
  }, []);

  const reloadHegel = useCallback(() => {
    return endpoints
      .hegelSettings()
      .then((settings) => {
        const model = (localStorage.getItem(storageKey('HegelModel')) || 'h390') as HegelModelId;
        const validModel = hegelModels[model] ? model : 'h390';
        suppressNextAutoSaveRef.current = true;
        setHegelSettings((current) => ({
          ...current,
          zoneId: stringValue(settings.zone_id),
          linkedAirplayZoneId: stringValue(settings.linked_airplay_zone_id),
          host: stringValue(settings.host, current.host),
          port: numberValue(settings.port, 50001),
          input: numberValue(settings.input, hegelModels[validModel].usb),
          defaultVolume: numberValue(settings.default_volume, 20),
          maxVolume: numberValue(settings.max_volume, 50),
          standbyUsbVisible: Boolean(settings.standby_usb_visible),
          volume: Math.min(current.volume, numberValue(settings.max_volume, 50)),
          model: validModel
        }));
      })
      .catch(() => {
        autoSaveReadyRef.current = true;
      });
  }, []);

  useEffect(() => {
    if (!autoSaveReadyRef.current) return;
    if (suppressNextAutoSaveRef.current) {
      suppressNextAutoSaveRef.current = false;
      return;
    }
    if (saveTimerRef.current) window.clearTimeout(saveTimerRef.current);
    saveTimerRef.current = window.setTimeout(() => {
      persistHegelSettings(hegelSettings).catch((error) => {
        setHegelMessage(error instanceof Error ? error.message : 'Hegel setup save failed');
      });
    }, 650);
    return () => {
      if (saveTimerRef.current) {
        window.clearTimeout(saveTimerRef.current);
        saveTimerRef.current = null;
      }
    };
  }, [
    hegelSettings.defaultVolume,
    hegelSettings.host,
    hegelSettings.input,
    hegelSettings.linkedAirplayZoneId,
    hegelSettings.maxVolume,
    hegelSettings.model,
    hegelSettings.port,
    hegelSettings.standbyUsbVisible,
    hegelSettings.zoneId,
    persistHegelSettings
  ]);

  useEffect(() => {
    if (autoSaveReadyRef.current) return;
    if (!suppressNextAutoSaveRef.current) return;
    autoSaveReadyRef.current = true;
    suppressNextAutoSaveRef.current = false;
  }, [hegelSettings]);

  const hegelPayload = useCallback(() => {
    const host = hegelSettings.host.trim();
    if (!host) {
      setHegelMessage('Enter the Hegel IP address first');
      return null;
    }
    localStorage.setItem(storageKey('HegelHost'), host);
    localStorage.setItem(storageKey('HegelPort'), String(hegelSettings.port || 50001));
    return { host, port: Math.max(1, Math.min(65535, Math.round(hegelSettings.port || 50001))) };
  }, [hegelSettings.host, hegelSettings.port]);

  const runHegelAction = useCallback(
    async (action: (payload: JsonRecord) => Promise<JsonRecord>, message: string) => {
      const payload = hegelPayload();
      if (!payload) return;
      setHegelMessage('Contacting Hegel...');
      try {
        const nextStatus = await action(payload);
        setHegelMessage(message);
        if (Number.isFinite(Number(nextStatus.volume))) {
          setHegelSettings((current) => ({ ...current, volume: Number(nextStatus.volume) }));
        }
      } catch (error) {
        setHegelMessage(error instanceof Error ? error.message : 'Hegel command failed');
      }
    },
    [hegelPayload]
  );

  const hegelControls: HegelControlActions = {
    refreshStatus: () =>
      runHegelAction((payload) => endpoints.hegelStatus(payload), 'Hegel status refreshed')
  };

  return {
    hegelControls,
    hegelMessage,
    hegelSettings,
    reloadHegel,
    setHegelSettings
  };
}
