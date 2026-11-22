const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

const authorizationPanel = page => page.locator('[data-settings-panel=authorizations]');
const providerCard = (panel, provider) => panel.locator(`[data-provider="${provider}"]`);
const action = (node, key) => node.locator(`[data-action="${key}"]`);

async function openAuthorizations(page) {
  await page.locator('#nav-settings').click();
  await page.locator('[data-settings-category=authorizations]').click();
  await expect(authorizationPanel(page)).toBeVisible();
}

test('authorization settings are available before choosing a quota-capable model', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await openAuthorizations(page);
  const panel = authorizationPanel(page);
  await expect(action(providerCard(panel, 'qianwen'), 'sign_in')).toBeEnabled(LIVE);
  await expect(action(providerCard(panel, 'tencent'), 'add_key')).toBeEnabled(LIVE);
  const view = await page.evaluate(() => rpc('quota.authorizations.get', {}));
  expect(view.providers.map(({provider, configured, allowance}) => [provider, configured, allowance.status]))
    .toEqual([['qianwen', false, 'needs_auth'], ['tencent', false, 'needs_auth']]);
  const afterRemoval = await page.evaluate(async () => {
    await rpc('quota.tencent.key', { secret_id: 'local-fixture-id', secret_key: 'local-fixture-secret' });
    await rpc('quota.authorizations.remove', { provider: 'tencent' });
    return rpc('quota.authorizations.get', {});
  });
  expect(afterRemoval.providers.find(row => row.provider === 'tencent').configured).toBe(false);
});

test('authorize, refresh and remove console credentials without exposing saved keys', async ({ page }) => {
  let login = 'none', qianwenConfigured = false, tencentConfigured = false;
  let remaining = 71;
  const tencentWrites = [], opens = [], removals = [];
  await page.exposeFunction('observeAuthorizationPage', url => opens.push(url));
  await page.addInitScript(() => { window.open = url => window.observeAuthorizationPage(url); });
  await page.route('**/api/quota.*', async route => {
    const method = new URL(route.request().url()).pathname.split('/').at(-1);
    const params = route.request().postDataJSON();
    let value = {};
    switch (method) {
      case 'quota.authorizations.get':
        if (params.refresh) remaining = 41;
        value = {
          qianwen_login: login,
          providers: [['qianwen', qianwenConfigured], ['tencent', tencentConfigured]].map(([provider, configured]) => ({
            provider, configured, refreshing: false,
            configured_at: configured ? '2026-11-21T12:00:00Z' : null,
            allowance: {
              status: configured ? 'available' : 'needs_auth',
              items: configured ? [{kind: 'quota', value: String(remaining),
                window_hours: provider === 'qianwen' ? 168 : null}] : [],
            },
          })),
        };
        break;
      case 'quota.console.login':
        login = 'pending';
        value = { url: 'https://account.qianwenai.com/sso' };
        break;
      case 'quota.console.cancel':
        login = 'none';
        break;
      case 'quota.tencent.key':
        tencentWrites.push(params);
        tencentConfigured = true;
        break;
      case 'quota.authorizations.remove':
        removals.push(params.provider);
        if (params.provider === 'tencent') tencentConfigured = false;
        else { qianwenConfigured = false; login = 'none'; }
        break;
      default: return route.continue();
    }
    await route.fulfill({ json: { ok: true, value } });
  });

  await openWorkbench(page, hostInfo('success'));
  await openAuthorizations(page);
  const panel = authorizationPanel(page);
  const qianwen = providerCard(panel, 'qianwen'), tencent = providerCard(panel, 'tencent');
  await action(qianwen, 'sign_in').click();
  await action(qianwen, 'open_login').click();
  expect(opens).toEqual(['https://account.qianwenai.com/sso']);
  await action(qianwen, 'cancel_login').click();
  await expect(action(qianwen, 'open_login')).toBeHidden();
  await action(qianwen, 'sign_in').click();
  await expect(action(qianwen, 'open_login')).toBeVisible(LIVE);
  qianwenConfigured = true;
  login = 'complete';
  await expect(qianwen.getByRole('progressbar')).toHaveAttribute('value', '71', LIVE);

  await action(tencent, 'add_key').click();
  const fields = tencent.locator('input[type=password]');
  await fields.nth(0).fill('fake-tested-id');
  await fields.nth(1).fill('fake-tested-secret');
  await page.locator('[data-settings-category=appearance]').click();
  await page.locator('[data-settings-category=authorizations]').click();
  await action(tencent, 'add_key').click();
  await expect(fields.nth(0)).toHaveValue('');
  await expect(fields.nth(1)).toHaveValue('');
  await fields.nth(0).fill('second-test-id');
  await fields.nth(1).fill('second-test-secret');
  await action(tencent, 'save_key').click();
  await expect(tencent.getByRole('progressbar')).toHaveAttribute('value', '71', LIVE);
  expect(tencentWrites).toEqual([{secret_id: 'second-test-id', secret_key: 'second-test-secret'}]);
  await expect(fields.nth(1)).toHaveValue('');

  await action(panel, 'refresh_all').click();
  await expect(tencent.getByRole('progressbar')).toHaveAttribute('value', '41', LIVE);
  await expect(qianwen.getByRole('progressbar')).toHaveAttribute('value', '41', LIVE);

  await action(tencent, 'remove').click();
  await page.locator('dialog[data-kind=confirm] [data-role=accept]').click();
  await expect(tencent.getByRole('progressbar')).toBeHidden();
  await expect(qianwen.getByRole('progressbar')).toBeVisible();
  expect(removals).toEqual(['tencent']);

  await page.setViewportSize({ width: 430, height: 850 });
  await action(tencent, 'add_key').click();
  await fields.nth(1).fill('temporary-mobile-key');
  await page.locator('.settings-mobile-back').click();
  await page.locator('[data-settings-category=authorizations]').click();
  await action(tencent, 'add_key').click();
  await expect(fields.nth(1)).toHaveValue('');
});
