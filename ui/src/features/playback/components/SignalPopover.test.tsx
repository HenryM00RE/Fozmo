// @vitest-environment jsdom

import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { endpoints } from '../../../shared/lib/api';
import { SignalPopover } from './SignalPopover';

describe('SignalPopover', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('shows EQ reported by the displayed Agent device', async () => {
    const zoneEq = vi.spyOn(endpoints, 'zoneEq').mockResolvedValue({
      enabled: false,
      preamp_db: 0,
      bands: Array.from({ length: 10 }, () => ({ enabled: false }))
    });

    render(
      <SignalPopover
        status={{
          active_zone_id: 'agent-1-wasapi-speakers',
          active_zone_name: 'Studio PC',
          zone_protocol: 'remote_agent',
          state: 'Playing',
          file_name: 'track.flac',
          source_rate: 44100,
          source_bits: 24,
          target_rate: 44100,
          target_bits: 24,
          active_output_mode: 'Pcm',
          remote_signal_path: {
            eq_enabled: true,
            eq_active_bands: 2
          }
        }}
      />
    );

    await waitFor(() => {
      expect(zoneEq).toHaveBeenCalledWith('agent-1-wasapi-speakers');
    });
    expect(await screen.findByText('Parametric EQ')).toBeInTheDocument();
    expect(screen.getByText(/2 active/)).toBeInTheDocument();
  });

  it('renders a concise DSD signal path', async () => {
    vi.spyOn(endpoints, 'zoneEq').mockResolvedValue({ enabled: false, bands: [] });

    render(
      <SignalPopover
        status={{
          active_zone_id: 'hegel-h390',
          active_zone_name: 'Hegel H390',
          state: 'Playing',
          file_name: 'track.flac',
          source_rate: 96000,
          source_bits: 24,
          target_rate: 6144000,
          target_bits: 1,
          active_output_mode: 'Dsd128',
          output_transport: 'dop_coreaudio',
          active_filter_type: 'SplitPhase128kE3',
          headroom_db: -2,
          dsd_modulator: '7th-order-search',
          dsd_limiter_peak_ratio: 0.189,
          dsd_limiter_peak_ratio_max: 0.576,
          cpu_percent: 12,
          dsp_buffer_ms: 1000,
          dsd_last_load: 0.4
        }}
      />
    );

    expect(screen.getByText('24/96.0 kHz → DSD128 (6.1440 MHz)')).toBeInTheDocument();
    expect(screen.getByText('Split Phase → DSD128')).toBeInTheDocument();
    expect(screen.getByText('7th Order Search · DoP via CoreAudio')).toBeInTheDocument();
    expect(screen.getByText('-2.0dB Headroom')).toBeInTheDocument();
    expect(screen.getByText('2.5x Realtime')).toBeInTheDocument();
    expect(screen.queryByText(/Input 18\.9%/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Buffer 1000ms/)).not.toBeInTheDocument();
    expect(screen.queryByText('12%')).not.toBeInTheDocument();

    await waitFor(() => {
      expect(endpoints.zoneEq).toHaveBeenCalledWith('hegel-h390');
    });
  });
});
