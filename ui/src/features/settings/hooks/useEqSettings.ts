import {
  type PointerEvent as ReactPointerEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState
} from 'react';
import { endpoints } from '../../../shared/lib/api';
import { safeArray } from '../../../shared/lib/appSupport';
import type { JsonRecord } from '../../../shared/types';
import {
  buildEqCurve,
  clampNumber,
  createNewEqPresetConfig,
  EQ_DB_RANGE,
  EQ_PLOT_HEIGHT,
  EQ_PLOT_PAD_X,
  EQ_PLOT_WIDTH,
  numberValue
} from '../settingsModel';

export function useEqSettings(status: JsonRecord, targetZoneId = '') {
  const [targetStatus, setTargetStatus] = useState<JsonRecord | null>(null);
  const statusForEq = targetZoneId ? targetStatus || status : status;
  const [eqConfig, setEqConfig] = useState<JsonRecord | null>(null);
  const [eqPresets, setEqPresets] = useState<JsonRecord[]>([]);
  const [eqPresetName, setEqPresetName] = useState('');
  const eqSaveTimerRef = useRef<number | null>(null);
  const eqLoadGenerationRef = useRef(0);
  const activeEqDragBandRef = useRef<number | null>(null);
  const [draggingEqBand, setDraggingEqBand] = useState<number | null>(null);

  // EQ reads/writes are always zone-scoped when a target zone is known. The
  // `status` passed in is the *selected* zone's status, so comparing the
  // target against `status.active_zone_id` is not a reliable way to detect
  // the server's active zone: a selected browser zone reports itself as the
  // active zone there while the server-wide active zone is a different one,
  // and the global `/api/eq` route would silently edit that other zone.
  const reloadEq = useCallback(() => {
    const loadGeneration = ++eqLoadGenerationRef.current;
    const configRequest = targetZoneId ? endpoints.zoneEq(targetZoneId) : endpoints.eq();
    const statusRequest = targetZoneId ? endpoints.zoneStatus(targetZoneId) : Promise.resolve(null);
    return Promise.allSettled([configRequest, endpoints.eqPresets(), statusRequest]).then(
      ([config, presets, targetZoneStatus]) => {
        if (loadGeneration !== eqLoadGenerationRef.current) return;
        if (config.status === 'fulfilled') setEqConfig(config.value);
        if (presets.status === 'fulfilled') setEqPresets(presets.value);
        if (targetZoneStatus.status === 'fulfilled') setTargetStatus(targetZoneStatus.value);
      }
    );
  }, [targetZoneId]);

  const eqBands = safeArray<JsonRecord>(eqConfig?.bands);
  const activeOutputMode = String(statusForEq.active_output_mode ?? statusForEq.output_mode ?? '');
  const eqSampleRate =
    activeOutputMode === 'Dsd128' || activeOutputMode === 'Dsd256'
      ? numberValue(statusForEq.source_rate, 44100)
      : numberValue(statusForEq.target_rate, 192000);
  const eqCurve = useMemo(() => buildEqCurve(eqConfig, eqSampleRate), [eqConfig, eqSampleRate]);

  useEffect(() => {
    ++eqLoadGenerationRef.current;
    setEqConfig(null);
    setEqPresetName('');
    setTargetStatus(null);
    void reloadEq();
  }, [reloadEq]);

  const scheduleEqSave = (nextConfig: JsonRecord) => {
    if (eqSaveTimerRef.current) window.clearTimeout(eqSaveTimerRef.current);
    const saveZoneId = targetZoneId;
    eqSaveTimerRef.current = window.setTimeout(() => {
      eqSaveTimerRef.current = null;
      const save = saveZoneId
        ? endpoints.setZoneEq(saveZoneId, nextConfig)
        : endpoints.setEq(nextConfig);
      save.catch(() => undefined);
    }, 50);
  };

  const commitEqConfig = (updater: (current: JsonRecord) => JsonRecord) => {
    setEqConfig((current) => {
      if (!current) return current;
      const next = updater(current);
      scheduleEqSave(next);
      return next;
    });
  };

  const updateEq = <K extends keyof JsonRecord>(key: K, value: JsonRecord[K]) => {
    commitEqConfig((current) => ({ ...current, [key]: value }));
  };

  const updateEqBand = (index: number, patch: JsonRecord) => {
    commitEqConfig((current) => {
      const bands = safeArray<JsonRecord>(current.bands).map((band, bandIndex) =>
        bandIndex === index ? { ...band, ...patch } : band
      );
      return { ...current, bands };
    });
  };

  const loadEqPreset = async (name: string) => {
    setEqPresetName(name);
    if (!name) {
      resetEqParameters();
      return;
    }
    const preset = await endpoints.eqPreset(name);
    setEqConfig(preset);
  };

  const resetEqParameters = () => {
    const nextConfig = createNewEqPresetConfig();
    setEqConfig(nextConfig);
    scheduleEqSave(nextConfig);
  };

  const saveEqPreset = async () => {
    const name = eqPresetName.trim();
    if (!name || !eqConfig) return;
    await endpoints.saveEqPreset({ ...eqConfig, name });
    await reloadEq();
  };

  const deleteEqPreset = async () => {
    const name = eqPresetName.trim();
    if (!name) return;
    await endpoints.deleteEqPreset(name);
    setEqPresetName('');
    await reloadEq();
  };

  const dragEqBand = (event: ReactPointerEvent<SVGSVGElement>) => {
    const bandIndex = activeEqDragBandRef.current;
    if (bandIndex === null) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const x = clampNumber(
      ((event.clientX - rect.left) / rect.width) * EQ_PLOT_WIDTH,
      0,
      EQ_PLOT_WIDTH
    );
    const y = clampNumber(
      ((event.clientY - rect.top) / rect.height) * EQ_PLOT_HEIGHT,
      0,
      EQ_PLOT_HEIGHT
    );
    const usable = EQ_PLOT_WIDTH - EQ_PLOT_PAD_X * 2;
    const xClamped = clampNumber(x, EQ_PLOT_PAD_X, EQ_PLOT_WIDTH - EQ_PLOT_PAD_X);
    const logVal =
      Math.log10(20) + ((xClamped - EQ_PLOT_PAD_X) / usable) * (Math.log10(20000) - Math.log10(20));
    const freq = clampNumber(Math.round(10 ** logVal), 20, 20000);
    const gain = clampNumber(
      Math.round(((EQ_PLOT_HEIGHT / 2 - y) / (EQ_PLOT_HEIGHT / 2 - 6)) * EQ_DB_RANGE * 10) / 10,
      -EQ_DB_RANGE,
      EQ_DB_RANGE
    );
    const band = safeArray<JsonRecord>(eqConfig?.bands)[bandIndex];
    const type = String(band?.type || 'peaking');
    const patch: JsonRecord = { freq_hz: freq };
    if (!['low_pass', 'high_pass', 'notch', 'all_pass'].includes(type)) patch.gain_db = gain;
    updateEqBand(bandIndex, patch);
  };

  const startEqBandDrag = (index: number, event: ReactPointerEvent<SVGCircleElement>) => {
    activeEqDragBandRef.current = index;
    setDraggingEqBand(index);
    event.currentTarget.ownerSVGElement?.setPointerCapture(event.pointerId);
    const band = eqBands[index];
    if (band && !band.enabled) updateEqBand(index, { enabled: true });
    event.preventDefault();
  };

  const stopEqBandDrag = (event: ReactPointerEvent<SVGSVGElement>) => {
    if (activeEqDragBandRef.current === null) return;
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      // Pointer capture may already be gone if the drag ended outside the SVG.
    }
    activeEqDragBandRef.current = null;
    setDraggingEqBand(null);
  };

  useEffect(
    () => () => {
      if (eqSaveTimerRef.current) window.clearTimeout(eqSaveTimerRef.current);
    },
    []
  );

  return {
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
  };
}
