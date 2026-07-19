import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { capabilityEnabled } from '../../shared/lib/capabilities';
import type { JsonRecord, ZoneProfile } from '../../shared/types';
import {
  loadDspTargetZoneId,
  resolveSettingsTargetZoneId,
  saveDspTargetZoneId
} from './dspTargetZone';
import { useAppearanceSettings } from './hooks/useAppearanceSettings';
import { useDataSettings } from './hooks/useDataSettings';
import { useDspSettings } from './hooks/useDspSettings';
import { useEqSettings } from './hooks/useEqSettings';
import { useMediaSettings } from './hooks/useMediaSettings';
import { useProfileSettings } from './hooks/useProfileSettings';
import { useQobuzCache } from './hooks/useQobuzCache';
import { useSettingsInitialLoad } from './hooks/useSettingsInitialLoad';
import { useZonesSettings } from './hooks/useZonesSettings';
import { AppleMusicCapturePage } from './pages/AppleMusicCapturePage';
import { DspSettingsPage } from './pages/DspSettingsPage';
import { EqSettingsPage } from './pages/EqSettingsPage';
import { GeneralSettingsPage } from './pages/GeneralSettingsPage';
import { MetaBrainzSettingsPage } from './pages/MetaBrainzSettingsPage';
import { ProfilesSettingsPage } from './pages/ProfilesSettingsPage';
import { QobuzSettingsPage } from './pages/QobuzSettingsPage';
import { RemoteAccessPage } from './pages/RemoteAccessPage';
import { ZonesSettingsPage } from './pages/ZonesSettingsPage';
import { SettingsShell } from './SettingsShell';
import {
  type ApplyProfilesResponse,
  dspSelectedDeviceDisplayName,
  isHostDeviceBrowser,
  type ProfilesResponse,
  type SettingsTabId,
  zoneDisplayName
} from './settingsModel';

