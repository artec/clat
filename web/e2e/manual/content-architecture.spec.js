// Human review evidence only. No DOM structure, column totals or pixel goldens.
const { test, expect } = require('@playwright/test');
const fs = require('node:fs');
const path = require('node:path');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');

test('review the application content pages and nested conversation workspace', async ({ page }, info) => {
  await page.setViewportSize({ width: 1487, height: 1058 });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await openWorkbench(page, hostInfo('workbench'));
  await page.evaluate(() => CLATI18n.setLanguage('zh-Hans'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'design-review.md');
  fs.writeFileSync(file, '# 页面内容与对话工作区\n\n项目文件可在资料页阅读，并引用固定快照回到对话草稿。\n');
  const shot = name => page.screenshot({ path: info.outputPath(name + '.png'), animations: 'disabled' });
  try {
    await page.evaluate(() => {
      addUserMessage('请检查应用导航、内容页和右侧工作区的归属。');
      const reply = addAssistantMessage();
      reply.appendBody('导航保持常驻。\n\n对话页包含项目与会话列表、消息详情和输入；资料、任务与搜索使用各自的内容页。');
      reply.finishBody();
    });
    await page.fill('#prompt', '返回对话后继续保留这份草稿。');
    await page.click('#inspector-toggle');
    await shot('conversation-expanded');
    await page.click('#project-list-toggle');
    await shot('conversation-collapsed');
    await page.click('#project-list-toggle');
    for (const [nav, name] of [['nav-history','search'], ['nav-tasks','tasks'], ['nav-settings','settings'], ['nav-help','help']]) {
      await page.click('#' + nav); await shot(name);
    }
    await page.click('#nav-resources');
    await page.locator('#resource-files input[type="search"]').fill('design-review');
    await page.locator('#resource-files .review-file').click();
    await expect(page.locator('#resource-files pre')).toContainText('页面内容与对话工作区', LIVE);
    await shot('resources');
    await page.setViewportSize({ width: 390, height: 844 });
    await shot('resources-phone');
    await page.click('#nav-history'); await shot('search-phone');
    await page.click('#nav-workbench');
    await page.click('#inspector-close');
    await page.click('#mobile-sidebar-open');
    await shot('conversation-list-phone');
  } finally { fs.rmSync(file, { force: true }); }
});
