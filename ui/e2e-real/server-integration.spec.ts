import { expect, test, type BrowserContext } from '@playwright/test';

const remotePort = Number(process.env.FOZMO_REAL_E2E_REMOTE_PORT || 4189);
const localPort = Number(process.env.FOZMO_REAL_E2E_PORT || 4188);
const remoteBaseUrl = `https://127.0.0.1:${remotePort}`;

async function pair(context: BrowserContext) {
  const start = await context.request.post('/api/pairing/start');
  expect(start.ok()).toBeTruthy();
  const pairing = (await start.json()) as { token: string };
  const exchange = await context.request.post('/api/sessions/browser', {
    data: { pairing_token: pairing.token }
  });
  expect(exchange.ok()).toBeTruthy();
  await expect
    .poll(async () => (await context.cookies()).some((cookie) => cookie.name.includes('control')))
    .toBe(true);
}

test('pairing cookie gates API and authenticated WebSockets', async ({ browser }) => {
  const context = await browser.newContext();
  expect((await context.request.get('/api/status')).status()).toBe(401);

  const unauthenticatedPage = await context.newPage();
  await unauthenticatedPage.goto('/styles.css');
  const unauthenticatedReceivedStatus = await unauthenticatedPage.evaluate(
    () =>
      new Promise<boolean>((resolve) => {
        const socket = new WebSocket(`${location.origin.replace('http', 'ws')}/api/ws`);
        const timeout = window.setTimeout(() => {
          socket.close();
          resolve(false);
        }, 7_000);
        socket.addEventListener('open', () => socket.send('{"type":"not-auth"}'));
        socket.addEventListener('message', () => {
          window.clearTimeout(timeout);
          socket.close();
          resolve(true);
        });
        socket.addEventListener('close', () => {
          window.clearTimeout(timeout);
          resolve(false);
        });
        socket.addEventListener('error', () => {
          window.clearTimeout(timeout);
          resolve(false);
        });
      })
  );
  expect(unauthenticatedReceivedStatus).toBe(false);
  await unauthenticatedPage.close();

  await pair(context);
  expect((await context.request.get('/api/status')).status()).toBe(200);

  const page = await context.newPage();
  await page.goto('/');
  const sameOriginOpened = await page.evaluate(
    () =>
      new Promise<boolean>((resolve) => {
        const socket = new WebSocket(`${location.origin.replace('http', 'ws')}/api/ws`);
        socket.addEventListener('open', () => {
          socket.close();
          resolve(true);
        });
        socket.addEventListener('error', () => resolve(false));
      })
  );
  expect(sameOriginOpened).toBe(true);

  await page.goto('data:text/html,<title>foreign origin</title>');
  const foreignOriginOpened = await page.evaluate(
    (url) =>
      new Promise<boolean>((resolve) => {
        const socket = new WebSocket(url);
        socket.addEventListener('open', () => {
          socket.close();
          resolve(true);
        });
        socket.addEventListener('error', () => resolve(false));
      }),
    `ws://127.0.0.1:${localPort}/api/ws`
  );
  expect(foreignOriginOpened).toBe(false);
  await context.close();
});