export function SettingsView({
  status,
  qobuzStatus,
  zones,
  profiles,
  activeProfileId,
  activeTab,
  onRefresh,
  onProfilesChanged,
  onProfileScopedRefresh,
  selectActiveProfile
}: {
  status: JsonRecord;
  qobuzStatus: JsonRecord | null;
  zones: ZoneProfile[];
  profiles: JsonRecord[];
  activeProfileId: string;
  activeTab: SettingsTabId;
  onRefresh: () => Promise<void>;
  onProfilesChanged: ApplyProfilesResponse;
  onProfileScopedRefresh: () => Promise<void>;
  selectActiveProfile: (profileId: string) => Promise<ProfilesResponse>;
}) {
  const activeZoneId = String(status.active_zone_id || '');
  const [settingsTargetZoneId, setSettingsTargetZoneId] = useState(() =>
    activeTab === 'dsp' || activeTab === 'eq' ? '' : loadDspTargetZoneId()
  );
  const previousActiveTabRef = useRef<SettingsTabId | null>(null);
  const settingsTargetZones = useMemo(() => {
    const enabled = zones.filter((zone) => zone.enabled !== false);
    return enabled.length ? enabled : zones;
  }, [zones]);
  const selectSettingsTargetZone = useCallback((zoneId: string) => {
    saveDspTargetZoneId(zoneId);
    setSettingsTargetZoneId(zoneId);
  }, []);
  const effectiveSettingsTargetZoneId = settingsTargetZones.some(
    (zone) => zone.id === settingsTargetZoneId
  )
    ? settingsTargetZoneId
    : activeZoneId || settingsTargetZones[0]?.id || '';
  const settingsTargetZone = settingsTargetZones.find(
    (zone) => zone.id === effectiveSettingsTargetZoneId
  );
  const settingsTargetZoneName =
    effectiveSettingsTargetZoneId === activeZoneId
      ? dspSelectedDeviceDisplayName(status)
      : settingsTargetZone
        ? zoneDisplayName(settingsTargetZone)
        : dspSelectedDeviceDisplayName(status);
  const libraryManagementAvailable = status.surface !== 'remote' && isHostDeviceBrowser();
  const { clearQobuzCache, qobuzCache, reloadQobuzCache } = useQobuzCache();
  const {
    addFolder,
    folderInput,
    folderStatus,
    folders,
    isPickingFolder,
    isScanning,
    pickFolder,
    removeFolder,
    removingFolder,
    rescan,
    scanProgress,
    scanStatus,
    setFolderInput
  } = useMediaSettings(onRefresh, libraryManagementAvailable);
  const {
    applyState,
    ecBeam2Selectable,
    playbackConfig,
    playbackConfigError,
    updatePlaybackConfig
  } = useDspSettings(status, onRefresh, effectiveSettingsTargetZoneId, settingsTargetZone);
  const {
    deleteEqPreset,
    dragEqBand,
    draggingEqBand,
    eqBands,
    eqConfig,
    eqCurve,
    eqPresetName,
    eqPresets,
    loadEqPreset,
    reloadEq,
    resetEqParameters,
    saveEqPreset,
    setEqPresetName,
    startEqBandDrag,
    stopEqBandDrag,
    updateEq,
    updateEqBand
  } = useEqSettings(status, effectiveSettingsTargetZoneId);
  const {
    createProfile,
    deleteProfile,
    profileName,
    selectProfile,
    setProfileName,
    updateProfile
  } = useProfileSettings(onProfilesChanged, onProfileScopedRefresh, selectActiveProfile);
  const {
    dataStatus,
    exportHistory,
    importFile,
    importHistory,
    importMode,
    setImportFile,
    setImportMode
  } = useDataSettings();
  const { setTheme, theme } = useAppearanceSettings();
  const {
    calibrateZoneCapabilities,
    disableSettingsZone,
    openZoneSettings,
    refreshZoneHegelStatus,
    saveZoneHegelSettings,
    saveZoneSettings,
    selectSettingsZone,
    setSettingsZoneId,
    setZoneDefaultVolumeEnabled,
    setZoneDefaultVolumePercent,
    setZoneQobuzHiresEnabled,
    setZoneDeviceTypeDraft,
    setZoneHegelDraft,
    setZoneHegelSettingsOpen,
    setZoneIconDraft,
    setZoneBrowserStreamDraft,
    setZoneNameDraft,
    setZoneUpnpCapabilitiesDraft,
    settingsZone,
    zoneBrowserStreamDraft,
    zoneCalibrationBusy,
    zoneCalibrationMessage,
    zoneDeviceTypeDraft,
    zoneDefaultVolumeEnabled,
    zoneDefaultVolumePercent,
    zoneQobuzHiresEnabled,
    zoneGroups,
    zoneHegelDraft,
    zoneHegelMessage,
    zoneHegelSettingsOpen,
    zoneIconDraft,
    zoneNameDraft,
    zoneUpnpCapabilitiesDraft,
    outputSettingsZones
  } = useZonesSettings(zones, onRefresh);

  // Entering DSP/EQ starts on the active playback device. DSP and EQ share the
  // same target, so moving directly between them preserves an explicit choice.
  // The choice also remains stable across status refreshes while either is open.
  useEffect(() => {
    const targetsAudioSettings = activeTab === 'dsp' || activeTab === 'eq';
    const previouslyTargetedAudioSettings =
      previousActiveTabRef.current === 'dsp' || previousActiveTabRef.current === 'eq';
    const enteringAudioSettings = targetsAudioSettings && !previouslyTargetedAudioSettings;
    previousActiveTabRef.current = activeTab;
    if (!targetsAudioSettings) return;
    setSettingsTargetZoneId((current) =>
      resolveSettingsTargetZoneId(settingsTargetZones, activeZoneId, current, enteringAudioSettings)
    );
  }, [activeTab, activeZoneId, settingsTargetZones]);

  useSettingsInitialLoad({ reloadEq, reloadQobuzCache });

  return (
    <SettingsShell>
      {activeTab === 'general' ? (
        <GeneralSettingsPage
          addFolder={addFolder}
          clearQobuzCache={clearQobuzCache}
          dataStatus={dataStatus}
          exportHistory={exportHistory}
          folderInput={folderInput}
          folderStatus={folderStatus}
          folders={folders}
          importFile={importFile}
          importHistory={importHistory}
          importMode={importMode}
          isPickingFolder={isPickingFolder}
          isScanning={isScanning}
          libraryManagementAvailable={libraryManagementAvailable}
          pickFolder={pickFolder}
          removeFolder={removeFolder}
          removingFolder={removingFolder}
          qobuzCache={qobuzCache}
          rescan={rescan}
          scanProgress={scanProgress}
          scanStatus={scanStatus}
          setFolderInput={setFolderInput}
          setImportFile={setImportFile}
          setImportMode={setImportMode}
          setTheme={setTheme}
          theme={theme}
        />
      ) : null}

      {activeTab === 'zones' ? (
        <ZonesSettingsPage
          calibrateZoneCapabilities={calibrateZoneCapabilities}
          disableSettingsZone={disableSettingsZone}
          hegelAvailable={capabilityEnabled(status, 'hegel')}
          onRefresh={onRefresh}
          openZoneSettings={openZoneSettings}
          refreshZoneHegelStatus={refreshZoneHegelStatus}
          saveZoneHegelSettings={saveZoneHegelSettings}
          saveZoneSettings={saveZoneSettings}
          selectSettingsZone={selectSettingsZone}
          setSettingsZoneId={setSettingsZoneId}
          setZoneDefaultVolumeEnabled={setZoneDefaultVolumeEnabled}
          setZoneDefaultVolumePercent={setZoneDefaultVolumePercent}
          setZoneQobuzHiresEnabled={setZoneQobuzHiresEnabled}
          setZoneDeviceTypeDraft={setZoneDeviceTypeDraft}
          setZoneHegelDraft={setZoneHegelDraft}
          setZoneHegelSettingsOpen={setZoneHegelSettingsOpen}
          setZoneIconDraft={setZoneIconDraft}
          setZoneBrowserStreamDraft={setZoneBrowserStreamDraft}
          setZoneNameDraft={setZoneNameDraft}
          setZoneUpnpCapabilitiesDraft={setZoneUpnpCapabilitiesDraft}
          settingsZone={settingsZone}
          status={status}
          zoneBrowserStreamDraft={zoneBrowserStreamDraft}
          zoneCalibrationBusy={zoneCalibrationBusy}
          zoneCalibrationMessage={zoneCalibrationMessage}
          zoneDeviceTypeDraft={zoneDeviceTypeDraft}
          zoneDefaultVolumeEnabled={zoneDefaultVolumeEnabled}
          zoneDefaultVolumePercent={zoneDefaultVolumePercent}
          zoneQobuzHiresEnabled={zoneQobuzHiresEnabled}
          zoneGroups={zoneGroups}
          zoneHegelDraft={zoneHegelDraft}
          zoneHegelMessage={zoneHegelMessage}
          zoneHegelSettingsOpen={zoneHegelSettingsOpen}
          zoneIconDraft={zoneIconDraft}
          zoneNameDraft={zoneNameDraft}
          zoneUpnpCapabilitiesDraft={zoneUpnpCapabilitiesDraft}
          zones={outputSettingsZones}
        />
      ) : null}

      {activeTab === 'dsp' ? (
        <DspSettingsPage
          applyState={applyState}
          ecBeam2Selectable={ecBeam2Selectable}
          playbackConfig={playbackConfig}
          playbackConfigError={playbackConfigError}
          selectedDeviceName={settingsTargetZoneName}
          selectedZoneId={effectiveSettingsTargetZoneId}
          settingsZones={settingsTargetZones}
          status={status}
          updatePlaybackConfig={updatePlaybackConfig}
          onSelectedZoneChange={selectSettingsTargetZone}
        />
      ) : null}

      {activeTab === 'eq' ? (
        <EqSettingsPage
          deleteEqPreset={deleteEqPreset}
          dragEqBand={dragEqBand}
          draggingEqBand={draggingEqBand}
          eqBands={eqBands}
          eqConfig={eqConfig}
          eqCurve={eqCurve}
          eqPresetName={eqPresetName}
          eqPresets={eqPresets}
          loadEqPreset={loadEqPreset}
          resetEqParameters={resetEqParameters}
          saveEqPreset={saveEqPreset}
          selectedDeviceName={settingsTargetZoneName}
          selectedZoneId={effectiveSettingsTargetZoneId}
          settingsZones={settingsTargetZones}
          setEqPresetName={setEqPresetName}
          startEqBandDrag={startEqBandDrag}
          stopEqBandDrag={stopEqBandDrag}
          updateEq={updateEq}
          updateEqBand={updateEqBand}
          onSelectedZoneChange={selectSettingsTargetZone}
        />
      ) : null}

      {activeTab === 'qobuz' ? (
        <QobuzSettingsPage onRefresh={onRefresh} qobuzStatus={qobuzStatus} />
      ) : null}

      {activeTab === 'apple-music' ? <AppleMusicCapturePage /> : null}

      {activeTab === 'metabrainz' ? (
        <MetaBrainzSettingsPage onRefresh={onRefresh} qobuzStatus={qobuzStatus} />
      ) : null}

      {activeTab === 'remote' ? <RemoteAccessPage appStatus={status} /> : null}

      {activeTab === 'profiles' ? (
        <ProfilesSettingsPage
          activeProfileId={activeProfileId}
          createProfile={createProfile}
          deleteProfile={deleteProfile}
          profileName={profileName}
          profiles={profiles}
          selectProfile={selectProfile}
          setProfileName={setProfileName}
          updateProfile={updateProfile}
        />
      ) : null}
    </SettingsShell>
  );
}
