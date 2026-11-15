const { test, expect } = require('@playwright/test');
const { readFile } = require('node:fs/promises');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');

test('export and reimport a trajectory without replacing the conversation', async ({ page }) => {
  const message = 'Round-trip trajectory marker';
  await openWorkbench(page, hostInfo('success'));
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.fill('#prompt', message);
  await page.click('#send');
  await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);
  await expect(page.locator('#cancel')).not.toBeVisible(LIVE);
  const original = await page.evaluate(() => state.sessionId);
  await page.fill('#prompt', 'Unsaved draft');

  await page.click('#nav-history');
  const history = page.locator('#history-dialog');
  const search = history.locator('.session-search input');
  await search.fill(message);
  await search.press('Enter');
  await history.locator('.history-results .navigation-row').filter({ hasText: message }).first().click();
  const exportButton = page.locator('[data-action=archive-export]');
  const downloading = page.waitForEvent('download');
  await exportButton.click();
  const downloaded = await downloading;
  const bytes = await readFile(await downloaded.path());
  expect(bytes.subarray(0, 4).toString('hex')).toBe('504b0304');

  await history.locator('input[type=file]').setInputFiles({
    name: downloaded.suggestedFilename(), mimeType: 'application/zip', buffer: bytes,
  });
  await expect(history.locator('.history-results .navigation-row')).toContainText(message, LIVE);
  await expect(history.locator('.history-trajectory')).toBeVisible(LIVE);
  const reexporting = page.waitForEvent('download');
  await exportButton.click();
  expect(await readFile(await (await reexporting).path())).toEqual(bytes);
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toHaveValue('Unsaved draft');
  expect(await page.evaluate(() => state.sessionId)).toBe(original);
});
