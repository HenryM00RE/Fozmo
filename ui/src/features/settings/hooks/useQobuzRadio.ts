import { useCallback, useEffect, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import {
  qobuzRadioDefaultMessage,
  qobuzRadioEnabledFromStatus,
  qobuzRadioSavedMessage,
  qobuzRadioSaveFailedMessage,
  qobuzRadioSavingMessage,
  qobuzSettingsErrorMessage
} from '../model/qobuzSettingsModel';

export function useQobuzRadio(qobuzStatus: JsonRecord | null) {
  const [radioEnabled, setRadioEnabled] = useState(true);
  const [radioMessage, setRadioMessage] = useState(qobuzRadioDefaultMessage);

  useEffect(() => {
    setRadioEnabled(qobuzRadioEnabledFromStatus(qobuzStatus));
  }, [qobuzStatus]);

  const saveRadioEnabled = useCallback(async (enabled: boolean) => {
    setRadioEnabled(enabled);
    setRadioMessage(qobuzRadioSavingMessage);
    try {
      const saved = await endpoints.saveQobuzSettings({ radio_enabled: enabled });
      const savedEnabled = qobuzRadioEnabledFromStatus(saved);
      setRadioEnabled(savedEnabled);
      setRadioMessage(qobuzRadioSavedMessage(enabled));
    } catch (error) {
      setRadioEnabled(!enabled);
      setRadioMessage(qobuzSettingsErrorMessage(error, qobuzRadioSaveFailedMessage));
    }
  }, []);

  return {
    radioEnabled,
    radioMessage,
    saveRadioEnabled
  };
}
