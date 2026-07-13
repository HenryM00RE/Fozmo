import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import type { ApplyProfilesResponse, ProfilesResponse } from '../settingsModel';

export type SettingsRouteState = {
  activeProfileId: string;
  applyProfilesResponse: ApplyProfilesResponse;
  onRefresh: () => Promise<void>;
  onProfileScopedRefresh: () => Promise<void>;
  profiles: JsonRecord[];
  qobuzStatus: JsonRecord | null;
  selectProfile: (profileId: string) => Promise<ProfilesResponse>;
  status: JsonRecord;
  zones: ZoneProfile[];
};
