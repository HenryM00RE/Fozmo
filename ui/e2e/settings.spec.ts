import { expect, test } from '@playwright/test';
import { fixtures, installMockBackend, lastApiCall, waitForApiCall } from './fixtures/mockBackend';

test('DSP settings auto-apply changed playback config', async ({ page }) => {
  const backend = await installMockBackend(page);

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'DSP' }).click();
  await page.getByRole('button', { name: 'Upsampling / DSP enabled' }).click();

  const configPath = '/api/zones/local-core/config';
  await waitForApiCall(page, backend.calls, (call) => call.path === configPath);
  expect(lastApiCall(backend.calls, (call) => call.path === configPath)?.body).toMatchObject({
    upsampling_enabled: false
  });
});

test('filter selections persist their canonical setting names', async ({ page }) => {
  const backend = await installMockBackend(page);

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'DSP' }).click();

  const configPath = '/api/zones/local-core/config';
  for (const [label, filterType] of [
    ['Linear Phase', 'LinearPhase128k'],
    ['Minimum Phase', 'MinimumPhaseCompact128kV2'],
    ['Minimum Phase B', 'MinimumPhaseCompact128k'],
    ['Smooth Phase', 'SmoothPhase128k'],
    ['Split Phase', 'Split128k'],
    ['Split Phase B', 'Split128kV2']
  ]) {
    await page.getByRole('button', { name: 'Filter' }).click();
    await page.getByRole('option', { name: label, exact: true }).click();
    await waitForApiCall(
      page,
      backend.calls,
      (call) =>
        call.path === configPath &&
        (call.body as Record<string, unknown> | null)?.filter_type === filterType
    );
    expect(
      lastApiCall(
        backend.calls,
        (call) =>
          call.path === configPath &&
          (call.body as Record<string, unknown> | null)?.filter_type === filterType
      )?.body
    ).toMatchObject({ filter_type: filterType });
  }
});

test('Qobuz service shows logged-out connect state @smoke', async ({
  page
}) => {
  const loggedOut = await installMockBackend(page, { qobuzStatus: fixtures.qobuzLoggedOut });

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'Services' }).click();
  await expect(page.getByText('Not connected').first()).toBeVisible();
  await page.getByRole('button', { name: 'Qobuz settings' }).click();
  await expect(page.getByRole('link', { name: 'Connect' })).toHaveAttribute(
    'href',
    '/api/qobuz/oauth/start'
  );
  await expect(loggedOut.calls.some((call) => call.path === '/api/qobuz/status')).toBeTruthy();
});

test('Qobuz sign-out success updates account state', async ({ page }) => {
  const backend = await installMockBackend(page, { qobuzStatus: fixtures.qobuzLoggedIn });

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'Services' }).click();
  await expect(page.getByText(/Connected as Casey Listener/)).toBeVisible();
  await page.getByRole('button', { name: 'Qobuz settings' }).click();
  await page.getByRole('button', { name: 'Sign out' }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path === '/api/qobuz/logout');
  await expect(page.getByText('Not connected').first()).toBeVisible();
});

test('Qobuz sign-out failure is surfaced', async ({ page }) => {
  const backend = await installMockBackend(page, {
    qobuzStatus: fixtures.qobuzLoggedIn,
    failures: {
      'POST /api/qobuz/logout': {
        status: 500,
        body: 'Logout exploded'
      }
    }
  });

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'Services' }).click();
  await page.getByRole('button', { name: 'Qobuz settings' }).click();
  await page.getByRole('button', { name: 'Sign out' }).click();
  await waitForApiCall(page, backend.calls, (call) => call.path === '/api/qobuz/logout');
  await expect(page.getByTestId('qobuz-service-message')).toContainText('Qobuz sign out failed');
});
