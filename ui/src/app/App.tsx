import { useEffect, useMemo, useState } from 'react';
import {
  BROWSER_ZONE_REGISTERED_EVENT,
  initBrowserZoneAgent,
  setBrowserZoneStreamPrefs
} from '../features/browserZone/browserZoneAgent';
import { useHistoryData } from '../features/history/hooks/useHistoryData';
import { useRecentlyPlayedData } from '../features/home/hooks/useRecentlyPlayedData';
import { useLibraryData } from '../features/library/hooks/useLibraryData';
import { usePlaylistsData } from '../features/playlists/hooks/usePlaylistsData';
import { useQobuzHome } from '../features/qobuz/hooks/useQobuzHome';
import { useGlobalSearch } from '../features/search/hooks/useGlobalSearch';
import { useAppearanceSettings } from '../features/settings/hooks/useAppearanceSettings';
import { useSettingsStatus } from '../features/settings/hooks/useSettingsStatus';
import { useSettingsSupport } from '../features/settings/hooks/useSettingsSupport';
import { isOwnBrowserZoneId } from '../shared/lib/browserZone';
import { filterZonesByCapabilities } from '../shared/lib/capabilities';
import { AppChrome } from './AppChrome';
import { AppRoutes } from './AppRoutes';
import { AppShell } from './AppShell';
import {
  buildLibraryRoute,
  buildPlaybackChrome,
  buildProfileShell,
  buildSearchChrome,
  buildSettingsRoute
} from './appComposition';
import { FirstRunGuide } from './FirstRunGuide';
import { useAppNavigation } from './hooks/useAppNavigation';
import { useAppNotices } from './hooks/useAppNotices';
import { useAppPlaybackActions } from './hooks/useAppPlaybackActions';
import { useAppPlaylistWorkflows } from './hooks/useAppPlaylistWorkflows';
import { useAppRefresh } from './hooks/useAppRefresh';
import { useAppSelections } from './hooks/useAppSelections';
import { useProfileScopedRefresh } from './hooks/useProfileScopedRefresh';
import { isLoopbackHostname, useRemoteLinkExchange } from './hooks/useRemoteLinkExchange';
import { useAppChromeEffects } from './useAppChromeEffects';
import { useAppRouteEffects } from './useAppRouteEffects';

export default function App() {
  const { customDisplayFont } = useAppearanceSettings();
  const { notice, noticeKey, setNotice, setToolbarAction, toolbarAction } = useAppNotices();
  const { authMessage, authState, retryRemoteAuth } = useRemoteLinkExchange(setNotice);

  if (authState !== 'authorised') {
    return (
      <RemoteAuthRequiredPage
        authState={authState}
        message={authMessage}
        onRetry={retryRemoteAuth}
      />
    );
  }

  return (
    <AuthenticatedApp
      notice={notice}
      noticeKey={noticeKey}
      setNotice={setNotice}
      setToolbarAction={setToolbarAction}
      toolbarAction={toolbarAction}
      customDisplayFont={customDisplayFont}
    />
  );
}

