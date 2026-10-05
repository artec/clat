const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('UX-1 fresh local browser enters without credentials and searches sessions', async ({ page, context }) => {
  const entry = hostInfo('success');
  const requests = [];
  page.on('request', request => {
    if (request.url().startsWith(entry.origin)) requests.push(request);
  });
  await openWorkbench(page, entry);
  await expect(page.locator('#app')).toBeVisible();
  await expect(page.locator('body')).not.toContainText(/pairing token|web-token/i);
  expect(await page.evaluate(() => localStorage.getItem('clat.auth.v1'))).toBeNull();
  const cookies = await context.cookies(entry.origin);
  const session = cookies.find(cookie => cookie.name.startsWith('clat_session_'));
  expect(session).toMatchObject({ httpOnly: true, sameSite: 'Strict' });
  expect(session.value).not.toContain(entry.token);
  expect(requests.every(request => !request.headers().authorization && !request.url().includes(entry.token))).toBe(true);
  await page.fill('#prompt', 'UX-1 searchable session');
  await page.click('#send');
  await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);
  await expect(page.locator('#session-count')).not.toHaveText('0', LIVE);
  await page.fill('#session-search', 'UX-1-no-such-session');
  await expect(page.locator('#session-empty')).toBeVisible();
});

test('UX-1 localhost canonicalizes workspace and stale credentials recover silently', async ({ page }) => {
  const entry = hostInfo('success');
  await page.addInitScript(() => localStorage.setItem('clat.auth.v1', 'invalid-legacy-credential'));
  await page.goto(entry.origin.replace('127.0.0.1', 'localhost') + '/?workspace=default');
  await expect(page).toHaveURL(entry.origin + '/?workspace=default');
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#landing')).toBeHidden();
  expect(await page.evaluate(() => localStorage.getItem('clat.auth.v1'))).toBeNull();
});

test('UX-1 clears credentials only for explicit invalid_credentials', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => localStorage.setItem('clat.auth.v1', 'retain-on-policy-401'));
  let rejected = 0;
  await page.route('**/api/events', async route => {
    rejected += 1;
    await route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({
      ok: false, error: { code: 'unauthorized', message: 'policy unavailable' },
    }) });
  });
  await page.evaluate(() => resubscribe());
  await expect.poll(() => rejected, LIVE).toBeGreaterThan(0);
  await expect(page.locator('#conn-status')).toHaveText('reconnecting…', LIVE);
  expect(await page.evaluate(() => localStorage.getItem('clat.auth.v1'))).toBe('retain-on-policy-401');
  await expect(page.locator('#landing')).toBeHidden();
  await page.unroute('**/api/events');
  let invalid = 0;
  await page.route('**/api/events', async route => {
    if (invalid++ === 0) {
      await route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({
        ok: false, error: { code: 'unauthorized', authentication: 'invalid_credentials' },
      }) });
    } else await route.continue();
  });
  await page.evaluate(() => resubscribe());
  await expect.poll(() => invalid, LIVE).toBeGreaterThan(1);
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  expect(await page.evaluate(() => localStorage.getItem('clat.auth.v1'))).toBeNull();
  await expect(page.locator('#landing')).toBeHidden();
});

test('UX-1 local access works when legacy credential storage is unavailable', async ({ page }) => {
  await page.addInitScript(() => {
    const read = Storage.prototype.getItem;
    const remove = Storage.prototype.removeItem;
    Storage.prototype.getItem = function (key) {
      if (key === 'clat.auth.v1') throw new DOMException('Storage denied', 'SecurityError');
      return read.call(this, key);
    };
    Storage.prototype.removeItem = function (key) {
      if (key === 'clat.auth.v1') throw new DOMException('Storage denied', 'SecurityError');
      return remove.call(this, key);
    };
  });
  await openWorkbench(page, hostInfo('success'));
  await expect(page.locator('#landing')).toBeHidden();
});
