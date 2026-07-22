import { fileURLToPath } from 'node:url';
import { expect, test } from '@playwright/test';
import type { Locator, Page } from '@playwright/test';
import { installMockBackend, lastApiCall, waitForApiCall } from './fixtures/mockBackend';

const browserZoneId = 'browser-mobile-settings-owner';
const legacyStylesPath = fileURLToPath(new URL('../../static/styles.css', import.meta.url));

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

async function expectMobileModalGeometry(page: Page, dialog: Locator) {
  const surface = dialog.locator('.app-modal-surface').first();
  await expect(surface).toBeVisible();
  const bounds = await surface.boundingBox();
  if (!bounds) throw new Error('Expected the mobile modal surface to have layout bounds');
  const viewport = await page.evaluate(() => ({
    height: window.visualViewport?.height ?? window.innerHeight,
    width: window.visualViewport?.width ?? window.innerWidth
  }));

  expect(Math.abs(bounds.x + bounds.width / 2 - viewport.width / 2)).toBeLessThanOrEqual(1);
  expect(bounds.y).toBeGreaterThanOrEqual(0);
  expect(bounds.y).toBeLessThanOrEqual(32);
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(viewport.height + 1);
  await expect(surface).toHaveCSS('overflow-y', 'auto');
  await expect(dialog).toHaveCSS('position', 'fixed');
  await expect
    .poll(() =>
      dialog.evaluate((element) => window.getComputedStyle(element).backdropFilter)
    )
    .toContain('blur(5px)');
  const modalLayer = await dialog.evaluate((element) => {
    const miniPlayer = document.querySelector('.mobile-mini-player');
    const topBar = document.querySelector('.mobile-top-bar');
    const zIndex = (target: Element | null) =>
      target ? Number(window.getComputedStyle(target).zIndex) || 0 : 0;
    return {
      backdropFilter: window.getComputedStyle(element).backdropFilter,
      isAppLevel: element.parentElement?.matches('.react-app') ?? false,
      miniPlayerZIndex: zIndex(miniPlayer),
      modalZIndex: zIndex(element),
      topBarZIndex: zIndex(topBar)
    };
  });
  expect(modalLayer.isAppLevel).toBe(true);
  expect(modalLayer.modalZIndex).toBeGreaterThan(modalLayer.miniPlayerZIndex);
  expect(modalLayer.modalZIndex).toBeGreaterThan(modalLayer.topBarZIndex);
}

async function longPress(page: Page, target: Locator, holdDurationMs = 550) {
  const bounds = await target.boundingBox();
  if (!bounds) throw new Error('Expected the long-press target to have layout bounds');
  const pointer = {
    button: 0,
    buttons: 1,
    clientX: bounds.x + bounds.width / 2,
    clientY: bounds.y + bounds.height / 2,
    pointerId: 1,
    pointerType: 'touch'
  };

  await target.dispatchEvent('pointerdown', pointer);
  await page.waitForTimeout(holdDurationMs);
  await target.dispatchEvent('pointerup', { ...pointer, buttons: 0 });
  await target.dispatchEvent('click', {
    button: 0,
    clientX: pointer.clientX,
    clientY: pointer.clientY,
    detail: 1
  });
}

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

  await page.keyboard.press('Escape');
  await page.locator('.mobile-top-bar').getByRole('button', { name: 'Search' }).click();
  const searchDialog = page.getByRole('dialog', { name: 'Search library and Qobuz' });
  await expect(searchDialog).toBeVisible();
  await expectMobileModalGeometry(page, searchDialog);
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
  await expectMobileModalGeometry(page, dialog);
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

