import { useEffect, useMemo, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { safeArray } from '../../../shared/lib/appSupport';
import { capabilityEnabled } from '../../../shared/lib/capabilities';
import type { JsonRecord, ZoneProfile } from '../../../shared/types';
import { refreshPlaybackStatus } from '../../playback/model/playbackStore';
import {
  boolValue,
  canSyncPlaybackDspConfigFromStatus,
  configFromStatus,
  defaultDsdOutputModeForZone,
  dsdDefaultRules,
  dsdSourceRates,
  headroomAfterUpsamplingChange,
  knownFilterIds,
  numberValue,
  playbackDspConfigKey,
  playbackDspConfigMatchesStatus,
  seventhOrderSearchSelectableForDsdConfig,
  stringValue,
  visibleFilterType,
  zoneSupportsDsdOutputMode,
  zoneSupportsDsp
} from '../settingsModel';

const AUTO_APPLY_DELAY_MS = 350;
const TARGET_STATUS_POLL_MS = 3000;

function statusBelongsToZone(status: JsonRecord | null | undefined, zoneId: string) {
  return Boolean(status && zoneId && String(status.active_zone_id || '') === zoneId);
}

export type DspApplyState = {
  zoneProtocol: string;
  playState: string;
  renderStatus: string;
  restartPending: boolean;
  notice: string;
};

export function useDspSettings(
  status: JsonRecord,
  onRefresh: () => Promise<void>,
  targetZoneId = '',
  targetZone?: ZoneProfile | null
) {
  const [targetStatus, setTargetStatus] = useState<JsonRecord | null>(null);
  const targetStatusForZone = statusBelongsToZone(targetStatus, targetZoneId) ? targetStatus : null;
  const liveStatusForTarget =
    !targetZoneId || statusBelongsToZone(status, targetZoneId) ? status : null;
  const targetStatusReady = Boolean(targetStatusForZone || liveStatusForTarget);
  const statusForConfig = targetStatusForZone || liveStatusForTarget || status;
  const [playbackConfig, setPlaybackConfig] = useState(() => configFromStatus(status));
  const [playbackConfigDirty, setPlaybackConfigDirty] = useState(false);
  const [playbackConfigError, setPlaybackConfigError] = useState('');
  const [applyRetryTick, setApplyRetryTick] = useState(0);
  const latestApplyId = useRef(0);
  const lastAppliedConfigKeyRef = useRef<string | null>(null);
  const applyInFlightRef = useRef(false);
  const applyQueuedRef = useRef(false);
  const playbackConfigRef = useRef(playbackConfig);
  const onRefreshRef = useRef(onRefresh);

  const statusPlaybackConfig = useMemo(() => configFromStatus(statusForConfig), [statusForConfig]);
  const statusPlaybackConfigKey = playbackDspConfigKey(statusPlaybackConfig);
  const playbackConfigKey = playbackDspConfigKey(playbackConfig);
  const dsdRulesKey = JSON.stringify(statusForConfig.dsd_rules || null);
  const experimentalDsd256 = capabilityEnabled(statusForConfig, 'experimental_dsd256');
  const seventhOrderSearchSelectable = seventhOrderSearchSelectableForDsdConfig(
    playbackConfig.outputMode,
    playbackConfig.filterType,
    statusForConfig.dsd_rules_enabled,
    statusForConfig.dsd_rules
  );
  const dspAvailable = zoneSupportsDsp(targetZone);

  useEffect(() => {
    setPlaybackConfigDirty(false);
    setPlaybackConfigError('');
    lastAppliedConfigKeyRef.current = null;
    if (!targetZoneId) {
      setTargetStatus(null);
      return undefined;
    }
    let cancelled = false;
    setTargetStatus(null);
    const refreshTargetStatus = () => {
      endpoints
        .zoneStatus(targetZoneId)
        .then((nextStatus) => {
          if (!cancelled) setTargetStatus(nextStatus);
        })
        .catch((error) => {
          if (!cancelled) console.error('Could not load DSP settings target zone', error);
        });
    };
    refreshTargetStatus();
    // Keep the settings target bound to its zone-scoped status even when that
    // target is also the active playback zone.
    const timer = window.setInterval(refreshTargetStatus, TARGET_STATUS_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [targetZoneId]);

  useEffect(() => {
    if (!targetStatusReady) return;
    if (
      !canSyncPlaybackDspConfigFromStatus({
        dirty: playbackConfigDirty,
        localConfigKey: playbackConfigKey,
        appliedConfigKey: lastAppliedConfigKeyRef.current,
        statusConfigKey: statusPlaybackConfigKey
      })
    ) {
      return;
    }
    lastAppliedConfigKeyRef.current = null;
    setPlaybackConfigDirty(false);
    setPlaybackConfig(statusPlaybackConfig);
  }, [
    playbackConfigDirty,
    playbackConfigKey,
    statusPlaybackConfig,
    statusPlaybackConfigKey,
    targetStatusReady
  ]);

  useEffect(() => {
    if (!targetStatusReady || dspAvailable) return;
    if (
      !playbackConfig.upsamplingEnabled &&
      playbackConfig.headroomDb === 0 &&
      playbackConfig.outputMode === 'Pcm' &&
      playbackConfig.dsdIsiPenalty === 0 &&
      playbackConfig.dspBufferMs === 0
    ) {
      return;
    }
    setPlaybackConfigDirty(true);
    setPlaybackConfigError('');
    lastAppliedConfigKeyRef.current = null;
    setPlaybackConfig((current) => ({
      ...current,
      upsamplingEnabled: false,
      exclusive: false,
      headroomDb: 0,
      dspBufferMs: 0,
      outputMode: 'Pcm',
      dsdIsiPenalty: 0
    }));
  }, [dspAvailable, playbackConfig, targetStatusReady]);

  useEffect(() => {
    playbackConfigRef.current = playbackConfig;
  }, [playbackConfig, playbackConfigKey]);

  useEffect(() => {
    onRefreshRef.current = onRefresh;
  }, [onRefresh]);

  const updatePlaybackConfig = <K extends keyof typeof playbackConfig>(
    key: K,
    value: (typeof playbackConfig)[K]
  ) => {
    setPlaybackConfigDirty(true);
    setPlaybackConfigError('');
    lastAppliedConfigKeyRef.current = null;
    setPlaybackConfig((current) => {
      const next = {
        ...current,
        [key]: value
      };
      if (key === 'upsamplingEnabled' && typeof value === 'boolean') {
        next.headroomDb = headroomAfterUpsamplingChange(
          current.headroomDb,
          value,
          current.dsdModulator
        );
      }
      return next;
    });
  };

  const normalizePlaybackDsdRules = () => {
    const rulesByRate = new Map(
      safeArray<JsonRecord>(statusForConfig.dsd_rules).map((rule) => [
        numberValue(rule.source_rate),
        rule
      ])
    );
    return dsdSourceRates.map((sourceRate) => {
      const rule = rulesByRate.get(sourceRate);
      const fallback = dsdDefaultRules.find((candidate) => candidate.source_rate === sourceRate);
      const filterType = stringValue(rule?.filter_type, fallback?.filter_type);
      const migratedFilterType = visibleFilterType(filterType);
      const savedOutputMode = stringValue(rule?.output_mode, fallback?.output_mode || 'Dsd128');
      const preferredOutputMode =
        savedOutputMode === 'Dsd256' && experimentalDsd256
          ? 'Dsd256'
          : savedOutputMode === 'Dsd64'
            ? 'Dsd64'
            : 'Dsd128';
      const outputMode = zoneSupportsDsdOutputMode(
        targetZone,
        preferredOutputMode,
        experimentalDsd256
      )
        ? preferredOutputMode
        : defaultDsdOutputModeForZone(targetZone, experimentalDsd256);
      return {
        source_rate: sourceRate,
        filter_type: knownFilterIds.has(migratedFilterType)
          ? migratedFilterType
          : fallback?.filter_type || 'MinimumPhaseCompact128k',
        output_mode: outputMode
      };
    });
  };

  useEffect(() => {
    if (!playbackConfigDirty) return;
    if (!targetStatusReady) return;
    if (lastAppliedConfigKeyRef.current === playbackConfigKey) return;
    const applyKey = playbackConfigKey;
    const config = playbackConfig;
    const timer = window.setTimeout(() => {
      if (applyInFlightRef.current) {
        applyQueuedRef.current = true;
        return;
      }
      applyInFlightRef.current = true;
      const applyId = latestApplyId.current + 1;
      latestApplyId.current = applyId;
      const dsdRulesEnabled = boolValue(statusForConfig.dsd_rules_enabled, false);
      void (async () => {
        try {
          const sanitizedOutputMode = zoneSupportsDsdOutputMode(
            targetZone,
            config.outputMode,
            experimentalDsd256
          )
            ? config.outputMode
            : 'Pcm';
          const nextDsdRulesEnabled = dsdRulesEnabled && sanitizedOutputMode !== 'Pcm';
          const nextConfig = {
            filter_type: config.filterType,
            target_rate: config.targetRate,
            target_bit_depth: config.targetBitDepth,
            upsampling_enabled: config.upsamplingEnabled,
            exclusive: config.exclusive,
            headroom_db: config.headroomDb,
            dsp_buffer_ms: config.dspBufferMs,
            output_mode: sanitizedOutputMode,
            dsd_modulator: config.dsdModulator,
            dsd_isi_penalty: config.dsdIsiPenalty,
            dsd_rules_enabled: nextDsdRulesEnabled,
            dsd_rules: nextDsdRulesEnabled ? normalizePlaybackDsdRules() : []
          };
          if (targetZoneId) {
            await endpoints.updateZoneConfig(targetZoneId, nextConfig);
            if (
              latestApplyId.current === applyId &&
              playbackDspConfigKey(playbackConfigRef.current) === applyKey
            ) {
              lastAppliedConfigKeyRef.current = applyKey;
            }
            const nextTargetStatus = await endpoints.zoneStatus(targetZoneId);
            setTargetStatus(nextTargetStatus);
            if (playbackDspConfigMatchesStatus(config, nextTargetStatus)) {
              setPlaybackConfigError('');
            }
          } else {
            await endpoints.updateConfig(nextConfig);
            if (
              latestApplyId.current === applyId &&
              playbackDspConfigKey(playbackConfigRef.current) === applyKey
            ) {
              lastAppliedConfigKeyRef.current = applyKey;
            }
          }
          await refreshPlaybackStatus({ force: true });
          await onRefreshRef.current();
        } catch (error) {
          console.error('Could not auto-apply DSP settings', error);
          if (
            latestApplyId.current === applyId &&
            playbackDspConfigKey(playbackConfigRef.current) === applyKey
          ) {
            lastAppliedConfigKeyRef.current = null;
            setPlaybackConfigDirty(false);
            setPlaybackConfigError(
              error instanceof Error ? error.message : 'DSP settings could not be saved.'
            );
          }
        } finally {
          applyInFlightRef.current = false;
          if (applyQueuedRef.current) {
            applyQueuedRef.current = false;
            setApplyRetryTick((tick) => tick + 1);
          }
        }
      })();
    }, AUTO_APPLY_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [
    playbackConfigDirty,
    playbackConfig,
    playbackConfigKey,
    statusForConfig.dsd_rules_enabled,
    experimentalDsd256,
    targetZone,
    targetZoneId,
    targetStatusReady,
    dsdRulesKey,
    applyRetryTick
  ]);

  const applyState: DspApplyState = {
    zoneProtocol: stringValue(statusForConfig.zone_protocol, ''),
    playState: stringValue(statusForConfig.state, ''),
    renderStatus: stringValue(statusForConfig.upnp_render_status, ''),
    restartPending: boolValue(statusForConfig.upnp_restart_pending, false),
    notice: stringValue(statusForConfig.output_notice, '')
  };

  return {
    applyState,
    seventhOrderSearchSelectable,
    playbackConfig,
    playbackConfigError,
    updatePlaybackConfig
  };
}
