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

test('a full-length summary body stops at five lines and toggles in place', async ({ page }) => {
  await openWorkbench(page, hostInfo('session-summary'));
  await page.locator('.application-toolbar [data-action="inspector"]').click();
  const card = page.locator('#conversation-summary');
  const body = card.locator('.conversation-summary-text');
  const box = () => body.evaluate(node => {
    const style = getComputedStyle(node);
    return {
      limit: parseFloat(style.lineHeight) * 5 + 1,
      height: node.clientHeight,
      content: node.scrollHeight,
      fade: style.maskImage || style.webkitMaskImage,
    };
  });
  await page.locator('#prompt').fill('Summarize at full length');
  await page.locator('#send').click();
  // The host saves its largest English summary; it must wrap past five lines.
  await expect(card).toContainText('remaining work covers', LIVE);
  await expect(body).toHaveClass(/is-clamped/);
  await expect(body).toHaveAttribute('role', 'button');
  await expect(body).toHaveAttribute('aria-expanded', 'false');
  const clamped = await box();
  expect(clamped.height).toBeLessThanOrEqual(clamped.limit);
  expect(clamped.content).toBeGreaterThan(clamped.height);
  expect(clamped.fade).toContain('linear-gradient');

  await body.click();
  await expect(body).toHaveAttribute('aria-expanded', 'true');
  await expect(body).not.toHaveClass(/is-clamped/);
  const expanded = await box();
  expect(expanded.height).toBe(expanded.content);
  expect(expanded.fade).toBe('none');

  // The keyboard toggles the same body, and a fresh document starts clamped.
  await body.focus();
  await page.keyboard.press('Enter');
  await expect(body).toHaveAttribute('aria-expanded', 'false');
  await body.focus();
  await page.keyboard.press('Space');
  await expect(body).toHaveAttribute('aria-expanded', 'true');
  await body.click();
  await expect(body).toHaveAttribute('aria-expanded', 'false');
  await page.reload();
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).not.toBeNull();
  if (await page.locator('#inspector-toggle').getAttribute('aria-expanded') !== 'true') {
    await page.locator('.application-toolbar [data-action="inspector"]').click();
  }
  await expect(card).toContainText('remaining work covers', LIVE);
  await expect(body).toHaveClass(/is-clamped/);
  await expect(body).toHaveAttribute('aria-expanded', 'false');
});
