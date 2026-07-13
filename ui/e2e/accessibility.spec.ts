import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import { installMockBackend } from './fixtures/mockBackend';

test('application shell has no serious accessibility violations @smoke', async ({ page }) => {
  await installMockBackend(page);

  await page.goto('/');
  await expect(page.locator('#root .react-app')).toBeVisible();

  const results = await new AxeBuilder({ page }).analyze();
  const seriousViolations = results.violations.filter((violation) =>
    violation.impact === 'serious' || violation.impact === 'critical'
  );

  expect(seriousViolations).toEqual([]);
});
