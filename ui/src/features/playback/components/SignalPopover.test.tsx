// @vitest-environment jsdom

import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { endpoints } from '../../../shared/lib/api';
import { SignalPopover } from './SignalPopover';

describe('SignalPopover', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('loads EQ for the displayed remote zone', async () => {
    const zoneEq = vi.spyOn(endpoints, 'zoneEq').mockResolvedValue({
      enabled: true,
      preamp_db: 0,
      bands: Array.from({ length: 10 }, (_, index) => ({ enabled: index === 0 }))
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
          active_output_mode: 'Pcm'
        }}
      />
    );

    await waitFor(() => {
      expect(zoneEq).toHaveBeenCalledWith('agent-1-wasapi-speakers');
    });
    expect(await screen.findByText('Parametric EQ')).toBeInTheDocument();
    expect(screen.getByText(/1 active/)).toBeInTheDocument();
  });
});
