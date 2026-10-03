const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');
test.skip(!process.env.CLAT_PLG2_MARKET_URL, 'requires an explicitly configured signed staging market');

test('PLG-2 signed compiled DSH package installs after consent, executes, and uninstalls cleanly', async ({ page }) => {
  const entry = hostInfo('plugin-market'); const secret = 'private-fixture-key';
  const publicRequests = [];
  await page.route('https://pi.at.cn/catalog.json', route => {
    publicRequests.push(route.request());
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ schemaVersion: 1, packages: [{
      id: 'io.artec.dsh-official-web', name: 'DSH official web search and fetch', runtime: 'mcp-stdio',
      status: 'available', summary: 'Signed staging fixture', tags: ['DSH'],
    }] }) });
  });
  await openWorkbench(page, entry);
  // Exercise installation after the first run has frozen registrations.
  await page.fill('#prompt', 'before installation'); await page.click('#send');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect(page.locator('.msg.assistant').last()).toContainText('done', LIVE);
  await page.click('#market-open');
  await expect(page.locator('.plugin-installed')).toContainText('No plugins installed.', LIVE);
  const preparedReply = page.waitForResponse(r => r.url().endsWith('/api/plugin.prepare'));
  await page.getByRole('button', { name: 'Install…', exact: true }).click();
  const proposal = (await (await preparedReply).json()).value;
  await expect(page.locator('.plugin-review')).toContainText('Native executable', LIVE);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: '../../output/playwright/plg2-review-mobile.png', fullPage: true });
  await page.setViewportSize({ width: 1280, height: 720 });
  const install = page.locator('.plugin-review').getByRole('button', { name: 'Install', exact: true });
  await expect(install).toBeDisabled();
  const refused = await page.evaluate(async ticket => {
    try { await rpc('plugin.commit', { ticket, accept_capabilities: false }); return false; }
    catch (error) { return error.message.includes('consent'); }
  }, proposal.ticket);
  expect(refused).toBe(true);
  expect((await page.evaluate(() => rpc('plugin.list', {}))).installed).toEqual([]);
  await page.getByLabel('DeepSeek API key', { exact: true }).fill(secret);
  const committedReply = page.waitForResponse(r => r.url().endsWith('/api/plugin.commit'), { timeout: 90000 });
  await page.locator('.plugin-consent input').check(); await install.click();
  expect((await (await committedReply).json()).ok).toBe(true);
  const installed = page.locator('.plugin-installed [data-plugin-id="io.artec.dsh-official-web"]');
  await expect(installed).toContainText('Enabled', LIVE);
  await expect(page.locator('.plugin-review')).toBeHidden();
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain(secret);
  const described = await page.evaluate(() => rpc('host.describe', {}));
  const registry = path.join(described.storage_root, 'plugin-store', 'registry.json');
  expect(fs.readFileSync(registry, 'utf8')).toContain(secret);
  if (process.platform !== 'win32') expect(fs.statSync(registry).mode & 0o777).toBe(0o600);
  await page.screenshot({ path: '../../output/playwright/plg2-installed.png', fullPage: true });
  await page.click('#market-close');
  await page.fill('#prompt', 'search with installed plugin'); await page.click('#send');
  await page.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect(page.locator('#transcript')).toContainText('CLAT PLG-2 fixture', LIVE);
  expect(await page.locator('#transcript').textContent()).not.toContain(secret);
  await page.click('#market-open');
  await expect(installed).toBeVisible(LIVE); await installed.getByRole('button', { name: 'Disable', exact: true }).click();
  await expect(installed).toContainText('Disabled', LIVE);
  await installed.getByRole('button', { name: 'Enable', exact: true }).click();
  await page.locator('.plugin-consent input').check(); await page.locator('.plugin-review').getByRole('button', { name: 'Apply', exact: true }).click();
  await expect(installed).toContainText('Enabled', LIVE);
  await installed.getByRole('button', { name: 'Uninstall', exact: true }).click();
  await expect(page.locator('.plugin-installed')).toContainText('No plugins installed.', LIVE);
  expect(fs.existsSync(path.join(described.storage_root, 'plugin-store', 'artifacts', 'io.artec.dsh-official-web'))).toBe(false);
  expect(fs.readFileSync(registry, 'utf8')).not.toContain(secret);
  expect(fs.readFileSync(registry, 'utf8')).not.toContain('io.artec.dsh-official-web');
  for (const request of publicRequests) {
    expect(request.headers().authorization).toBeUndefined(); expect(request.headers().cookie).toBeUndefined();
    expect(request.url()).not.toContain(entry.token); expect(request.url()).not.toContain(secret);
  }
});
