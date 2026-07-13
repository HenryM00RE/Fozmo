import { expect, test } from '@playwright/test';
import { installMockBackend } from './fixtures/mockBackend';

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
