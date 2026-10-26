// Optional screenshot capture for human review, excluded from ordinary CI.
const {test} = require('@playwright/test');
const {expect} = require('@playwright/test');
const {LIVE,hostInfo,openWorkbench,fetchBrowserRequest} = require('../helpers/workbench');

test('capture representative workbench layouts for visual review', async ({page},testInfo) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.route('**/api/workbench.info', async (route) => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.model.model = '中文长模型名 · DeepSeek-V4-Long-Context-Preview';
    await route.fulfill({ response, json: body });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.evaluate(() => {
    dom['transcript-scroll'].style.scrollBehavior = 'auto';
    addUserMessage('请检查项目的权限边界，并用表格说明结论。\n\n我们希望保留本地优先、零构建和历史定位，长中文内容不能挤压输入区。');
    for (let index = 0; index < 8; index++) {
      handleReplay({ type: 'tool_requested', seq: index + 2, call: {
        name: index % 2 ? 'read_file' : 'search', arguments: { path: 'src/serve/permissions.rs' },
      } });
      handleReplay({ type: 'tool_finished', tool: index % 2 ? 'read_file' : 'search', output: '已核对读写者与失败路径。' });
    }
    const reply = addAssistantMessage();
    reply.setReasoning('先核对状态归属，再检查权限失败路径。\n不会仅凭绿色检查宣称真实设备验收通过。');
    reply.appendBody('## 结论：保留边界，简化呈现\n\n这是同一份内容的设计走查。正文、过程和输入区应有稳定的位置，中文段落保持适当行距。审批和错误仍然需要让人及时看见。\n\n'
      + '| 对象 | 当前状态 | 判断 |\n| --- | --- | --- |\n| 会话与草稿 | 由宿主和当前输入归属管理 | 不改变持久化语义 |\n| 工具与权限 | 请求和结果可追溯 | 普通过程可以折叠 |\n| 历史与地图 | 分页加载，角色明确 | 保留阅读位置 |\n\n'
      + '### 验证路径\n\n```rust\nlet policy = InteractivePermissionPolicy::new(approver);\n// 这条较长的代码只应在代码块内部横向滚动，不应撑宽整张页面。\n```\n\n'
      + '| 宽表验证 | 长路径 | 版本 | 备注 | 预期行为 |\n| --- | --- | --- | --- | --- |\n| 模型与工具边界 | src/very_long_directory_name/a_very_long_file_name.rs | session.v4 | 中文说明中文说明中文说明 | 表格内部滚动，不改变页面宽度 |\n\n'
      + '### 后续工作\n\n- 负责人用四个动作比较：找结论、读表格、看工具结果、输入下一句。\n- 流式正文、安全链接和图片附件不改变。\n- 真机键盘与长时间阅读仍需实际使用反馈。\n\n'
      + ('普通中文段落用于检验长会话的阅读节奏。信息不应依赖炫目的颜色，也不应因为弱化过程而隐藏待处理事项。\n\n').repeat(4));
    reply.finishBody();
    addVerdict('completed', 'completed · 1 turn · local scripted fixture');
  });
  for (const [name,width,height,theme] of [
    ['desktop-dark',1487,1058,'dark'],['desktop-light',1487,1058,'light'],
    ['phone-dark',390,844,'dark'],['narrow-light',320,844,'light'],
  ]) {
    await page.setViewportSize({width,height});
    await page.emulateMedia({reducedMotion:'reduce'});
    await page.click('#nav-settings');
    await page.click('[data-settings-category="appearance"]');
    await page.locator('input[name="theme"][value="'+theme+'"]').check();
    await page.click('#nav-workbench');
    await page.fill('#prompt','请继续核对失败路径。');
    await page.screenshot({path:testInfo.outputPath(name+'.png'),animations:'disabled'});
    await page.click('#model-picker-trigger');
    await page.screenshot({path:testInfo.outputPath(name+'-model-picker.png'),animations:'disabled'});
    await page.keyboard.press('Escape');
  }
});
