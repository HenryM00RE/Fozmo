import type { JsonRecord, QueueItem, ZoneProfile } from '../../../shared/types';
import type { ApplyProfilesResponse, ProfilesResponse } from '../settingsModel';

export type SettingsRouteState = {
  activeProfileId: string;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => Promise<boolean>;
  applyProfilesResponse: ApplyProfilesResponse;
  onRefresh: () => Promise<void>;
  onProfileScopedRefresh: () => Promise<void>;
  profiles: JsonRecord[];
  qobuzStatus: JsonRecord | null;
  selectProfile: (profileId: string) => Promise<ProfilesResponse>;
  status: JsonRecord;
  zones: ZoneProfile[];
};
