import { expect, test } from '@playwright/test';
import { installMockBackend, lastApiCall, waitForApiCall } from './fixtures/mockBackend';

const browserZoneId = 'browser-mobile-settings-owner';

const browserZone = {
  id: browserZoneId,
  name: 'Safari on iPhone',
  protocol: 'remote_agent',
  browser: true,
  enabled: true,
  status: 'available',
  capabilities: {
    max_sample_rate: 48000,
    max_bit_depth: 24,
    exclusive_supported: false,
    gapless_supported: true
  },
  dsp_profile: {
    upsampling_enabled: false,
    filter_type: 'LinearPhase128k',
    target_rate: 48000,
    dither_mode: 'Off'
  },
  browser_stream: { format: 'flac', opus_kbps: 256 }
};

test('mobile shell exposes its primary navigation and now-playing controls @smoke', async ({
  isMobile,
  page
}) => {
  test.skip(!isMobile, 'This responsive-shell check only applies to the mobile project.');
  await installMockBackend(page);

  await page.goto('/');

  await expect(page.getByRole('button', { name: 'Open navigation' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Open now playing' })).toBeVisible();

  await page.getByRole('button', { name: 'Open navigation' }).click();
  await expect(page.getByRole('dialog', { name: 'Navigation' })).toBeVisible();
});

test('this browser can save and reopen its private stream settings on mobile', async ({
  isMobile,
  page
}) => {
  test.skip(!isMobile, 'This responsive settings check only applies to the mobile project.');
  await page.addInitScript((zoneId) => {
    window.localStorage.setItem('fozmoBrowserZoneAgentId', zoneId);
  }, browserZoneId);
  const backend = await installMockBackend(page, {
    zones: [
      browserZone,
      {
        ...browserZone,
        id: 'browser-someone-else',
        name: 'Chrome on Android'
      }
    ]
  });

  await page.goto('/');
  await page.getByRole('button', { name: 'Open navigation' }).click();
  await page.getByRole('button', { name: 'Settings', exact: true }).click();
  await page.getByRole('button', { name: 'Outputs', exact: true }).click();

  await expect(page.getByRole('main').getByText('Safari', { exact: true })).toBeVisible();
  await expect(page.getByRole('main').getByText('Chrome', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Settings for Safari' }).click();

  const dialog = page.getByRole('dialog', { name: 'Safari' });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Browser stream format' }).click();
  await page.getByRole('option', { name: 'Opus', exact: true }).click();
  await dialog.getByRole('button', { name: 'Opus bitrate' }).click();
  await expect(page.getByRole('option', { name: '128 kbps' })).toBeVisible();
  await expect(page.getByRole('option', { name: '256 kbps' })).toBeVisible();
  await page.getByRole('option', { name: '320 kbps' }).click();
  await dialog.getByRole('button', { name: 'Save' }).click();

  const settingsPath = `/api/zones/${browserZoneId}/settings`;
  await waitForApiCall(page, backend.calls, (call) => call.path === settingsPath);
  expect(lastApiCall(backend.calls, (call) => call.path === settingsPath)?.body).toEqual({
    icon: 'auto',
    browser_stream: { format: 'opus', opus_kbps: 320 }
  });

  await page.reload();
  await page.getByRole('button', { name: 'Settings for Safari' }).click();
  await expect(page.getByRole('button', { name: 'Browser stream format' })).toContainText('Opus');
  await expect(page.getByRole('button', { name: 'Opus bitrate' })).toContainText('320 kbps');
});
