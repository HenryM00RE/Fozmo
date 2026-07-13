import type { JsonRecord } from '../../../shared/types';
import { numberValue, stringValue } from '../settingsModel';

export const qobuzRadioDefaultMessage = 'Qobuz Radio starts automatically when the queue finishes.';
export const qobuzRadioSavingMessage = 'Saving...';
export const qobuzRadioEnabledMessage = 'Qobuz Radio will start when the queue finishes.';
export const qobuzRadioDisabledMessage = 'Qobuz Radio is disabled.';
export const qobuzRadioSaveFailedMessage = 'Could not save Qobuz Radio setting.';

export function qobuzAccountStatusLabel(qobuzStatus: JsonRecord | null) {
  return qobuzStatus?.logged_in || qobuzStatus?.authenticated ? 'Connected' : 'Not connected';
}

export function qobuzAccountIdentity(qobuzStatus: JsonRecord | null) {
  const user = qobuzStatus?.user as JsonRecord | undefined;
  return stringValue(user?.display_name, stringValue(user?.email, 'Not signed in'));
}

export function qobuzAccountSummary(qobuzStatus: JsonRecord | null) {
  if (!(qobuzStatus?.logged_in || qobuzStatus?.authenticated)) {
    return 'Not connected';
  }
  const user = qobuzStatus.user as JsonRecord | undefined;
  const identity = qobuzAccountIdentity(qobuzStatus);
  const subscription = stringValue(user?.subscription_label);
  return subscription ? `Connected as ${identity} • ${subscription}` : `Connected as ${identity}`;
}

export function qobuzRadioEnabledFromStatus(qobuzStatus: JsonRecord | null) {
  return qobuzStatus?.radio_enabled !== false;
}

export function qobuzRadioSavedMessage(enabled: boolean) {
  return enabled ? qobuzRadioEnabledMessage : qobuzRadioDisabledMessage;
}

export function qobuzCacheSummary(qobuzCache: JsonRecord | null) {
  if (!qobuzCache) return 'Calculating...';
  const fileCount = numberValue(qobuzCache.files, numberValue(qobuzCache.count, 0));
  const byteCount = numberValue(qobuzCache.bytes);
  return `${fileCount} files${byteCount ? `, ${Math.round(byteCount / 1024 / 1024)} MB` : ''}`;
}

export function qobuzSettingsErrorMessage(error: unknown, fallback: string) {
  return error instanceof Error ? error.message : fallback;
}
