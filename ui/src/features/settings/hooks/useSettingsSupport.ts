import { useCallback, useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  forgetProfileId,
  rememberProfileId,
  storedProfileId
} from '../../../shared/lib/profileSelection';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import {
  type ApplyProfilesResponse,
  loadSettingsSupportData,
  type ProfilesResponse,
  profilesStateFromResponse
} from '../settingsModel';

export function useSettingsSupport() {
  const [zones, setZones] = useState<ZoneProfile[]>([]);
  const [profiles, setProfiles] = useState<JsonRecord[]>([]);
  const [activeProfileId, setActiveProfileId] = useState(() => storedProfileId() || 'default');
  const [serverActiveProfileId, setServerActiveProfileId] = useState('');
  const selectedProfileIdRef = useRef(storedProfileId());
  const restoreRequestProfileRef = useRef('');

  const applyProfilesResponse = useCallback<ApplyProfilesResponse>(
    (data: ProfilesResponse, preferredProfileId?: string) => {
      const nextProfiles = profilesStateFromResponse(data);
      setProfiles(nextProfiles.profiles);
      setServerActiveProfileId(nextProfiles.activeProfileId);
      const nextActiveProfileId = preferredProfile(
        nextProfiles.profiles,
        preferredProfileId || selectedProfileIdRef.current || storedProfileId(),
        nextProfiles.activeProfileId
      );
      setActiveProfileId(nextActiveProfileId);
      selectedProfileIdRef.current = nextActiveProfileId;
      if (nextActiveProfileId) rememberProfileId(nextActiveProfileId);
      else forgetProfileId();
    },
    []
  );

  useEffect(() => {
    if (!activeProfileId) return;
    if (
      profiles.length > 0 &&
      !profiles.some((profile) => String(profile.id || '') === activeProfileId)
    )
      return;
    if (serverActiveProfileId === activeProfileId) {
      restoreRequestProfileRef.current = '';
      return;
    }
    if (restoreRequestProfileRef.current === activeProfileId) return;
    restoreRequestProfileRef.current = activeProfileId;
    endpoints
      .selectProfile(activeProfileId)
      .then((data) => {
        if (restoreRequestProfileRef.current === activeProfileId) {
          applyProfilesResponse(data, activeProfileId);
        }
      })
      .catch(() => {
        if (restoreRequestProfileRef.current === activeProfileId) {
          restoreRequestProfileRef.current = '';
        }
      });
  }, [activeProfileId, applyProfilesResponse, profiles, serverActiveProfileId]);

  const selectProfile = useCallback(
    async (profileId: string): Promise<ProfilesResponse> => {
      const nextProfileId = String(profileId || '').trim();
      if (!nextProfileId) {
        return { profiles, active_profile_id: activeProfileId };
      }
      const previousProfileId = selectedProfileIdRef.current || activeProfileId;
      selectedProfileIdRef.current = nextProfileId;
      rememberProfileId(nextProfileId);
      setActiveProfileId(nextProfileId);
      try {
        const data = await endpoints.selectProfile(nextProfileId);
        applyProfilesResponse(data, nextProfileId);
        return data;
      } catch (error) {
        selectedProfileIdRef.current = previousProfileId;
        if (previousProfileId) rememberProfileId(previousProfileId);
        else forgetProfileId();
        setActiveProfileId(previousProfileId || 'default');
        throw error;
      }
    },
    [activeProfileId, applyProfilesResponse, profiles]
  );

  const refreshSettingsSupport = useCallback(async () => {
    const support = await loadSettingsSupportData();
    if (support.zones) setZones(support.zones);
    if (support.profilesResponse) applyProfilesResponse(support.profilesResponse);
  }, [applyProfilesResponse]);

  return {
    activeProfileId,
    applyProfilesResponse,
    profiles,
    refreshSettingsSupport,
    selectProfile,
    zones
  };
}

function preferredProfile(
  profiles: JsonRecord[],
  preferredProfileId: string,
  fallbackProfileId: string
) {
  const profileIds = new Set(profiles.map((profile) => String(profile.id || '')).filter(Boolean));
  if (preferredProfileId && (profileIds.size === 0 || profileIds.has(preferredProfileId)))
    return preferredProfileId;
  if (fallbackProfileId && (profileIds.size === 0 || profileIds.has(fallbackProfileId)))
    return fallbackProfileId;
  return String(profiles[0]?.id || 'default');
}