test('a mobile seek survives closing and reopening now playing', async ({ isMobile, page }) => {
  test.skip(!isMobile, 'This seek persistence check only applies to the mobile project.');
  await page.route('**/styles.css*', (route) =>
    route.fulfill({ path: legacyStylesPath, contentType: 'text/css' })
  );
  const backend = await installMockBackend(page);

  await page.goto('/');
  await page.getByRole('button', { name: 'Open now playing' }).click();
  let sheet = page.locator('.mobile-now-playing-sheet');
  await expect(sheet).toBeVisible();
  const playerSurfaces = await page.evaluate(() => {
    const miniPlayer = document.querySelector('.mobile-mini-player');
    const expandedPlayer = document.querySelector('.mobile-now-playing-sheet');
    if (!miniPlayer || !expandedPlayer) return null;
    const expandedStyle = window.getComputedStyle(expandedPlayer);
    return {
      expandedBackground: expandedStyle.backgroundColor,
      expandedOpeningAnimation: expandedStyle.animationName,
      miniBackground: window.getComputedStyle(miniPlayer).backgroundColor
    };
  });
  if (!playerSurfaces) throw new Error('Expected both mobile player surfaces');
  expect(playerSurfaces.expandedBackground).toBe(playerSurfaces.miniBackground);
  expect(playerSurfaces.expandedOpeningAnimation).toBe('mobile-sheet-open');
  await sheet.getByRole('button', { name: 'Queue', exact: true }).click();
  const queueHeaderBackground = await sheet
    .locator('.now-playing-queue-header')
    .evaluate((element) => window.getComputedStyle(element).backgroundColor);
  expect(queueHeaderBackground).toBe(playerSurfaces.expandedBackground);
  await sheet.getByRole('button', { name: 'Now Playing', exact: true }).click();
  const seek = sheet.getByRole('slider', { name: 'Seek' });
  const seekBounds = await seek.boundingBox();
  if (!seekBounds) throw new Error('Expected the mobile seek control to have layout bounds');
  expect(seekBounds.height).toBeGreaterThanOrEqual(44);
  const seekPointer = {
    bubbles: true,
    button: 0,
    buttons: 1,
    clientY: seekBounds.y + seekBounds.height / 2,
    isPrimary: true,
    pointerId: 7,
    pointerType: 'touch'
  };
  await seek.dispatchEvent('pointerdown', {
    ...seekPointer,
    clientX: seekBounds.x + seekBounds.width * 0.25
  });
  await seek.dispatchEvent('pointermove', {
    ...seekPointer,
    clientX: seekBounds.x + seekBounds.width * 0.7
  });
  await seek.dispatchEvent('pointerup', {
    ...seekPointer,
    buttons: 0,
    clientX: seekBounds.x + seekBounds.width * 0.7
  });
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/seek'));
  await expect
    .poll(async () => Number(await seek.inputValue()))
    .toBeGreaterThanOrEqual(Number(await seek.getAttribute('max')) * 0.69);
  const seekShell = seek.locator('..');
  await expect(seekShell).toHaveClass(/is-loading/);
  await expect(seekShell).toHaveCSS('animation-name', 'none');
  await expect(seekShell).toHaveCSS('filter', 'none');

  await sheet.getByRole('button', { name: 'Close now playing' }).click();
  await expect(sheet).toHaveClass(/is-closing/);
  expect(await sheet.evaluate((element) => window.getComputedStyle(element).transitionDuration)).toBe(
    '0.32s, 0.16s'
  );
  expect(await sheet.evaluate((element) => window.getComputedStyle(element).transitionDelay)).toBe(
    '0s, 0.14s'
  );
  await expect(sheet).toHaveCount(0);
  await page.getByRole('button', { name: 'Open now playing' }).click();
  sheet = page.locator('.mobile-now-playing-sheet');
  await expect(sheet).toBeVisible();
  await expect
    .poll(async () => Number(await sheet.getByRole('slider', { name: 'Seek' }).inputValue()))
    .toBeGreaterThanOrEqual(150);
});

