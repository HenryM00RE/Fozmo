import { defineConfig, devices } from '@playwright/test';

const localPort = Number(process.env.FOZMO_REAL_E2E_PORT || 4188);

export default defineConfig({
  testDir: './e2e-real',
  timeout: 45_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? [['dot'], ['html', { open: 'never' }]] : [['list']],
  use: {
    ...devices['Desktop Chrome'],
    baseURL: `http://127.0.0.1:${localPort}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure'
  },
  projects: [{ name: 'real-chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run build && ../tools/run-real-browser-server.sh',
    url: `http://127.0.0.1:${localPort}/healthz`,
    reuseExistingServer: false,
    timeout: 180_000
  }
});
