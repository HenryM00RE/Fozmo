import { useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { ApplyProfilesResponse, ProfilesResponse } from '../settingsModel';

export function useProfileSettings(
  onProfilesChanged: ApplyProfilesResponse,
  onProfileScopedRefresh: () => Promise<void>,
  selectActiveProfile: (profileId: string) => Promise<ProfilesResponse>
) {
  const [profileName, setProfileName] = useState('');

  const selectProfile = async (profileId: string) => {
    await selectActiveProfile(profileId);
    await onProfileScopedRefresh();
  };

  const createProfile = async () => {
    const name = profileName.trim();
    if (!name) return;
    const data = await endpoints.createProfile(name);
    onProfilesChanged(data, data.active_profile_id);
    setProfileName('');
    await onProfileScopedRefresh();
  };

  const updateProfile = async (
    profileId: string,
    name: string,
    color: string,
    image?: string | null
  ) => {
    const data = await endpoints.updateProfile(profileId, name.trim(), color, image);
    onProfilesChanged(data);
    await onProfileScopedRefresh();
  };

  const deleteProfile = async (profileId: string) => {
    const data = await endpoints.deleteProfile(profileId);
    onProfilesChanged(data, data.active_profile_id);
    await onProfileScopedRefresh();
  };

  return {
    createProfile,
    deleteProfile,
    profileName,
    selectProfile,
    setProfileName,
    updateProfile
  };
}
