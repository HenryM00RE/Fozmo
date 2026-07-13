import type { JsonRecord } from '../../../shared/types';
import type { ApplyProfilesResponse, ProfilesResponse } from '../settingsModel';

export type ProfileShellState = {
  activeProfileId: string;
  applyProfilesResponse: ApplyProfilesResponse;
  profiles: JsonRecord[];
  refreshCore: () => Promise<void>;
  refreshProfileScopedData: () => Promise<void>;
  selectProfile: (profileId: string) => Promise<ProfilesResponse>;
};
