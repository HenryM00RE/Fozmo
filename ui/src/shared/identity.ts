export const APP_SLUG = 'fozmo';
export const APP_DISPLAY_NAME = 'Fozmo';
export const ENV_PREFIX = 'FOZMO';
export const AUTH_HEADER = 'x-fozmo-token';
export const PLAYBACK_CLIENT_HEADER = 'x-fozmo-playback-client';
export const PLAYBACK_SEQUENCE_HEADER = 'x-fozmo-playback-seq';
export const LOCAL_STORAGE_PREFIX = 'fozmo';
export const DATA_DIR_NAME = 'Fozmo';
export const SCHEMA_BASE_URL = 'https://fozmo.local/schemas';
export const SCHEMA_ENDPOINTS_EXTENSION = 'x-fozmo-endpoints';
export const USER_AGENT = 'Fozmo/0.0.2';

export function storageKey(suffix: string) {
  return `${LOCAL_STORAGE_PREFIX}${suffix}`;
}

export function colonStorageKey(suffix: string) {
  return `${LOCAL_STORAGE_PREFIX}:${suffix}`;
}
