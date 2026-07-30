import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { type Locator, type Page, expect, test } from '@playwright/test';
import { fixtures, installMockBackend, lastApiCall, waitForApiCall } from './fixtures/mockBackend';

/* The app chrome lives in static/styles.css, which index.html loads from the
   backend. The mock suite has no backend, so anything asserting layout has to
   serve that stylesheet itself or it measures an unstyled shell. */
const chromeStylesheet = readFileSync(
  resolve(dirname(fileURLToPath(import.meta.url)), '../../static/styles.css'),
  'utf8'
);

async function serveChromeStylesheet(page: Page) {
  await page.route('**/styles.css*', (route) =>
    route.fulfill({ contentType: 'text/css', body: chromeStylesheet })
  );
}

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

test('settings content scrolls under the topbar and reveals its divider', async ({ page }) => {
  await installMockBackend(page);
  await serveChromeStylesheet(page);

  await page.setViewportSize({ width: 1280, height: 700 });
  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'DSP' }).click();

  const settingsView = page.locator('.settings-view');
  const settingsContent = page.locator('.settings-content');
  const toolbar = page.locator('.app-toolbar');

  // The view is the scroller, and it no longer dissolves its own top edge:
  // content passes under an opaque bar instead of being erased into the canvas.
  const viewStyles = await settingsView.evaluate((element) => {
    const styles = getComputedStyle(element);
    return { maskImage: styles.maskImage, overflowY: styles.overflowY };
  });
  expect(viewStyles.overflowY).toBe('auto');
  expect(viewStyles.maskImage).toBe('none');
  await expect(settingsContent).toHaveCSS('overflow-y', 'visible');

  // The divider stands in for that fade, and only once there is content behind
  // the bar to separate from.
  await expect(toolbar).toHaveAttribute('data-scrolled', 'false');
  // Polled rather than read once: this spec injects the chrome stylesheet over
  // the network, so it lands after first paint and WebKit runs the divider's
  // opacity transition from its initial value on that recalc. A backend-served
  // <link> blocks rendering, so the settled value is what ships.
  await expect.poll(() => dividerOpacity(toolbar)).toBe('0');

  await settingsView.evaluate((element) => {
    element.scrollTop = 240;
  });
  await expect(toolbar).toHaveAttribute('data-scrolled', 'true');
  await expect.poll(() => dividerOpacity(toolbar)).toBe('1');

  await settingsView.evaluate((element) => {
    element.scrollTop = 0;
  });
  await expect(toolbar).toHaveAttribute('data-scrolled', 'false');
});

function dividerOpacity(toolbar: Locator) {
  return toolbar.evaluate((element) => getComputedStyle(element, '::after').opacity);
}

test('filter selections persist their canonical setting names', async ({ page }) => {
  const backend = await installMockBackend(page);

  await page.goto('/');
  await page.locator('.sidebar-settings-bottom').click();
  await page.getByRole('button', { name: 'DSP' }).click();

  const configPath = '/api/zones/local-core/config';
  for (const [label, filterType] of [
    ['Linear Phase', 'LinearPhase128k'],
    ['Minimum Phase', 'MinimumPhaseCompact128k'],
    ['Split Phase', 'SplitPhase128kE3']
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