test('two browser contexts keep profile data isolated', async ({ browser }) => {
  const aliceContext = await browser.newContext();
  const bobContext = await browser.newContext();
  await pair(aliceContext);
  await pair(bobContext);

  const aliceCreate = await aliceContext.request.post('/api/profiles', { data: { name: 'Alice' } });
  const bobCreate = await bobContext.request.post('/api/profiles', { data: { name: 'Bob' } });
  expect(aliceCreate.ok()).toBeTruthy();
  expect(bobCreate.ok()).toBeTruthy();
  const alice = (await aliceCreate.json()) as { active_profile_id: string };
  const bob = (await bobCreate.json()) as { active_profile_id: string };
  expect(alice.active_profile_id).not.toBe(bob.active_profile_id);

  const aliceHeaders = { 'x-fozmo-profile-id': alice.active_profile_id };
  const bobHeaders = { 'x-fozmo-profile-id': bob.active_profile_id };
  await aliceContext.request.put(`/api/profiles/${alice.active_profile_id}/recent-searches`, {
    headers: aliceHeaders,
    data: { searches: ['alice-only'] }
  });
  await bobContext.request.put(`/api/profiles/${bob.active_profile_id}/recent-searches`, {
    headers: bobHeaders,
    data: { searches: ['bob-only'] }
  });

  const aliceSearches = await (
    await aliceContext.request.get(`/api/profiles/${alice.active_profile_id}/recent-searches`, {
      headers: aliceHeaders
    })
  ).json();
  const bobSearches = await (
    await bobContext.request.get(`/api/profiles/${bob.active_profile_id}/recent-searches`, {
      headers: bobHeaders
    })
  ).json();
  expect(aliceSearches).toMatchObject({ searches: ['alice-only'] });
  expect(bobSearches).toMatchObject({ searches: ['bob-only'] });

  await aliceContext.close();
  await bobContext.close();
});

test('real upload limits, settings recovery, and database migration are observable', async ({
  browser
}) => {
  const context = await browser.newContext();
  await pair(context);

  expect((await context.request.get('/api/appearance')).status()).toBe(200);
  expect((await context.request.get('/api/library/summary')).status()).toBe(200);

  const upload = await context.request.post('/api/upload', {
    multipart: {
      file: { name: '../../browser-suite.flac', mimeType: 'audio/flac', buffer: Buffer.from('fLaC') }
    }
  });
  expect(upload.status()).toBe(201);

  const oversizedFont = await context.request.post('/api/appearance/display-font', {
    multipart: {
      font: {
        name: 'oversized.ttf',
        mimeType: 'font/ttf',
        buffer: Buffer.alloc(8 * 1024 * 1024 + 2048)
      }
    }
  });
  expect(oversizedFont.status()).toBe(413);
  await context.close();
});

test('remote listener has a separate route surface and strict security headers', async ({
  browser
}) => {
  const local = await browser.newContext();
  await pair(local);
  const enabled = await local.request.post('/api/remote/settings', {
    data: { enabled: true, port: remotePort }
  });
  expect(enabled.ok()).toBeTruthy();

  const link = await local.request.post('/api/remote/link-code');
  const { code } = (await link.json()) as { code: string };
  const remote = await browser.newContext({ ignoreHTTPSErrors: true, baseURL: remoteBaseUrl });
  expect((await remote.request.get('/api/status')).status()).toBe(401);
  expect((await remote.request.post('/api/pairing/start')).status()).toBe(404);
  expect((await remote.request.get('/api/remote/settings')).status()).toBe(404);

  const exchange = await remote.request.post('/api/remote/session', { data: { code } });
  expect(exchange.ok()).toBeTruthy();
  const status = await remote.request.get('/api/status', {
    headers: { Origin: 'https://evil.test' }
  });
  expect(status.status()).toBe(200);
  expect(status.headers()['x-content-type-options']).toBe('nosniff');
  expect(status.headers()['referrer-policy']).toBe('no-referrer');
  expect(status.headers()['content-security-policy']).toContain("default-src 'self'");
  expect(status.headers()['access-control-allow-origin']).toBeUndefined();

  const remotePage = await remote.newPage();
  await remotePage.goto('/');
  const remoteSocketAuthenticated = await remotePage.evaluate(
    () =>
      new Promise<boolean>((resolve) => {
        const socket = new WebSocket(`${location.origin.replace('https', 'wss')}/api/ws`);
        socket.addEventListener('message', () => {
          socket.close();
          resolve(true);
        });
        socket.addEventListener('error', () => resolve(false));
      })
  );
  expect(remoteSocketAuthenticated).toBe(true);

  await remote.close();
  await local.close();
});
