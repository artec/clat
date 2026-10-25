const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('sidebar tools keep icons, translated names and working dialogs when collapsed', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  for (const locale of ['en', 'zh-Hans', 'zh-Hant']) {
    await page.evaluate(locale => CLATI18n.setLanguage(locale), locale);
    for (const collapsed of [false, true]) {
      await page.evaluate(collapsed => {
        state.sidebar = collapsed ? 'collapsed' : 'expanded'; applyPresentation();
      }, collapsed);
      if (!collapsed) {
        const project = page.locator('#project-navigation');
        await expect(project.locator('svg')).toBeVisible();
        expect((await project.textContent()).trim()).toBe('');
        const header = await page.locator('.project-heading').boundingBox();
        const button = await project.boundingBox();
        expect(Math.abs(header.y+header.height/2-button.y-button.height/2)).toBeLessThanOrEqual(1);
        expect(header.x+header.width-button.x-button.width).toBeLessThanOrEqual(1);
        for(const [id,key] of [['active','web.active_sessions_tab'],['archived','web.archived_sessions_tab']]) {
          const name=await page.evaluate(key=>CLATI18n.t(key),key);
          await expect(page.locator(`#${id}-sessions`)).toHaveAccessibleName(name);
        }
        await page.locator('#archived-sessions').click();
        await expect(page.locator('#archived-sessions')).toHaveAttribute('aria-selected','true');
        await page.locator('#archived-sessions').press('ArrowLeft');
        await expect(page.locator('#active-sessions')).toBeFocused();
        await expect(page.locator('#active-sessions')).toHaveAttribute('aria-selected','true');
      }
      for (const [id, key] of [['schedules', 'schedule.open'], ['jobs', 'job.open'], ['history', 'history.open']]) {
        const trigger = page.locator(`#${id}-open`);
        const label = await page.evaluate(key => CLATI18n.t(key), key);
        await expect(trigger).toHaveAccessibleName(label);
        await expect(trigger).toHaveAttribute('title', label);
        await expect(trigger.locator('svg')).toBeVisible();
        await expect(trigger.locator('span')).toBeVisible({ visible: !collapsed });
        await trigger.click();
        await expect(page.locator(`#${id}-dialog`)).toBeVisible();
        await page.keyboard.press('Escape');
      }
    }
  }
  await page.screenshot({ path: '../../output/playwright/sidebar-icons-collapsed.png' });
});

test('send and stop share one icon seat, preserve the draft and invoke only their own action', async ({ page }) => {
  await openWorkbench(page, hostInfo('long-stream'));
  await page.fill('#prompt', 'Run until stopped');
  const send = page.locator('#send'), stop = page.locator('#cancel');
  const before = await send.boundingBox();
  await expect(send.locator('svg')).toBeVisible();
  expect((await send.textContent()).trim()).toBe('');
  await expect(stop).toBeHidden();
  await send.click();
  await expect(stop).toBeVisible();
  await expect(send).toBeHidden();
  const during = await stop.boundingBox();
  expect(during.x).toBeCloseTo(before.x, 0);
  expect(during.width).toBe(before.width);
  await page.fill('#prompt', '保留草稿');
  for (const locale of ['en', 'zh-Hans', 'zh-Hant']) {
    const label = await page.evaluate(async locale => {
      await CLATI18n.setLanguage(locale); return CLATI18n.t('web.stop');
    }, locale);
    await expect(stop).toHaveAccessibleName(label);
    await expect(stop).toHaveAttribute('title', label);
    await expect(page.locator('#prompt')).toHaveValue('保留草稿');
  }
  await page.screenshot({ path: '../../output/playwright/composer-stop-icon.png' });
  const cancelled = page.waitForRequest(request => request.url().endsWith('/api/run.cancel'));
  await stop.click(); await cancelled;
  await expect(stop).toBeHidden(LIVE);
  await expect(send).toBeVisible(LIVE);
  await expect(page.locator('#prompt')).toHaveValue('保留草稿');
  await page.screenshot({ path: '../../output/playwright/composer-send-icon.png' });
  for (const width of [960, 390]) {
    await page.setViewportSize({ width, height: 820 });
    const box = await send.boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    expect(box.width).toBe(36);
  }
});
