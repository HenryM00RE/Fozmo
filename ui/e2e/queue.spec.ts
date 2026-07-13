import { expect, test } from '@playwright/test';
import { installMockBackend, waitForApiCall } from './fixtures/mockBackend';

test('queue renders and persists remove, jump, and reorder actions @smoke', async ({ page }) => {
  const backend = await installMockBackend(page);

  await page.goto('/');
  await page.getByRole('button', { name: 'Open now playing' }).click();

  await expect(page.getByRole('heading', { name: /Queue/ })).toBeVisible();
  await expect(page.getByTestId('queue-row')).toHaveCount(3);
  await expect(page.getByTestId('queue-row').nth(1)).toContainText('Reunion');

  await page.getByTestId('queue-row').nth(2).evaluate((element) => {
    element.setAttribute('draggable', 'true');
  });
  const source = await page.getByTestId('queue-row').nth(2).elementHandle();
  const target = await page.getByTestId('queue-row').nth(1).elementHandle();
  await page.evaluate((element) => {
    const dataTransfer = new DataTransfer();
    element.dispatchEvent(
      new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer })
    );
  }, source);
  await page.waitForTimeout(50);
  await page.evaluate((elements) => {
    const [source, target] = elements;
    if (!source || !target) return;
    const dataTransfer = new DataTransfer();
    const rect = target.getBoundingClientRect();
    target.dispatchEvent(
      new DragEvent('dragover', {
        bubbles: true,
        cancelable: true,
        clientY: rect.top + 1,
        dataTransfer
      })
    );
    target.dispatchEvent(
      new DragEvent('drop', {
        bubbles: true,
        cancelable: true,
        clientY: rect.top + 1,
        dataTransfer
      })
    );
    source.dispatchEvent(new DragEvent('dragend', { bubbles: true, cancelable: true, dataTransfer }));
  }, [source, target]);
  await waitForApiCall(
    page,
    backend.calls,
    (call) =>
      call.path.endsWith('/queue') &&
      JSON.stringify(call.body).includes('steve-mcqueen.flac')
  );

  await page.getByTestId('queue-row').nth(1).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/play'));

  await page.getByTestId('queue-remove').nth(2).click();
  await waitForApiCall(page, backend.calls, (call) => call.path.endsWith('/queue'));
});
