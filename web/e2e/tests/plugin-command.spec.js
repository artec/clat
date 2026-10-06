const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');
test('PLG-6 shared plugin command opens the existing PWA management surface', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.fill('#prompt', '/plugin');
  await page.locator('#send').click();
  await expect(page.locator('#market-dialog')).toBeVisible(LIVE);
  await expect(page.locator('.plugin-installed')).toBeVisible(LIVE);
  await expect(page.locator('#prompt')).toHaveValue('');
});
