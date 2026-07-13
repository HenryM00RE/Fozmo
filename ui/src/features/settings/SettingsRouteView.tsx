import type { RouteState } from '../../shared/types';
import type { SettingsRouteState } from './model/settingsRouteState';
import { SettingsView } from './SettingsView';
import { settingsTabFromValue } from './settingsModel';

type SettingsRouteViewProps = {
  route: RouteState;
  settingsRoute: SettingsRouteState;
};

export function SettingsRouteView({ route, settingsRoute }: SettingsRouteViewProps) {
  return (
    <SettingsView
      status={settingsRoute.status}
      qobuzStatus={settingsRoute.qobuzStatus}
      zones={settingsRoute.zones}
      profiles={settingsRoute.profiles}
      activeProfileId={settingsRoute.activeProfileId}
      activeTab={settingsTabFromValue(route.id, 'general', settingsRoute.status)}
      onRefresh={settingsRoute.onRefresh}
      onProfilesChanged={settingsRoute.applyProfilesResponse}
      onProfileScopedRefresh={settingsRoute.onProfileScopedRefresh}
      selectActiveProfile={settingsRoute.selectProfile}
    />
  );
}