test('a mobile long press selects songs and albums with compact top-bar actions', async ({
  browserName,
  isMobile,
  page
}) => {
  test.skip(!isMobile, 'This selection gesture check only applies to the mobile project.');
  await page.route('**/styles.css*', (route) =>
    route.fulfill({ path: legacyStylesPath, contentType: 'text/css' })
  );
  await installMockBackend(page, {
    albumBrowse: {
      items: [
        {
          id: 1,
          title: 'Hurry Up, We Are Dreaming',
          album_artist: 'M83',
          image_url: 'data:image/gif;base64,R0lGODlhAQABAAAAACw=',
          track_count: 3,
          confidence: 100,
          match_status: 'matched'
        }
      ],
      total: 1,
      limit: 160,
      offset: 0,
      has_more: false
    },
    trackBrowse: {
      items: [
        {
          id: 1,
          file_name: 'midnight-city.flac',
          title: 'Midnight City',
          artist: 'M83',
          album: 'Hurry Up, We Are Dreaming',
          duration_secs: 243,
          album_id: 1,
          play_count: 0,
          listened_secs: 0
        }
      ],
      total: 1,
      limit: 20,
      offset: 0,
      has_more: false
    }
  });

  await page.goto('/');
  await page.getByRole('button', { name: 'Open navigation' }).click();
  await page.getByRole('dialog', { name: 'Navigation' }).getByRole('button', {
    name: 'Albums',
    exact: true
  }).click();

  const album = page.locator('.albums-view .album-card').filter({
    hasText: 'Hurry Up, We Are Dreaming'
  });
  await expect(album).toBeVisible();
  await expect(album.locator('.album-cover-play')).toHaveCSS('display', 'none');
  if (browserName === 'webkit') {
    const image = album.locator('.album-cover img');
    await expect(image).toHaveAttribute('draggable', 'false');
    const safariImageBehavior = await image.evaluate((element) => {
      const style = window.getComputedStyle(element);
      return {
        userDrag: style.getPropertyValue('-webkit-user-drag'),
        userSelect: style.webkitUserSelect
      };
    });
    expect(safariImageBehavior).toEqual({
      userDrag: 'none',
      userSelect: 'none'
    });
  }
  await longPress(page, album, 1_500);

  await expect(album).toHaveClass(/is-selected/);
  const topBar = page.locator('.mobile-top-bar');
  await expect(topBar.getByRole('button', { name: 'Go back' })).toHaveCount(0);
  await expect(topBar.getByRole('button', { name: 'Go forward' })).toHaveCount(0);
  await expect(topBar.getByRole('button', { name: 'Play now' })).toBeVisible();
  await expect(topBar.getByRole('button', { name: 'Selected queue options' })).toBeVisible();

  const selectedCount = topBar.locator('.toolbar-selection-count');
  await expect(selectedCount).toHaveText('1 selected');
  await expect(selectedCount).toHaveCSS('position', 'absolute');
  await expect(selectedCount).toHaveCSS('clip-path', 'inset(50%)');

  const playBounds = await topBar.getByRole('button', { name: 'Play now' }).boundingBox();
  const menuBounds = await topBar
    .getByRole('button', { name: 'Selected queue options' })
    .boundingBox();
  const selectionToolbarBounds = await topBar.locator('.mobile-selection-toolbar').boundingBox();
  const selectionPlayBounds = await topBar.locator('.toolbar-selection-play').boundingBox();
  if (!playBounds || !menuBounds) throw new Error('Expected selection actions to have layout bounds');
  if (!selectionToolbarBounds || !selectionPlayBounds) {
    throw new Error('Expected the selection toolbar to have layout bounds');
  }
  expect(Math.abs(playBounds.y - menuBounds.y)).toBeLessThanOrEqual(1);
  expect(
    Math.abs(
      selectionPlayBounds.x + selectionPlayBounds.width / 2 -
        (selectionToolbarBounds.x + selectionToolbarBounds.width / 2)
    )
  ).toBeLessThanOrEqual(1);

  await topBar.getByRole('button', { name: 'Selected queue options' }).click();
  const selectionMenu = page.getByRole('menu').filter({ hasText: 'Add selected next' });
  await expect(selectionMenu).toBeVisible();
  const selectionMenuBounds = await selectionMenu.boundingBox();
  if (!selectionMenuBounds) throw new Error('Expected the selection menu to have layout bounds');
  const selectionMenuMaterial = await selectionMenu.evaluate((element) => ({
    backdropFilter: window.getComputedStyle(element).backdropFilter,
    overlay: window.getComputedStyle(element, '::before').backgroundImage
  }));
  expect(selectionMenuMaterial.backdropFilter).toContain('blur(30px)');
  expect(selectionMenuMaterial.overlay).toContain('linear-gradient');
  expect(selectionMenuBounds.x).toBeGreaterThanOrEqual(8);
  expect(selectionMenuBounds.x + selectionMenuBounds.width).toBeLessThanOrEqual(
    (await page.evaluate(() => window.innerWidth)) - 8
  );
  await topBar.getByRole('button', { name: 'Selected queue options' }).click();

  await topBar.getByRole('button', { name: 'Exit selection' }).click();
  await expect(topBar.getByRole('button', { name: 'Go back' })).toBeVisible();
  await expect(topBar.getByRole('button', { name: 'Go forward' })).toBeVisible();

  await page.getByRole('button', { name: 'Open navigation' }).click();
  await page.getByRole('dialog', { name: 'Navigation' }).getByRole('button', {
    name: 'Songs',
    exact: true
  }).click();
  const song = page.locator('.songs-view .songs-track-row').filter({ hasText: 'Midnight City' });
  await expect(song).toBeVisible();
  await longPress(page, song);

  await expect(song).toHaveClass(/is-selected/);
  await expect(topBar.getByRole('button', { name: 'Play now' })).toBeVisible();
  await expect(topBar.getByRole('button', { name: 'Go back' })).toHaveCount(0);
  await expect(topBar.getByRole('button', { name: 'Go forward' })).toHaveCount(0);
});
