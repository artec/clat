const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');


test('send and stop preserve the draft and invoke only their own action', async ({ page }) => {
  await openWorkbench(page, hostInfo('long-stream'));
  await page.fill('#prompt', 'Run until stopped');
  const send = page.locator('#send'), stop = page.locator('#cancel');
  await expect(stop).toBeHidden();
  await send.click();
  await expect(stop).toBeVisible();
  await expect(send).toBeHidden();
  await page.fill('#prompt', '保留草稿');
  for (const locale of ['en', 'zh-Hans', 'zh-Hant']) {
    const label = await page.evaluate(async locale => {
      await CLATI18n.setLanguage(locale); return CLATI18n.t('web.stop');
    }, locale);
    await expect(stop).toHaveAccessibleName(label);
    await expect(stop).toHaveAttribute('title', label);
    await expect(page.locator('#prompt')).toHaveValue('保留草稿');
  }

  const cancelled = page.waitForRequest(request => request.url().endsWith('/api/run.cancel'));
  await stop.click(); await cancelled;
  await expect(stop).toBeHidden(LIVE);
  await expect(send).toBeVisible(LIVE);
  await expect(page.locator('#prompt')).toHaveValue('保留草稿');

});
