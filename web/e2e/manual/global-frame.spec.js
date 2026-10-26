// Manual visual evidence. These screenshots are not CI goldens.
const { test, expect } = require('@playwright/test');
const fs = require('node:fs'), path = require('node:path');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');

test('review the global toolbar and extension across content pages', async ({ page }, info) => {
  await page.setViewportSize({ width: 1487, height: 1058 });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await openWorkbench(page, hostInfo('workbench'));
  await page.evaluate(() => CLATI18n.setLanguage('zh-Hans'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'global-workspace.md');
  fs.writeFileSync(file, '# 当前工作资料\n\n应用由固定导航、内容区和全局右扩展组成。\n\n- 顶部工具栏提供位置与导航\n- 内容页切换后，右侧检视继续保留\n- 文件引用复用版本校验与原会话草稿\n');
  const shot = name => page.screenshot({ path: info.outputPath(name + '.png'), animations: 'disabled' });
  try {
    await page.evaluate(() => {
      addUserMessage('在浏览设置和任务时，继续保留当前文件的检视。');
      const reply = addAssistantMessage();
      reply.appendBody('右扩展与内容页独立。\n\n可以在左侧切换资料、任务、搜索或设置，右侧继续查看当前工作的文件、计划或变更。顶部工具栏始终提供位置、后退、前进与扩展区开关。'); reply.finishBody();
    });
    await page.fill('#prompt', '继续保留这份草稿，稍后再发送。');
    await page.click('#composer-add'); await page.click('#add-project-file');
    await page.locator('#file-browser input[type="search"]').fill('global-workspace');
    await page.locator('#file-browser .review-file').click();
    await expect(page.locator('#file-browser pre')).toContainText('当前工作资料', LIVE);
    await shot('conversation-expanded');
    await page.click('#project-list-toggle'); await shot('conversation-collapsed');
    for (const [nav, name] of [['nav-history', 'search'], ['nav-tasks','tasks'], ['nav-settings','settings'], ['nav-resources','resources'], ['nav-extensions','extensions'], ['nav-help','help']]) {
      await page.click('#' + nav); await shot(name);
    }
    await page.setViewportSize({ width: 390, height: 844 });
    await shot('help-phone-extension');
    await page.click('#inspector-toggle'); await shot('help-phone');
    await page.click('#nav-workbench'); await page.click('#mobile-sidebar-open');
    await shot('conversation-list-phone');
  } finally { fs.rmSync(file, { force: true }); }
});