function AuthenticatedApp({
  notice,
  noticeKey,
  setNotice,
  setToolbarAction,
  toolbarAction,
  customDisplayFont
}: {
  notice: string;
  noticeKey: number;
  setNotice: (message: string) => void;
  setToolbarAction: ReturnType<typeof useAppNotices>['setToolbarAction'];
  toolbarAction: ReturnType<typeof useAppNotices>['toolbarAction'];
  customDisplayFont: ReturnType<typeof useAppearanceSettings>['customDisplayFont'];
}) {
  const [nowPlayingOpen, setNowPlayingOpen] = useState(false);
  const [signalOpen, setSignalOpen] = useState(false);
  const { navigate, openArtistName, route } = useAppNavigation({ setNowPlayingOpen });
  const { albums, artists, refreshLibraryData, tracks } = useLibraryData();
  const {
    historyStats,
    historyStatsLoading,
    recentHistory,
    recentHistoryLoading,
    refreshHistoryStats,
    refreshRecentHistory
  } = useHistoryData();
  const { recentAlbums, recentPlaylists, recentlyPlayedLoading, refreshRecentlyPlayed } =
    useRecentlyPlayedData();
  const { playlists, refreshPlaylists, setPlaylists } = usePlaylistsData();
  const {
    qobuzHome,
    qobuzStatus,
    qobuzStatusLoaded,
    refreshQobuzHomeHighlight,
    refreshQobuzOverview
  } = useQobuzHome();
  const {
    activeProfileId,
    applyProfilesResponse,
    profiles,
    refreshSettingsSupport,
    selectProfile,
    zones
  } = useSettingsSupport();
  const globalSearch = useGlobalSearch(activeProfileId, profiles);
  const {
    activeZoneId,
    addItemsToQueue,
    clearQueue,
    playAlbum,
    playItems,
    playQobuzTrack,
    playSingleTrack,
    playbackZones,
    queue,
    queueGlobalSearchAlbum,
    queueGlobalSearchTrack,
    routePlaybackActions,
    selectZone,
    shuffleQueue,
    status,
    toggleLoop
  } = useAppPlaybackActions({
    albums,
    refreshRecentlyPlayed,
    setNotice,
    setSignalOpen,
    tracks,
    zones
  });
  const visibleZones = useMemo(
    () => filterZonesByCapabilities(playbackZones, status),
    [playbackZones, status]
  );
  const settingsStatus = useSettingsStatus(status);
  const { loading, refreshCore } = useAppRefresh({
    refreshHistoryStats,
    refreshLibraryData,
    refreshPlaylists,
    refreshQobuzHomeHighlight,
    refreshQobuzOverview,
    refreshRecentHistory,
    refreshRecentlyPlayed,
    refreshSettingsSupport,
    route,
    setNotice
  });
  const refreshProfileScopedData = useProfileScopedRefresh({
    refreshHistoryStats,
    refreshLibraryData,
    refreshRecentHistory,
    refreshRecentlyPlayed
  });
  const { openPlaylistPickerForItems, playlistChrome, playlistRoute, playlistShell } =
    useAppPlaylistWorkflows({
      addItemsToQueue,
      navigate,
      playItems,
      playlists,
      refreshCore,
      setNotice,
      setPlaylists,
      tracks
    });
  const {
    activeSelectionType,
    albumSelectionActive,
    albumTrackSelection,
    clearAlbumTrackSelection,
    clearRecentSelection,
    homeRoute,
    recentSelectionActive,
    selectionToolbar
  } = useAppSelections({
    addItemsToQueue,
    albums,
    navigate,
    openPlaylistPickerForItems,
    playAlbum,
    playItems,
    playlists,
    recentAlbums,
    recentlyPlayedLoading,
    recentPlaylists,
    setNotice
  });

  const libraryRoute = buildLibraryRoute({
    albums,
    artists,
    historyStats,
    historyStatsLoading,
    recentHistory,
    recentHistoryLoading,
    tracks
  });
  const settingsRoute = buildSettingsRoute({
    activeProfileId,
    applyProfilesResponse,
    profiles,
    qobuzStatus,
    refreshCore,
    refreshProfileScopedData,
    selectProfile,
    settingsStatus,
    zones: visibleZones
  });
  useEffect(() => {
    const refreshBrowserZone = () => {
      refreshSettingsSupport().catch(() => undefined);
      window.setTimeout(() => {
        refreshSettingsSupport().catch(() => undefined);
      }, 250);
    };
    window.addEventListener(BROWSER_ZONE_REGISTERED_EVENT, refreshBrowserZone);
    return () => {
      window.removeEventListener(BROWSER_ZONE_REGISTERED_EVENT, refreshBrowserZone);
    };
  }, [refreshSettingsSupport]);
  useEffect(() => {
    // The Remote Access surface affects auth/routing only; browser playback
    // quality follows the per-device output preference, not the surface.
    initBrowserZoneAgent();
  }, []);
  useEffect(() => {
    // Keep this browser's playback agent in sync with its zone's saved
    // FLAC/Opus stream choice so stream URLs reflect the output settings.
    const ownZone = zones.find((zone) => isOwnBrowserZoneId(zone.id));
    const saved = (ownZone?.browser_stream || null) as {
      format?: string;
      opus_kbps?: number;
    } | null;
    setBrowserZoneStreamPrefs(
      saved
        ? {
            format: saved.format === 'opus' ? 'opus' : 'flac',
            opusKbps: Number(saved.opus_kbps) || 256
          }
        : null
    );
  }, [zones]);
  const playbackChrome = buildPlaybackChrome({
    activeZoneId,
    albums,
    clearQueue,
    navigate,
    nowPlayingOpen,
    onSelectZone: selectZone,
    queue,
    setNowPlayingOpen,
    setSignalOpen,
    shuffleQueue,
    signalOpen,
    status,
    toggleLoop,
    zones: visibleZones
  });
  const searchChrome = buildSearchChrome({
    albums,
    globalSearch,
    navigate,
    openPlaylistPickerForItems,
    openArtistName,
    playQobuzTrack,
    playSingleTrack,
    queueGlobalSearchAlbum,
    queueGlobalSearchTrack
  });
  const profileShell = buildProfileShell({
    activeProfileId,
    applyProfilesResponse,
    profiles,
    refreshCore,
    refreshProfileScopedData,
    selectProfile
  });

  useAppChromeEffects({
    activeSelectionType,
    albumSelectionActive,
    globalSearchSetOpen: globalSearch.setOpen,
    nowPlayingOpen,
    recentSelectionActive,
    setSignalOpen,
    signalOpen
  });

  useAppRouteEffects({
    albumSelectionActive,
    clearAlbumTrackSelection,
    clearRecentSelection,
    recentSelectionActive,
    route,
    setToolbarAction
  });

  const view = (
    <AppRoutes
      route={route}
      loading={loading}
      qobuzHome={qobuzHome}
      navigate={navigate}
      openArtistName={openArtistName}
      setNotice={setNotice}
      playbackActions={routePlaybackActions}
      playbackStatus={status}
      albumTrackSelection={albumTrackSelection}
      homeRoute={homeRoute}
      libraryRoute={libraryRoute}
      playlistRoute={playlistRoute}
      settingsRoute={settingsRoute}
      customDisplayFont={customDisplayFont}
    />
  );

  return (
    <AppShell
      globalSearchOpen={globalSearch.open}
      notice={notice}
      noticeKey={noticeKey}
      onNavigate={navigate}
      onNotice={setNotice}
      onOpenSearch={() => globalSearch.setOpen(true)}
      playlistShell={playlistShell}
      profileShell={profileShell}
      route={route}
      selectionToolbar={selectionToolbar}
      status={status}
      toolbarAction={toolbarAction}
      chrome={
        <AppChrome
          playbackChrome={playbackChrome}
          playlistChrome={playlistChrome}
          searchChrome={searchChrome}
        />
      }
    >
      {view}
      <FirstRunGuide
        onNavigate={navigate}
        qobuzStatus={qobuzStatus}
        qobuzStatusLoaded={qobuzStatusLoaded}
      />
    </AppShell>
  );
}

