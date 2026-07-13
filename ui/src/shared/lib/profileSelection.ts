import { storageKey } from '../identity';

const SELECTED_PROFILE_STORAGE_KEY = storageKey('SelectedListeningProfile');

export function storedProfileId() {
  try {
    return normalizeProfileId(localStorage.getItem(SELECTED_PROFILE_STORAGE_KEY));
  } catch {
    return '';
  }
}

export function rememberProfileId(profileId: string) {
  const normalized = normalizeProfileId(profileId);
  if (!normalized) return;
  try {
    localStorage.setItem(SELECTED_PROFILE_STORAGE_KEY, normalized);
  } catch {
    // Local storage is an enhancement; the in-memory profile still works.
  }
}

export function forgetProfileId(profileId?: string) {
  try {
    const normalized = normalizeProfileId(profileId);
    if (normalized && storedProfileId() !== normalized) return;
    localStorage.removeItem(SELECTED_PROFILE_STORAGE_KEY);
  } catch {
    // Local storage is an enhancement; the in-memory profile still works.
  }
}

function normalizeProfileId(profileId: unknown) {
  return String(profileId || '').trim();
}
