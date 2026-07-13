import { expect, test } from '@playwright/test';
import { installMockBackend, lastApiCall, waitForApiCall } from './fixtures/mockBackend';

test('playback controls issue protected commands @smoke', async ({ page }) => {
  const backend = await installMockBackend(page);

  await page.goto('/');

  await expect(page.getByText('Midnight City').first()).toBeVisible();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/now-playing-queue'));
  const controls = page.getByRole('region', { name: 'Playback controls' });
  await expect(controls.getByRole('button', { name: 'Pause' })).toBeVisible();

  await controls.getByRole('button', { name: 'Pause' }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/pause'));
  await expect(controls.getByRole('button', { name: 'Pause' })).toBeEnabled();

  await controls.getByRole('button', { name: 'Next', exact: true }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/play'));
  await expect(controls.getByRole('button', { name: 'Next', exact: true })).toBeEnabled();

  await controls.getByRole('button', { name: 'Loop' }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/loop-mode'));
  await expect(controls.getByRole('button', { name: 'Loop on' })).toBeEnabled();
  expect(lastApiCall(backend.calls, (call) => call.path.endsWith('/loop-mode'))?.body).toMatchObject(
    {
      mode: 'loop'
    }
  );

  await controls.getByRole('button', { name: 'Shuffle upcoming', exact: true }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/queue'));

  const seek = controls.getByLabel('Seek');
  const seekCallCount = () => backend.calls.filter((call) => call.path.endsWith('/seek')).length;

  const keyboardSeekCalls = seekCallCount();
  await seek.focus();
  await seek.press('End');
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/seek'));
  await expect.poll(seekCallCount).toBe(keyboardSeekCalls + 1);

  const pointerSeekCalls = seekCallCount();
  await seek.evaluate((element) => {
    const input = element as HTMLInputElement;
    input.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    input.value = '120';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new PointerEvent('pointerup', { bubbles: true }));
    // Touch browsers may emit this compatibility event after pointerup. It
    // must not commit the same scrub a second time.
    input.dispatchEvent(new TouchEvent('touchend', { bubbles: true }));
  });
  await expect.poll(seekCallCount).toBe(pointerSeekCalls + 1);
});

test('websocket close failures update transport connection state', async ({ page }) => {
  await installMockBackend(page, {
    websocketMode: 'close'
  });

  await page.goto('/');

  await expect(page.getByTestId('player-bar')).toHaveAttribute(
    'data-playback-connection',
    /connecting|disconnected/
  );
});

test('selected remote-zone polling survives frequent global websocket frames', async ({ page }) => {
  const remoteZone = {
    id: 'remote-zone',
    name: 'Remote iPhone',
    protocol: 'remote_agent',
    backend: 'remote_agent',
    capabilities: {
      max_sample_rate: 48000,
      max_bit_depth: 24,
      exclusive_supported: false,
      gapless_supported: true
    },
    dsp_profile: {
      upsampling_enabled: false,
      filter_type: 'SincBest',
      target_rate: 48000,
      dither_mode: 'Auto'
    },
    status: 'available',
    enabled: true
  };
  const backend = await installMockBackend(page, {
    zones: [remoteZone],
    websocketIntervalMs: 40,
    zoneStatusDelayMs: 150,
    zoneStatus: {
      state: 'Playing',
      active_zone_id: remoteZone.id,
      active_zone_name: remoteZone.name,
      file_name: 'Artist - First Song',
      track_title: 'First Song',
      track_artist: 'Artist',
      position_secs: 5,
      duration_secs: 180
    }
  });
  await page.addInitScript(() => {
    localStorage.setItem('fozmoSelectedPlaybackZone', 'remote-zone');
  });

  await page.goto('/');
  await expect(page.getByText('First Song').first()).toBeVisible();

  backend.setZoneStatus({
    file_name: 'Artist - Second Song',
    track_title: 'Second Song',
    position_secs: 0
  });

  await expect(page.getByText('Second Song').first()).toBeVisible({ timeout: 3000 });
});
