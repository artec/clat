const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('blank updates retain configuration; optional enum and boolean distinguish omission from false', async ({ page }) => {
  const id = 'dev.clat.configuration'; const commits = [];
  await page.route('https://pi.at.cn/catalog.json', route => route.fulfill({ contentType: 'application/json', body: JSON.stringify({ schemaVersion: 1, packages: [] }) }));
  await page.route('**/api/plugin.*', route => {
    const method = route.request().url().split('/api/')[1];
    let value = {};
    if (method === 'plugin.list') value = { installed: [{ id, name: 'Configuration fixture', version: '1.0.0', enabled: true, trust: 'publisher-verified' }] };
    if (method === 'plugin.prepare') value = { ticket: 'ui-only-review', action: 'update', review: { packages: [{ id, name: 'Configuration fixture', version: '2.0.0', runtime: 'wasm-component', capabilities: {}, config_schema: { type: 'object', properties: {
      enabled: { type: 'boolean', title: 'Optional boolean' }, mode: { type: 'string', enum: ['a', 'b'], title: 'Optional mode' },
    } } }] } };
    if (method === 'plugin.commit') { commits.push(route.request().postDataJSON()); value = { committed: true }; }
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ ok: true, value }) });
  });
  await openWorkbench(page, hostInfo('success')); await page.click('#market-open');
  const update = page.locator('.plugin-installed').getByRole('button', { name: 'Update', exact: true });
  await expect(update).toBeVisible(LIVE); await update.click();
  await page.locator('.plugin-consent input').check();
  await page.locator('.plugin-review').getByRole('button', { name: 'Apply', exact: true }).click();
  await expect.poll(() => commits.length).toBe(1);
  expect(commits[0].configs).toEqual({});
  await expect(update).toBeEnabled(LIVE); await update.click();
  await page.getByLabel('Optional boolean', { exact: true }).selectOption('false');
  await page.getByLabel('Optional mode', { exact: true }).selectOption('"b"');
  await page.locator('.plugin-consent input').check();
  await page.locator('.plugin-review').getByRole('button', { name: 'Apply', exact: true }).click();
  await expect.poll(() => commits.length).toBe(2);
  expect(commits[1].configs).toEqual({ [id]: { enabled: false, mode: 'b' } });
  await expect(page.locator('.plugin-review')).toBeHidden();
});