function RemoteAuthRequiredPage({
  authState,
  message,
  onRetry
}: {
  authState: ReturnType<typeof useRemoteLinkExchange>['authState'];
  message: string;
  onRetry: () => void;
}) {
  const lanAuthentication =
    window.location.protocol === 'http:' && !isLoopbackHostname(window.location.hostname);
  const linking = authState === 'linking';
  const checking = authState === 'checking';
  const exchangeFailed = authState === 'exchange_failed';
  const title = checking
    ? 'Checking access'
    : linking
      ? 'Linking this device'
      : authState === 'unauthorised'
        ? 'Authorisation required'
        : 'Link this device';
  const detail =
    message ||
    (checking
      ? 'Checking whether this browser is already linked.'
      : linking
        ? 'Checking the remote link code for this browser.'
        : 'This Fozmo server is reachable, but this browser is not linked yet.');
  const instructions = lanAuthentication
    ? [
        'Open the Fozmo menu on the server Mac.',
        'Choose Pair a Device.',
        'Scan the QR code or open the generated link on this device.'
      ]
    : [
        'Open Fozmo on the local/LAN app.',
        'Go to Settings - Remote Access.',
        'Enable Remote Access if needed.',
        'Generate a link code or scan the QR code.',
        'Open the generated link on this device.'
      ];

  return (
    <main className="react-app remote-auth-page">
      <section className="remote-auth-panel" aria-busy={linking || checking}>
        <div className="remote-auth-kicker">
          {lanAuthentication ? 'LAN Authentication' : 'Remote Access'}
        </div>
        <h1>{title}</h1>
        <p className={exchangeFailed ? 'remote-auth-error' : undefined}>{detail}</p>
        <ol>
          {instructions.map((instruction) => (
            <li key={instruction}>{instruction}</li>
          ))}
        </ol>
        <div className="remote-auth-actions">
          <button className="pill primary" type="button" onClick={onRetry} disabled={linking}>
            {linking || checking ? 'Checking...' : 'Retry'}
          </button>
          <button className="pill" type="button" onClick={() => window.location.reload()}>
            Reload
          </button>
        </div>
      </section>
    </main>
  );
}
