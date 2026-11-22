const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('session summary follows language, survives manual naming, and has its own switch', async ({ page }) => {
  await openWorkbench(page, hostInfo('session-summary'));
  const setLanguage = async locale => {
    await page.locator('[data-settings-category="appearance"]').click();
    await page.locator('#settings-dialog [data-language-select]').selectOption(locale);
  };
  await page.locator('#nav-settings').click();
  await setLanguage('zh-Hans');
  await page.locator('[data-settings-category="utility"]').click();
  const naming = page.locator('#utility-naming-enabled');
  const summary = page.locator('#utility-summary-enabled');
  const save = page.locator('#utility-settings-save');
  await expect(naming).toBeChecked(LIVE);
  await expect(summary).toBeChecked(LIVE);

  await page.locator('#nav-workbench').click();
  await page.locator('.application-toolbar [data-action="inspector"]').click();
  const card = page.locator('#conversation-summary');
  await expect(card).toBeHidden();
  const send = async text => {
    await page.locator('#prompt').fill(text);
    await page.locator('#send').click();
  };
  await send('Review the change');
  await expect(card).toBeVisible(LIVE);
  await expect(card).toContainText('已完成初步检查');
  await expect(card).toHaveAttribute('aria-label', '会话摘要');
  await expect(card).not.toContainText('会话摘要');
  const session = await page.evaluate(async () => {
    const id = state.sessionId;
    await rpc('session.rename', { id, title: 'My review' });
    return id;
  });

  await page.locator('#nav-settings').click();
  await setLanguage('zh-Hant');
  await page.locator('[data-settings-category="utility"]').click();
  await naming.uncheck();
  await save.click();
  await expect(page.locator('#utility-settings-saved')).not.toBeEmpty(LIVE);
  await page.locator('#nav-workbench').click();
  await expect(card).toContainText('已完成初步检查');
  await send('Add a focused test');
  await expect(card).toContainText('已完成初步檢查', LIVE);
  await expect.poll(() => page.evaluate(async () => (await rpc('workbench.info', {})).session.title), LIVE).toBe('My review');

  await page.locator('#nav-settings').click();
  await page.locator('[data-settings-category="utility"]').click();
  await summary.uncheck();
  await save.click();
  await expect(page.locator('#utility-settings-saved')).not.toBeEmpty(LIVE);
  await page.locator('#nav-workbench').click();
  await expect(card).toBeHidden(LIVE);

  await page.locator('#nav-settings').click();
  await page.locator('[data-settings-category="utility"]').click();
  await summary.check();
  await save.click();
  await expect(page.locator('#utility-settings-saved')).not.toBeEmpty(LIVE);
  await page.locator('#nav-workbench').click();
  await expect(card).toContainText('已完成初步檢查', LIVE);
  await page.reload();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(session);
  if (await page.locator('#inspector-toggle').getAttribute('aria-expanded') !== 'true') {
    await page.locator('.application-toolbar [data-action="inspector"]').click();
  }
  await expect(card).toBeVisible(LIVE);
  await expect(card).toContainText('已完成初步檢查');
});
