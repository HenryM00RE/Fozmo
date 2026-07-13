import { expect, test } from '@playwright/test';
import { installMockBackend, waitForApiCall } from './fixtures/mockBackend';

test('playback command errors show user-visible notices', async ({ page }) => {
  const backend = await installMockBackend(page, {
    queueState: {
      kind: 'local',
      cursor: 0,
      loopMode: 'off',
      items: [
        {
          title: 'Midnight City',
          artist: 'M83',
          album: 'Hurry Up, We Are Dreaming',
          durationSecs: 243,
          filename: 'midnight-city.flac',
          ref: { track_id: 1, file_name: 'midnight-city.flac' }
        }
      ]
    },
    failures: {
      'POST /api/zones/local-core/next': {
        status: 409,
        body: 'Playback request is stale'
      }
    }
  });

  await page.goto('/');
  await page.getByRole('button', { name: 'Next' }).click();

  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/next'));
  await expect(page.getByTestId('app-notice')).toContainText('Playback request is stale');
});

test('network failures on queue updates show user-visible notices', async ({ page }) => {
  const backend = await installMockBackend(page, {
    failures: {
      'POST /api/zones/local-core/queue': {
        status: 500,
        body: 'Queue update failed'
      }
    }
  });

  await page.goto('/');
  await page.getByRole('button', { name: 'Open now playing' }).click();
  await page.getByTestId('queue-remove').nth(2).click();

  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/queue'));
  await expect(page.getByTestId('app-notice')).toContainText('Queue update failed');
});
