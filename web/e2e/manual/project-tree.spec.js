// Manual review only: real isolated fixture sessions, no pixel or layout goldens.
const { test, expect } = require('@playwright/test');
const { hostInfo, openWorkbench, addWorkbenchProject, LIVE } = require('../helpers/workbench');

test('review the combined CLAT navigation icon, gear and grouped project conversations', async ({ page }, info) => {
  const fixture = hostInfo('project-tree');
  await page.setViewportSize({ width: 1487, height: 1058 });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await openWorkbench(page, fixture);
  await page.evaluate(() => CLATI18n.setLanguage('zh-Hans'));
  await expect(page.locator('#session-list .session-item').first()).toBeVisible(LIVE);
  await page.fill('#prompt', '继续保留当前项目的草稿。');
  const route = await addWorkbenchProject(page, fixture.secondaryProject.root);
  await expect(page.locator('#session-list .session-item').first()).toBeVisible(LIVE);
  await page.fill('#prompt', 'ECAR 的草稿与其他项目分开保存。');
  await page.click('[data-project-route="default"]'); await page.click('#nav-workbench');
  const shot = name => page.screenshot({ path: info.outputPath(name + '.png'), animations: 'disabled' });
  await shot('project-groups-dark');
  // Presentation-only reasoning sample; it is not submitted or persisted as chat.
  await page.evaluate(() => {
    const reply = addAssistantMessage();
    reply.setReasoning('检查现有能力，并整理后续处理步骤。');
    reply.appendBody('已完成检查，接下来继续处理项目。'); reply.finishBody();
    reply.node.dataset.manualReview = 'reasoning';
  });
  await shot('conversation-cleanup');
  await page.click('#tools-open'); await expect(page.locator('#workbench-tools')).toBeVisible();
  await shot('tools-icon-menu'); await page.click('#tools-open');
  await page.evaluate(() => document.querySelector('[data-manual-review="reasoning"]')?.remove());
  await page.locator('[data-project-owner="default"] [data-action="more-project-sessions"]').click();
  await shot('project-show-more');
  await page.locator(`[data-project-owner="${route}"] [data-action="toggle-project"]`).click();
  await shot('project-collapsed');
  await page.locator(`[data-project-owner="${route}"] [data-action="toggle-project"]`).click();
  await page.click('#nav-help'); await shot('navigation-help-dark');
  await page.locator('#global-nav').screenshot({ path: info.outputPath('navigation-icons-dark.png'), animations: 'disabled' });
  await page.click('#nav-workbench');
  await page.evaluate(() => { state.theme = 'light'; applyPresentation(); });
  await shot('project-groups-light');
  await page.click('#nav-settings'); await shot('settings-gear-light');
  await page.click('#nav-workbench');
  await page.evaluate(() => { state.theme = 'dark'; applyPresentation(); });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.click('#mobile-sidebar-open'); await shot('project-groups-phone');
  await page.click('#mobile-sidebar-close');
  await page.setViewportSize({ width: 1487, height: 1058 });
  await page.click('#project-list-toggle'); await shot('combined-navigation-icon');
});
