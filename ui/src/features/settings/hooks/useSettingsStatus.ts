import { useMemo } from 'react';
import type { JsonRecord } from '../../../shared/types';

export function useSettingsStatus(status: JsonRecord) {
  const dsdRulesKey = JSON.stringify(status.dsd_rules || null);

  return useMemo(
    () => ({
      active_filter_type: status.active_filter_type,
      active_zone_id: status.active_zone_id,
      active_zone_name: status.active_zone_name,
      configured_target_rate: status.configured_target_rate,
      dither_mode: status.dither_mode,
      dsd_isi_penalty: status.dsd_isi_penalty,
      dsd_modulator: status.dsd_modulator,
      dsd_rules: status.dsd_rules,
      dsd_rules_enabled: status.dsd_rules_enabled,
      dsp_buffer_ms: status.dsp_buffer_ms,
      exclusive: status.exclusive,
      filter_type: status.filter_type,
      headroom_db: status.headroom_db,
      output_mode: status.output_mode,
      selected_device: status.selected_device,
      source_rate: status.source_rate,
      surface: status.surface,
      state: status.state,
      target_rate: status.target_rate,
      upsampling_enabled: status.upsampling_enabled,
      capabilities: status.capabilities
    }),
    [
      status.active_filter_type,
      status.active_zone_id,
      status.active_zone_name,
      status.configured_target_rate,
      status.dither_mode,
      status.dsd_isi_penalty,
      status.dsd_modulator,
      dsdRulesKey,
      status.dsd_rules_enabled,
      status.dsp_buffer_ms,
      status.exclusive,
      status.filter_type,
      status.headroom_db,
      status.output_mode,
      status.selected_device,
      status.source_rate,
      status.surface,
      status.state,
      status.target_rate,
      status.upsampling_enabled,
      status.capabilities
    ]
  );
}
