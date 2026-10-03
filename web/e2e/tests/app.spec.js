// clat web e2e — worklist PWA-4 验收①②③④。
// 宿主：global-setup 拉起的门控 cargo 测试（真 clat serve + TestProvider）。
const { test, expect } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const zlib = require('zlib');

function hostInfo(key) {
  const stateDir = process.env.CLAT_E2E_RUN_DIR || path.join(__dirname, '..');
  return JSON.parse(
    fs.readFileSync(path.join(stateDir, `.serve-${key}.json`), 'utf8'),
  );
}

async function openWorkbench(page, entry) {
  await page.goto(`${entry.origin}/`);
  await expect(page).toHaveURL(`${entry.origin}/`);
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await page.fill('#connect-token', entry.token);
  await page.click('#connect-form button[type="submit"]');
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
}

async function chooseModel(page, label) {
  await page.click('#model-picker-trigger');
  await expect(page.locator('#model-picker')).toBeVisible(LIVE);
  await page.getByRole('radio', { name: 'Use ' + label, exact: true }).click();
  await expect(page.locator('#model-picker')).not.toBeVisible(LIVE);
}

async function openDetails(page) {
  if (await page.locator('#inspector-toggle').getAttribute('aria-expanded') !== 'true') {
    await page.click('#inspector-toggle');
  }
}

async function revealWorkRecord(record) {
  await expect(record).toBeAttached(LIVE);
  const group = record.locator('xpath=ancestor::details[contains(@class,"work-summary")]');
  if (await group.getAttribute('open') === null) await group.locator(':scope > summary').click();
}

const LIVE = { timeout: 30_000 };

test('WEB-3 conversation defaults to open reading space and respects saved details', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await expect(page.locator('#inspector-toggle')).toHaveAttribute('aria-expanded', 'false');
  await expect(page.locator('#inspector')).toHaveAttribute('inert', '');
  await page.click('#inspector-toggle');
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#inspector-toggle')).toHaveAttribute('aria-expanded', 'true');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#inspector-toggle')).toHaveAttribute('aria-expanded', 'false');
});

test('WEB-3 idle session switch clears its progress label without erasing a newer notice', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await expect(page.locator('#run-state')).toBeEmpty();
  await page.evaluate(() => {
    setSwitching(true);
    updateRunState('send failed: retain this notice');
    setSwitching(false);
  });
  await expect(page.locator('#run-state')).toHaveText('send failed: retain this notice');
});

test('WEB-3 process grouping agrees in live and replay and does not span a history fragment', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  const result = await page.evaluate(() => {
    const call = { name: 'read_file', arguments: { path: 'file.rs' } };
    const reset = () => handleFrame({ event: 'replay.begin', data: '{}' });
    const live = [
      { type: 'tool_requested', call },
      { type: 'permission_checked', tool: 'read_file', decision: { allow: null } },
      { type: 'tool_finished', result: { tool_name: 'read_file', output: 'ok', is_error: false } },
    ];
    const replay = [
      { type: 'tool_requested', call },
      { type: 'permission_checked', tool: 'read_file', decision: { allow: null } },
      { type: 'tool_finished', tool: 'read_file', output: 'ok', is_error: false },
    ];
    const snapshot = () => [...dom.transcript.querySelectorAll('.work-records > *')]
      .map((node) => [node.className, node.textContent]);
    reset(); live.forEach(handleLive); const liveView = snapshot();
    reset(); replay.forEach(handleReplay); const replayView = snapshot();
    const tail = dom.transcript.lastElementChild;
    tail.open = true;
    const fragment = document.createDocumentFragment();
    state.history.replayTarget = fragment;
    replay.forEach(handleReplay);
    state.history.replayTarget = null;
    const separate = fragment.lastElementChild !== tail && tail.dataset.records === '3';
    dom.transcript.prepend(fragment);
    handleLive({ type: 'tool_requested', call });
    return { liveView, replayView, separate, groups: dom.transcript.childElementCount,
      tailKept: dom.transcript.lastElementChild === tail && tail.open && tail.dataset.records === '4' };
  });
  expect(result.liveView).toEqual(result.replayView);
  expect(result.separate).toBe(true);
  expect(result.groups).toBe(2);
  expect(result.tailKept).toBe(true);
});

test('WEB-3 display preferences survive reload without storing conversation content', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.click('#settings-open');
  await page.getByText('Single column', { exact: true }).click();
  await page.getByText('All steps', { exact: true }).click();
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem('clat.presentation.v1')));
  expect(saved).toMatchObject({ messageLayout: 'column', processView: 'expanded' });
  expect(Object.keys(saved).sort()).toEqual(['inspector', 'messageLayout', 'processView', 'sidebar', 'theme']);
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#app')).toHaveAttribute('data-message-layout', 'column');
  await page.evaluate(() => addToolCard('read_file', 'file.rs', null, false));
  await expect(page.locator('.work-summary').last()).toHaveAttribute('open', '');
});

test('WEB-3 background session repaint restores its own focus and never steals the draft focus', async ({ page }) => {
  await openWorkbench(page, hostInfo('history'));
  await expect(page.locator('.session-item').first()).toBeVisible(LIVE);
  const focus = await page.evaluate(() => {
    const button = dom['session-list'].querySelector('.session-item');
    openSessionMenu(state.sessions[0], 20, 20, button);
    renderSessions();
    const returned = document.activeElement === dom['session-list'].querySelector('.session-item');
    openSessionMenu(state.sessions[0], 20, 20, dom['session-list'].querySelector('.session-item'));
    window.dispatchEvent(new Event('resize'));
    const resized = document.activeElement === dom['session-list'].querySelector('.session-item');
    dom.prompt.focus();
    renderSessions();
    return { returned, resized, retained: document.activeElement === dom.prompt };
  });
  expect(focus).toEqual({ returned: true, resized: true, retained: true });
});

test('WEB-3 reading scale and small text contrast hold across themes and mobile', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => {
    addUserMessage('请解释这个表格，并保留中文标题。');
    const reply = addAssistantMessage();
    reply.appendBody('## 判断\n正文可读。\n\n| 项目 | 结论 |\n| --- | --- |\n| 本地 | 保留 |\n\n```rust\nlet ready = true;\n```');
    reply.finishBody();
  });
  await expect(page.locator('.msg.assistant .body').last()).toHaveCSS('font-size', '16px');
  await expect(page.locator('.rich-text pre').last()).toHaveCSS('font-size', '14px');
  await expect(page.locator('.rich-text table').last()).toHaveCSS('font-size', '15px');
  for (const theme of ['light', 'dark', 'system']) {
    await page.emulateMedia({ colorScheme: 'light' });
    const ratios = await page.evaluate((value) => {
      document.documentElement.dataset.theme = value;
      const styles = getComputedStyle(document.documentElement);
      const luminance = (name) => {
        const hex = styles.getPropertyValue(name).trim().slice(1);
        const rgb = [0, 2, 4].map((offset) => parseInt(hex.slice(offset, offset + 2), 16) / 255)
          .map((v) => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4);
        return rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722;
      };
      const text = luminance('--text-muted');
      return ['--ink-1', '--ink-2', '--surface-input', '--sidebar-bg'].map((name) => {
        const bg = luminance(name);
        return (Math.max(text, bg) + .05) / (Math.min(text, bg) + .05);
      });
    }, theme);
    for (const ratio of ratios) expect(ratio).toBeGreaterThanOrEqual(4.5);
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator('#prompt')).toHaveCSS('font-size', '16px');
  await expect(page.locator('#composer-permission')).toBeVisible();
  await expect(page.locator('#send')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('WEB-3 layout variants preserve drafts and disclosure node identity', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => {
    addUserMessage('长中文提问 '.repeat(20));
    addToolCard('read_file', 'file.rs', null, false);
    addNoticeLine('read_file → allowed', 'permission_checked');
    const reply = addAssistantMessage();
    reply.appendBody('结论'); reply.finishBody();
  });
  const group = page.locator('.work-summary').last();
  await expect(group).toBeAttached({ timeout: 3000 });
  await expect(group).not.toHaveAttribute('open');
  await group.locator(':scope > summary').click();
  await group.locator('.tool-card summary').click();
  await page.fill('#prompt', '尚未提交的草稿');
  await page.click('#settings-open');
  await page.getByText('Single column', { exact: true }).click();
  await page.getByText('All steps', { exact: true }).click();
  await expect(page.locator('#app')).toHaveAttribute('data-message-layout', 'column');
  await expect(group.locator('.tool-card')).toHaveAttribute('open', '');
  await page.getByText('Work summaries', { exact: true }).click();
  await page.getByText('User bubbles', { exact: true }).click();
  await page.keyboard.press('Escape');
  await expect(page.locator('#prompt')).toHaveValue('尚未提交的草稿');
  await expect(page.locator('#app')).toHaveAttribute('data-message-layout', 'bubbles');
  await expect(group.locator('.tool-card')).toHaveAttribute('open', '');
});

test('WEB-3 work summaries never contain answers interactive cards errors or retry signals', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => {
    handleFrame({ event: 'replay.begin', data: '{}' });
    const events = [
      { type: 'user_message', seq: 1, turn: 1, text: 'inspect' },
      { type: 'tool_requested', seq: 2, call: { name: 'read_file', arguments: {} } },
      { type: 'permission_checked', seq: 3, tool: 'read_file', decision: 'allow' },
      { type: 'tool_finished', seq: 4, tool: 'read_file', output: 'ok' },
      { type: 'assistant_message', seq: 5, text: 'answer' },
      { type: 'permission_checked', seq: 6, tool: 'bash', decision: 'deny' },
      { type: 'retry_scheduled', seq: 7, retry: 1, delay_ms: 10 },
      { type: 'tool_finished', seq: 8, tool: 'bash', output: 'broken', is_error: true },
      { type: 'turn_ended', seq: 9, turn: 1, reason: { error: 'failed on replay' } },
      { type: 'turn_ended', seq: 10, turn: 1, reason: 'blocked' },
      { type: 'turn_ended', seq: 11, turn: 1, reason: { aborted: 'user' } },
      { type: 'turn_ended', seq: 12, turn: 1, reason: 'max_tokens' },
      { type: 'turn_ended', seq: 13, turn: 1, reason: 'interrupted' },
    ];
    events.forEach(handleReplay);
    onApprovalRequested({ rpc_id: 'web3-approval', request: { tool: 'bash' } });
    onQuestionNotice({ kind: 'question_requested', payload: {
      rpc_id: 'web3-question', question: { question: '继续吗？', options: [], allow_custom: true },
    } });
  });
  await expect(page.locator('.work-summary')).toHaveCount(1);
  await expect(page.locator('.work-summary .tool-card')).toHaveCount(2);
  await expect(page.locator('.work-summary .msg, .work-summary .approval-card, .work-summary .question-card')).toHaveCount(0);
  await expect(page.locator('.tool-card.is-error')).toBeVisible();
  await expect(page.locator('.trace-event', { hasText: 'Retry' })).toBeVisible();
  await expect(page.locator('.trace-event', { hasText: 'bash' })).toBeVisible();
  await expect(page.locator('.trace-event', { hasText: 'failed on replay' })).toBeVisible();
  for (const reason of ['Blocked', 'Aborted', 'Max Tokens', 'Interrupted']) {
    await expect(page.locator('.trace-event', { hasText: reason })).toBeVisible();
  }
  await expect(page.locator('.approval-card')).toBeVisible();
  await expect(page.locator('.question-card')).toBeVisible();
  await expect(page.locator('.transcript > .msg[data-seq="5"]')).toBeVisible();
});

test('WEB-3 same-content visual matrix keeps wide content and composer inside the viewport', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.route('**/api/workbench.info', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.value.model.model = '中文长模型名 · DeepSeek-V4-Long-Context-Preview';
    await route.fulfill({ response, json: body });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.click('#new-session');
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
  const shot = async (name) => page.screenshot({ path: testInfo.outputPath(name + '.png'), animations: 'disabled' });
  const scrollTo = async (selector) => {
    await page.locator('#transcript-scroll').evaluate(async (viewport, targetSelector) => {
      viewport.style.scrollBehavior = 'auto';
      const target = targetSelector ? [...viewport.querySelectorAll(targetSelector)].at(-1) : null;
      viewport.scrollTop = target ? viewport.scrollTop + target.getBoundingClientRect().top
        - viewport.getBoundingClientRect().top - 16 : 0;
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    }, selector);
  };
  await page.click('#settings-open');
  await page.getByText('Light', { exact: true }).click();
  await page.keyboard.press('Escape');
  await scrollTo(null);
  await shot('desktop-light-bubbles');
  await page.locator('.work-summary > summary').click();
  await page.locator('.tool-card').first().locator('summary').click();
  await shot('desktop-light-expanded-tools');
  await page.locator('.work-summary > summary').click();
  await page.click('#settings-open');
  await page.getByText('Single column', { exact: true }).click();
  await page.keyboard.press('Escape');
  await shot('desktop-light-column');
  await page.click('#settings-open');
  await page.getByText('User bubbles', { exact: true }).click();
  await page.getByText('Dark', { exact: true }).click();
  await page.keyboard.press('Escape');
  await shot('desktop-dark-bubbles');
  for (const width of [390, 320, 768]) {
    await page.setViewportSize({ width, height: 844 });
    await page.fill('#prompt', '下一句：请继续核对失败路径。');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const bodyBox = await page.locator('.msg.assistant .body').last().boundingBox();
    expect(bodyBox.width).toBeGreaterThan(width < 760 ? width - 50 : 300);
    for (const selector of ['#prompt', '#send', '#composer-permission', '#model-picker-trigger']) {
      const box = await page.locator(selector).boundingBox();
      expect(box.x).toBeGreaterThanOrEqual(0);
      expect(box.x + box.width).toBeLessThanOrEqual(width);
      expect(box.y + box.height).toBeLessThanOrEqual(844);
    }
    const model = await page.locator('#model-picker-trigger').boundingBox();
    const send = await page.locator('#send').boundingBox();
    expect(model.x + model.width).toBeLessThanOrEqual(send.x);
    await page.evaluate(() => show(dom.cancel));
    const activeModel = await page.locator('#model-picker-trigger').boundingBox();
    const activeSend = await page.locator('#send').boundingBox();
    const stop = await page.locator('#cancel').boundingBox();
    expect(stop.x + stop.width).toBeLessThanOrEqual(activeModel.x);
    expect(activeModel.x + activeModel.width).toBeLessThanOrEqual(activeSend.x);
    expect(activeSend.x + activeSend.width).toBeLessThanOrEqual(width);
    await page.evaluate(() => hide(dom.cancel));
    await scrollTo(null);
    await shot('responsive-' + width);
    await scrollTo('.rich-text .table-scroll');
    const table = await page.locator('.rich-text .table-scroll').last().boundingBox();
    const tableComposer = await page.locator('#composer-shell').boundingBox();
    expect(table.y).toBeGreaterThanOrEqual(0);
    expect(table.y + table.height).toBeLessThan(tableComposer.y);
    await shot('wide-table-' + width);
    await page.locator('#transcript-scroll').evaluate((node) => { node.scrollTop = node.scrollHeight; });
    const lastLine = await page.locator('.verdict.completed').boundingBox();
    const composer = await page.locator('#composer-shell').boundingBox();
    expect(lastLine.y + lastLine.height).toBeLessThan(composer.y);
  }
  // A 640×450 CSS viewport exercises the reflow space of a 1280×900 window
  // at 200% desktop zoom. This is not a claim of native browser zoom acceptance.
  await page.setViewportSize({ width: 640, height: 450 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const zoomModel = await page.locator('#model-picker-trigger').boundingBox();
  const zoomSend = await page.locator('#send').boundingBox();
  expect(zoomModel.x + zoomModel.width).toBeLessThanOrEqual(zoomSend.x);
  for (const selector of ['#mobile-sidebar-open', '#inspector-toggle', '#prompt', '#send']) {
    const box = await page.locator(selector).boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(640);
    expect(box.y + box.height).toBeLessThanOrEqual(450);
  }
  await scrollTo('.rich-text .table-scroll');
  await shot('desktop-200-percent-equivalent');
  await page.evaluate(() => {
    addToolCard('run_command', '← permission failed', null, true);
    onApprovalRequested({ rpc_id: 'visual-approval', request: {
      tool: 'run_command', effect: 'workspace write', reason: '等待你的确认', arguments: { command: 'echo inspect' },
    } });
    onQuestionNotice({ kind: 'question_requested', payload: {
      rpc_id: 'visual-question', question: { question: '下一步检查哪个边界？',
        options: [{ label: '权限', description: '核对拒绝与失效路径' }], allow_custom: true },
    } });
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await scrollTo('.tool-card.is-error');
  await shot('mobile-approval-and-error');
  await scrollTo('.question-card');
  const question = await page.locator('.question-card').boundingBox();
  const questionComposer = await page.locator('#composer-shell').boundingBox();
  expect(question.y).toBeGreaterThanOrEqual(0);
  expect(question.y + question.height).toBeLessThan(questionComposer.y);
  await shot('mobile-question');
  await page.evaluate(() => addUserMessage('这是一条长中文提问，用来比较用户消息中的代码与长文本。'.repeat(8)
    + '\n\n```rust\nlet permission = InteractivePermissionPolicy::new(approver);\n```'));
  await page.setViewportSize({ width: 1280, height: 900 });
  await scrollTo('.msg.user');
  await shot('desktop-long-user-code');
  await page.click('#settings-open');
  await page.getByText('Single column', { exact: true }).click();
  await page.keyboard.press('Escape');
  await shot('desktop-long-user-code-column');
});

test('model picker hit area follows its label while long labels stay bounded beside Send', async ({ page }, testInfo) => {
  let model = { model: 'Qwen', thinking_level: 'low' };
  await page.route('**/api/workbench.info', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    Object.assign(body.value.model, model);
    await route.fulfill({ response, json: body });
  });
  await page.setViewportSize({ width: 1280, height: 900 });
  await openWorkbench(page, hostInfo('success'));
  await expect(page.locator('#model-picker-label')).toHaveText('Qwen · low', LIVE);
  await page.fill('#prompt', '模型切换不能改变这条草稿');
  const trigger = page.locator('#model-picker-trigger');
  const short = await trigger.boundingBox();
  const chevron = await trigger.locator('.picker-chevron').boundingBox();
  // Short labels must not leave a large invisible clickable tail after the arrow.
  expect(short.x + short.width - chevron.x - chevron.width).toBeLessThanOrEqual(10);
  await trigger.hover();
  await page.screenshot({ path: testInfo.outputPath('short-label-hover.png'), animations: 'disabled' });
  model = { model: 'DeepSeek', thinking_level: 'medium' };
  await page.evaluate(() => refreshWorkbench());
  await expect(page.locator('#model-picker-label')).toHaveText('DeepSeek · medium');
  const medium = await trigger.boundingBox();
  expect(medium.width).toBeGreaterThan(short.width + 8);
  await page.screenshot({ path: testInfo.outputPath('medium-label-hover.png'), animations: 'disabled' });
  model = { model: 'Qwen', thinking_level: 'low' };
  await page.evaluate(() => refreshWorkbench());
  expect((await trigger.boundingBox()).width).toBeCloseTo(short.width, 0);
  await trigger.click();
  await expect(page.locator('#model-picker')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(trigger).toBeFocused();
  model = { model: '中文长模型名 · DeepSeek-V4-Long-Context-Preview', thinking_level: 'high' };
  await page.evaluate(() => refreshWorkbench());
  for (const width of [1280, 768, 640, 390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    for (const running of [false, true]) {
      await page.evaluate((active) => active ? show(dom.cancel) : hide(dom.cancel), running);
      const picker = await trigger.boundingBox();
      const seat = await page.locator('.model-picker-seat').boundingBox();
      const send = await page.locator('#send').boundingBox();
      expect(picker.width).toBeLessThanOrEqual(width <= 420 ? 112 : width <= 760 ? 148 : 210);
      expect(picker.x).toBeGreaterThanOrEqual(0);
      expect(picker.x + picker.width).toBeCloseTo(seat.x + seat.width, 0);
      expect(send.x - picker.x - picker.width).toBeGreaterThanOrEqual(5);
      expect(send.x - picker.x - picker.width).toBeLessThanOrEqual(7);
      expect(send.x + send.width).toBeLessThanOrEqual(width);
      expect(await page.locator('#model-picker-label').evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
      if (running) {
        const stop = await page.locator('#cancel').boundingBox();
        expect(stop.x + stop.width).toBeLessThanOrEqual(picker.x);
      }
    }
  }
  await page.screenshot({ path: testInfo.outputPath('long-label-mobile.png'), animations: 'disabled' });
  for (const viewport of [{ width: 640, height: 450 }, { width: 390, height: 844 }, { width: 320, height: 844 }]) {
    await page.setViewportSize(viewport);
    await trigger.click();
    await expect(page.locator('#model-picker')).toBeVisible();
    await expect(page.locator('#model-picker-list .model-choice')).not.toHaveCount(0, LIVE);
    const popup = await page.locator('#model-picker').boundingBox();
    const header = await page.locator('.conversation-header').boundingBox();
    const composer = await page.locator('#composer-shell').boundingBox();
    expect(popup.x).toBeGreaterThanOrEqual(0);
    expect(popup.x + popup.width).toBeLessThanOrEqual(viewport.width);
    expect(popup.y).toBeGreaterThanOrEqual(header.y + header.height);
    expect(popup.y + popup.height).toBeLessThan(composer.y);
    await page.screenshot({ path: testInfo.outputPath('mobile-picker-open-' + viewport.width + '.png'), animations: 'disabled' });
    await page.keyboard.press('Escape');
    await expect(trigger).toBeFocused();
  }
  await expect(page.locator('#prompt')).toHaveValue('模型切换不能改变这条草稿');
});

test('settings categories isolate panels and composer links select their destination', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('success'));
  await page.click('#settings-open');
  for (const category of ['appearance', 'projects', 'models', 'utility', 'permissions', 'wechat']) {
    await page.click(`[data-settings-category="${category}"]`);
    await expect(page.locator('[data-settings-panel]:visible')).toHaveCount(1);
    await expect(page.locator(`[data-settings-panel="${category}"]`)).toBeVisible();
  }
  await page.keyboard.press('Escape');
  await page.click('#composer-permission');
  await expect(page.locator('[data-settings-panel="permissions"]')).toBeVisible();
  await page.keyboard.press('Escape');
  await page.click('#model-picker-trigger');
  await page.click('#model-picker-manage');
  await expect(page.locator('[data-settings-panel="models"]')).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('categorized-settings.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.click('[data-settings-category="permissions"]');
  await expect(page.locator('[data-settings-panel="permissions"]')).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('mobile-settings.png'), fullPage: true });
});

test('session actions support right click keyboard and more button', async ({ page, context }, testInfo) => {
  await openWorkbench(page, hostInfo('history'));
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  const row = page.locator('.session-item').first();
  await expect(row).toBeVisible(LIVE);
  await row.click({ button: 'right' });
  await expect(page.getByRole('menu', { name: 'Session actions' })).toBeVisible();
  await expect(page.getByRole('menuitem', { name: 'Copy session ID' })).toBeEnabled();
  await page.screenshot({ path: testInfo.outputPath('session-actions.png'), fullPage: true });
  await page.keyboard.press('Escape');
  await expect(row).toBeFocused();
  // A background snapshot may close a now-stale menu and repaint its source.
  // Keyboard focus must return to the same session, not to a detached button.
  await page.keyboard.press('Shift+F10');
  await expect(page.getByRole('menu')).toBeVisible();
  await page.evaluate(() => renderSessions());
  await expect(row).toBeFocused();
  await page.keyboard.press('Shift+F10');
  await expect(page.getByRole('menu')).toBeVisible();
  await page.getByRole('menuitem', { name: 'Copy session ID' }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toMatch(/^[0-9a-f-]+$/i);
  expect(copied.length).toBeGreaterThan(16);
  await page.locator('.session-more').first().click();
  const prompt = page.waitForEvent('dialog');
  await Promise.all([
    page.getByRole('menuitem', { name: 'Rename session' }).click(),
    prompt.then(async (dialog) => {
      expect(dialog.message()).toBe('Session title');
      await dialog.dismiss();
    }),
  ]);
  await page.locator('.session-more').first().click();
  await page.getByRole('menuitem', { name: 'Open session' }).click();
  await expect(page.getByRole('menu')).toHaveCount(0);
});

test('model intensity uses host choices and preserves the composer', async ({ page }, testInfo) => {
  let intensity = 'high';
  await page.route('**/api/model.settings.get', async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.value.current.thinking_levels = ['low', 'high', 'max'];
    body.value.current.thinking_level = intensity;
    await route.fulfill({ response, json: body });
  });
  await page.route('**/api/model.overrides.set', async (route) => {
    expect(route.request().postDataJSON()).toMatchObject({ field: 'thinking_level', state: 'set', value: 'max' });
    intensity = 'max';
    await route.fulfill({ json: { ok: true, value: {} } });
  });
  await openWorkbench(page, hostInfo('success'));
  await page.fill('#prompt', 'Keep this draft');
  await page.click('#model-picker-trigger');
  await expect(page.locator('#model-thinking')).toHaveValue('high', LIVE);
  await page.selectOption('#model-thinking', 'max');
  await expect(page.locator('#model-thinking')).toBeEnabled(LIVE);
  await expect(page.locator('#model-thinking')).toHaveValue('max');
  await expect(page.locator('#prompt')).toHaveValue('Keep this draft');
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.screenshot({ path: testInfo.outputPath('model-intensity-dark.png'), fullPage: true });
});

test('unsupported thinking stays legible and details actions preserve drafts', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.fill('#prompt', 'Do not submit this draft');
  await page.click('#model-picker-trigger');
  await expect(page.locator('#model-thinking')).toBeDisabled();
  await expect(page.locator('#model-thinking option:checked')).toHaveText('Not adjustable');
  await page.click('#model-picker-close');
  await openDetails(page);
  await page.click('#inspector-refresh');
  await expect(page.locator('#inspector-refresh')).toBeEnabled(LIVE);
  await page.click('#inspector-context');
  await expect(page.locator('.context-notice')).toHaveCount(1, LIVE);
  await expect(page.locator('#prompt')).toHaveValue('Do not submit this draft');
  await page.click('#inspector-models');
  await expect(page.locator('[data-settings-panel="models"]')).toBeVisible();
});

test('details context response cannot land in a newly selected session', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await openDetails(page);
  let release;
  const barrier = new Promise((resolve) => { release = resolve; });
  let received;
  const arrived = new Promise((resolve) => { received = resolve; });
  await page.route('**/api/command.run', async (route) => {
    const response = await route.fetch();
    received();
    await barrier;
    await route.fulfill({ response });
  });
  await page.click('#inspector-context');
  await arrived;
  await page.click('#new-session');
  await expect(page.locator('#new-session')).toBeEnabled(LIVE);
  release();
  await expect(page.locator('#inspector-context')).toBeEnabled(LIVE);
  await expect(page.locator('.context-notice')).toHaveCount(0);
  await expect(page.locator('#inspector-feedback')).toBeEmpty();
});

test('question cards replay to another client and first answer closes both', async ({ page, context }, testInfo) => {
  const entry = hostInfo('question');
  await openWorkbench(page, entry);
  await page.fill('#prompt', 'ask me');
  await page.click('#send');
  const card = page.locator('.question-card').last();
  await expect(card).toBeVisible(LIVE);
  await card.getByRole('button', { name: 'Decline', exact: true }).scrollIntoViewIfNeeded();
  await expect(card.locator('.question-description')).toHaveText('recommended');
  await page.screenshot({ path: testInfo.outputPath('question-desktop.png'), fullPage: true, animations: 'disabled' });
  const other = await context.newPage();
  await other.goto(entry.origin);
  await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(other.locator('.question-card')).toHaveCount(1);
  await other.reload();
  await expect(other.locator('.question-card')).toHaveCount(1, LIVE);
  await other.locator('.question-card').getByRole('button', { name: 'stable', exact: true }).click();
  await expect(card.locator('button')).toHaveCount(0, LIVE);
  await expect(other.locator('.question-card button')).toHaveCount(0, LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await other.close();
});

test('question response failure retains custom input and retries only explicitly', async ({ page }) => {
  const entry = hostInfo('question');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await page.fill('#prompt', 'ask again');
  await page.click('#send');
  const card = page.locator('.question-card').last();
  await expect(card).toBeVisible(LIVE);
  let calls = 0;
  await page.route('**/api/question.respond', async (route) => {
    calls++;
    if (calls === 1) await route.fulfill({ status: 503, body: 'temporarily unavailable' });
    else await route.continue();
  });
  await card.getByRole('textbox', { name: 'Your answer' }).fill('my own answer');
  await card.getByRole('button', { name: 'Send answer', exact: true }).click();
  await expect(card.locator('.resolution')).toContainText('Answer retained', LIVE);
  await expect(card.getByRole('textbox')).toHaveValue('my own answer');
  expect(calls).toBe(1);
  await card.getByRole('button', { name: 'Send answer', exact: true }).click();
  await expect(card.locator('button')).toHaveCount(0, LIVE);
  expect(calls).toBe(2);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
});

test('question notices deduplicate safely and stale failures cannot reopen a resolved card', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('success'));
  await page.setViewportSize({ width: 390, height: 844 });
  const requested = (id) => ({ kind: 'question_requested', payload: {
    rpc_id: id, question: { question: '<img src=x onerror=alert(1)>', options: [], allow_custom: false },
  } });
  await page.evaluate((notice) => { onNotice(notice); onNotice(notice); }, requested('first'));
  await expect(page.locator('.question-card')).toHaveCount(1);
  await expect(page.locator('.question-card img')).toHaveCount(0);
  let release;
  const barrier = new Promise((resolve) => { release = resolve; });
  let started;
  const inFlight = new Promise((resolve) => { started = resolve; });
  let calls = 0;
  await page.route('**/api/question.respond', async (route) => {
    calls++;
    started();
    await barrier;
    await route.fulfill({ status: 503, body: 'delayed failure' });
  });
  const first = page.locator('.question-card').first();
  await first.getByRole('button', { name: 'Send answer', exact: true }).click();
  await expect(first.locator('.resolution')).toContainText('nonblank');
  expect(calls).toBe(0);
  await first.getByRole('textbox').fill('retained draft');
  await first.getByRole('button', { name: 'Send answer', exact: true }).click();
  await inFlight;
  await expect(first.getByRole('button', { name: 'Decline', exact: true })).toBeDisabled();
  await page.evaluate((notice) => {
    onNotice({ kind: 'question_resolved', payload: { rpc_id: 'first' } });
    onNotice(notice);
  }, requested('second'));
  const second = page.locator('.question-card').last();
  await second.getByRole('textbox').fill('new question draft');
  await page.screenshot({ path: testInfo.outputPath('question-mobile.png'), fullPage: true, animations: 'disabled' });
  release();
  await expect(first).toHaveAttribute('data-sending', 'false');
  await expect(first.locator('button')).toHaveCount(0);
  await expect(second.getByRole('textbox')).toHaveValue('new question draft');
  await page.evaluate(() => closeQuestionCards());
  await expect(page.locator('.question-card button')).toHaveCount(0);
  expect(calls).toBe(1);
});

test('question options without custom input allow explicit decline', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => onNotice({ kind: 'question_requested', payload: {
    rpc_id: 'options-only', question: { question: 'Proceed?',
      options: [{ label: 'Yes', description: 'Continue work' }], allow_custom: false },
  } }));
  const card = page.locator('.question-card').last();
  await expect(card.getByRole('textbox')).toHaveCount(0);
  await page.route('**/api/question.respond', async (route) => {
    expect(route.request().postDataJSON()).toEqual({ rpcId: 'options-only', answer: { kind: 'declined' } });
    await route.fulfill({ json: { ok: true, value: {} } });
  });
  await card.getByRole('button', { name: 'Decline', exact: true }).click();
  await expect(card.locator('.resolution')).toHaveText('Answer accepted.');
  await expect(card.locator('button')).toHaveCount(0);
});

test('reconnect during approval shows each identical admission once', async ({ page, context }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  for (let round = 1; round <= 2; round++) {
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
    await page.fill('#prompt', 'same admitted input');
    await page.click('#send');
    await expect(page.locator('.approval-card').last()).toBeVisible(LIVE);
    const other = await context.newPage();
    await other.goto(entry.origin);
    await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(other.locator('.approval-card').last()).toBeVisible(LIVE);
    await expect(other.locator('.msg.user')).toHaveCount(round, LIVE);
    await other.reload();
    await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(other.locator('.approval-card').last()).toBeVisible(LIVE);
    await expect(other.locator('.msg.user')).toHaveCount(round, LIVE);
    await other.locator('.approval-card').last().locator('button.ghost').click();
    await expect(other.locator('#detail-run')).toHaveText('Idle', LIVE);
    await other.reload();
    await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(other.locator('.msg.user')).toHaveCount(round, LIVE);
    await other.close();
  }
  // This fixture is shared with the original fresh-session acceptance test.
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
});

test('model profiles are editable in PWA and broadcast without echoing API keys', async ({ page, context }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);
  await page.click('#model-picker-trigger');
  await expect(page.locator('#model-picker-list .model-choice')).not.toHaveCount(0);
  await page.click('#model-picker-close');
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  const other = await context.newPage();
  await other.goto(entry.origin);
  await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
  await other.click('#settings-open');
  await other.click('[data-settings-category="models"]');
  const name = 'browser-profile-' + Date.now();
  const secret = 'browser-key-do-not-return';
  const responseBodies = [];
  page.on('response', (response) => {
    if (response.url().includes('/api/model.')) responseBodies.push(response.text());
  });
  await page.locator('#model-profile-editor summary').click();
  await page.fill('#model-profile-name', name);
  await page.fill('#model-profile-model', 'browser-test-model');
  await page.fill('#model-profile-endpoint', 'https://example.invalid/v1');
  await page.fill('#model-profile-key', secret);
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', LIVE);
  await expect(page.locator('#model-profile-key')).toHaveValue('');
  await page.locator('#settings-dialog button[aria-label="Close settings"]').click();
  await chooseModel(page, name);
  await expect(other.locator('#model-current')).toContainText('browser-test-model', LIVE);
  await expect(other.locator('#model-current')).toContainText('key set');
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  await page.getByRole('button', { name: 'Edit profile ' + name, exact: true }).click();
  await expect(page.locator('#model-profile-key-state')).toContainText('Key is set', LIVE);
  await expect(page.locator('#model-profile-key')).toHaveValue('');
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', LIVE);
  await expect(other.locator('#model-current')).toContainText('key set');
  await page.check('#model-profile-clear-key');
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', LIVE);
  await page.getByRole('button', { name: 'Edit profile ' + name, exact: true }).click();
  await expect(page.locator('#model-profile-key-state')).toContainText('Key is not set', LIVE);
  await page.locator('#settings-dialog button[aria-label="Close settings"]').click();
  await chooseModel(page, name);
  await expect(other.locator('#model-current')).toContainText('key not set', LIVE);
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  await page.getByRole('button', { name: 'Edit profile ' + name, exact: true }).click();
  await page.fill('#model-profile-model', 'browser-edited-model');
  await page.click('#model-profile-save');
  await expect(page.locator('#model-settings-saved')).toContainText('Profile saved', LIVE);
  await page.locator('#settings-dialog button[aria-label="Close settings"]').click();
  await chooseModel(page, name);
  await expect(other.locator('#model-current')).toContainText('browser-edited-model', LIVE);
  const responses = await Promise.all(responseBodies);
  expect(responses.every((response) => !response.includes(secret))).toBeTruthy();
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Delete profile ' + name, exact: true }).click();
  await other.locator('#settings-dialog button[aria-label="Close settings"]').click();
  await other.click('#model-picker-trigger');
  await expect(other.getByRole('radio', { name: 'Use ' + name, exact: true })).toHaveCount(0, LIVE);
  await other.close();
});

test('companion utility policy and manual suggestion stay preview-only', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await expect(page.locator('#suggest')).toBeDisabled();
  await expect(page.locator('#suggest')).toHaveAccessibleName('Generate prompt suggestion');
  await expect(page.locator('#suggest svg')).toHaveCount(1);
  await page.click('#settings-open');
  await page.click('[data-settings-category="utility"]');
  await expect(page.locator('#utility-naming-enabled')).toBeChecked();
  await page.check('#utility-suggestions-enabled');
  await page.click('#utility-settings-save');
  await expect(page.locator('#utility-settings-saved')).toContainText('saved', LIVE);
  await page.click('#settings-dialog .icon-button');
  await page.fill('#prompt', 'Review the current session state.');
  await page.click('#send');
  await expect(page.locator('.msg.user')).toHaveCount(1, LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', { timeout: 30_000 });
  await expect(page.locator('#prompt')).toHaveValue('');
  await expect(page.locator('#suggest')).toBeEnabled();
  await page.click('#suggest');
  await expect(page.locator('#suggestion-panel')).toBeVisible(LIVE);
  const suggestion = await page.locator('#suggestion-text').textContent();
  expect(suggestion.trim()).not.toBe('');
  await page.click('#suggestion-use');
  await expect(page.locator('#prompt')).toHaveValue(suggestion.trim());
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#settings-open');
  await page.click('[data-settings-category="utility"]');
  await page.uncheck('#utility-suggestions-enabled');
  await page.click('#utility-settings-save');
  await expect(page.locator('#utility-settings-saved')).toContainText('saved', LIVE);
  await page.click('#settings-dialog .icon-button');
  await expect(page.locator('#suggest')).toBeDisabled();
});

test('delayed utility policy reads cannot overwrite a saved policy', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  await page.click('#new-session');
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await expect(page.locator('#suggest')).toBeDisabled();

  // 2026-09-16 病历的两类全量负载时序，本测试各钉一条：
  // ①保存前发出的策态读取在保存完成后才落地（晚于保存触发的新鲜
  //   重取），旧响应成为最后一次写入，把刚保存的策略打回旧值；
  // ②后台刷新重绘表单，把用户未保存的编辑（uncheck）复位。
  const gates = [];
  await page.route('**/api/model.utility.get', async (route) => {
    const gate = gates.shift();
    if (!gate) {
      await route.continue();
      return;
    }
    const response = await route.fetch();
    await gate.promise;
    await route.fulfill({ response });
  });

  await page.click('#settings-open');
  await page.click('[data-settings-category="utility"]');
  await expect(page.locator('#utility-naming-enabled')).toBeChecked();

  let releaseStaleRead;
  gates.push({ promise: new Promise((resolve) => { releaseStaleRead = resolve; }) });
  await page.evaluate(() => { refreshUtilitySettings() });
  await page.check('#utility-suggestions-enabled');
  await page.click('#utility-settings-save');
  await expect(page.locator('#utility-settings-saved')).toContainText('saved', LIVE);
  await page.click('#settings-dialog .icon-button');
  await page.fill('#prompt', 'Review the current session state.');
  await page.click('#send');
  await expect(page.locator('.msg.user')).toHaveCount(1, LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', { timeout: 30_000 });
  await expect(page.locator('#prompt')).toHaveValue('');
  await expect(page.locator('#suggest')).toBeEnabled();
  let releaseFreshRead;
  gates.push({ promise: new Promise((resolve) => { releaseFreshRead = resolve; }) });
  await page.evaluate(() => { refreshUtilitySettings() });
  releaseStaleRead();
  // 让扣住的旧响应落地并被消费——未修复时它会把策态打回旧值。
  await page.waitForTimeout(500);
  await expect(page.locator('#suggest')).toBeEnabled();
  releaseFreshRead();

  // ②后台重绘不覆盖用户未保存的编辑：扣住保存后的新鲜读取（内容为
  // ON），uncheck 之后放行——重绘不得把勾选框复位。
  let releasePostEditRead;
  gates.push({ promise: new Promise((resolve) => { releasePostEditRead = resolve; }) });
  await page.evaluate(() => { refreshUtilitySettings() });
  await page.click('#settings-open');
  await page.click('[data-settings-category="utility"]');
  await expect(page.locator('#utility-suggestions-enabled')).toBeChecked();
  await page.uncheck('#utility-suggestions-enabled');
  releasePostEditRead();
  // 未修复时这条新鲜读取落地会把勾选框复位成 ON（渲染覆盖用户编辑）
  await page.waitForTimeout(500);
  await expect(page.locator('#utility-suggestions-enabled')).not.toBeChecked();
  await page.click('#utility-settings-save');
  await expect(page.locator('#utility-settings-saved')).toContainText('saved', LIVE);
  await page.click('#settings-dialog .icon-button');
  await expect(page.locator('#suggest')).toBeDisabled();
});

test('composer keeps suggestion in context and model selection directly before Send', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('success'));
  const order = await page.evaluate(() => ({
    suggestParent: document.querySelector('#suggest').parentElement.className,
    pickerBeforeSend: document.querySelector('#model-picker-trigger')
      .compareDocumentPosition(document.querySelector('#send')) & Node.DOCUMENT_POSITION_FOLLOWING,
    settingsHasActivation: Boolean(document.querySelector('#model-preset-select'))
      || Boolean(document.querySelector('[aria-label^="Use profile "]')),
  }));
  expect(order).toEqual({ suggestParent: 'composer-context', pickerBeforeSend: 4, settingsHasActivation: false });
  await page.click('#model-picker-trigger');
  await expect(page.locator('#model-picker')).toBeVisible();
  await expect(page.locator('#model-picker')).toContainText('Built-in presets');
  await expect(page.locator('#model-picker')).toContainText('NEXT RUN');
  await page.screenshot({ path: testInfo.outputPath('composer-model-picker.png'), animations: 'disabled' });
  await page.keyboard.press('Escape');
  await expect(page.locator('#model-picker')).not.toBeVisible();
  await expect(page.locator('#model-picker-trigger')).toBeFocused();
});

for (const transition of ['input', 'new', 'close']) {
test(`delayed profile reads cannot overwrite newer form state: ${transition}`, async ({ page }, testInfo) => {
  const entry = hostInfo('success');
  const name = 'mf2-stale-profile';
  const api = (method, data) => page.request.post(`${entry.origin}/api/${method}`, {
    headers: { Authorization: `Bearer ${entry.token}` }, data,
  });
  await api('model.profile.save', { name, protocol: 'open_ai_compatible', model: 'old-model',
    endpoint: 'https://example.invalid/v1', request_path: '/chat/completions' });
  await openWorkbench(page, entry);
  await page.click('#settings-open');
  await page.click('[data-settings-category="models"]');
  await page.locator('#model-profile-editor summary').click();
  let release;
  const barrier = new Promise((resolve) => { release = resolve; });
  let requested;
  const started = new Promise((resolve) => { requested = resolve; });
  await page.route('**/api/model.profile.get', async (route) => {
    const response = await route.fetch();
    requested();
    await barrier;
    await route.fulfill({ response });
  });
  const edit = page.getByRole('button', { name: 'Edit profile ' + name, exact: true });
  await page.fill('#model-profile-model', 'newer-user-draft');
  await page.fill('#model-profile-key', 'unsaved-key');
  await page.check('#model-profile-clear-key');
  await edit.click();
  await started;
  if (transition === 'input') {
    await page.fill('#model-profile-model', 'newer-user-draft-after-read');
  } else if (transition === 'new') {
    await page.click('#model-profile-new');
    await expect(page.locator('#model-profile-name')).toBeFocused();
    await expect(page.locator('#model-profile-key')).toHaveValue('');
    await expect(page.locator('#model-profile-clear-key')).not.toBeChecked();
  } else if (transition === 'close') {
    await page.keyboard.press('Escape');
    await expect(page.locator('#settings-dialog')).not.toBeVisible();
    await page.click('#settings-open');
    await page.click('[data-settings-category="models"]');
  }
  release();
  await expect(edit).toBeEnabled(LIVE);
  const expected = { input: 'newer-user-draft-after-read', new: '', close: 'newer-user-draft' };
  await expect(page.locator('#model-profile-model')).toHaveValue(expected[transition]);
  if (transition === 'new') await page.screenshot({ path: testInfo.outputPath('new-profile.png') });
  await api('model.profile.delete', { name });
});
}

test('project tabs keep drafts and permission state isolated', async ({ page, context }) => {
  const entry = hostInfo('success');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-tab-'));
  try {
    await openWorkbench(page, entry);
    await page.fill('#prompt', 'Keep this draft in the first project');
    await page.click('#settings-open');
    await page.click('[data-settings-category="projects"]');
    await page.fill('#workspace-root', directory);
    await page.click('#workspace-open');
    await expect(page.locator('#workspace-error')).toContainText('not trusted', LIVE);
    await page.check('#workspace-trust');
    await page.click('#workspace-open');
    const link = page.locator('#workspace-error a');
    await expect(link).toBeVisible(LIVE);
    const other = await context.newPage();
    await other.goto(entry.origin + await link.getAttribute('href'));
    await expect(other.locator('#conn-status')).toHaveText('live', LIVE);
    await other.click('#settings-open');
    await other.click('[data-settings-category="permissions"]');
    await other.locator('label').filter({ has: other.locator('input[name="permission-mode"][value="read-only"]') }).click();
    await other.click('#permission-save');
    await expect(other.locator('#settings-saved')).toContainText('updated', LIVE);
    await expect(page.locator('#prompt')).toHaveValue('Keep this draft in the first project');
    await expect(page.locator('input[name="permission-mode"][value="workspace-write"]')).toBeChecked();
    await other.close();
    await page.locator('#settings-dialog button[aria-label="Close settings"]').click();
    await expect(page.locator('#conn-status')).toHaveText('live');
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
});

function currentRssBytes(pid) {
  const kib = Number.parseInt(
    execFileSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' }).trim(),
    10,
  );
  if (!Number.isFinite(kib)) throw new Error(`invalid RSS for pid ${pid}`);
  return kib * 1024;
}

async function measureRss(pid, action) {
  const idleRssBytes = currentRssBytes(pid);
  let peakRssBytes = idleRssBytes;
  const started = Date.now();
  const sampler = setInterval(() => {
    peakRssBytes = Math.max(peakRssBytes, currentRssBytes(pid));
  }, 50);
  try {
    await action();
    await new Promise((resolve) => setTimeout(resolve, 100));
    peakRssBytes = Math.max(peakRssBytes, currentRssBytes(pid));
  } finally {
    clearInterval(sampler);
  }
  return { idleRssBytes, peakRssBytes, elapsedMs: Date.now() - started };
}

function crc32(parts) {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = (value >>> 1) ^ ((value & 1) ? 0xedb88320 : 0);
    }
    table[index] = value >>> 0;
  }
  let crc = 0xffffffff;
  for (const part of parts) {
    for (const byte of part) crc = table[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const name = Buffer.from(type, 'ascii');
  const chunk = Buffer.allocUnsafe(data.length + 12);
  chunk.writeUInt32BE(data.length, 0);
  name.copy(chunk, 4);
  data.copy(chunk, 8);
  chunk.writeUInt32BE(crc32([name, data]), chunk.length - 4);
  return chunk;
}

function solidPng(width, height, rgb) {
  const rowBytes = 1 + (width * 3);
  const raw = Buffer.alloc(rowBytes * height);
  for (let y = 0; y < height; y += 1) {
    const row = y * rowBytes;
    raw[row] = 0;
    for (let x = 0; x < width; x += 1) {
      const offset = row + 1 + (x * 3);
      raw[offset] = rgb[0];
      raw[offset + 1] = rgb[1];
      raw[offset + 2] = rgb[2];
    }
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 2;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', header),
    pngChunk('IDAT', zlib.deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

function makeLiveColorFixtures() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-live-glm-pwa-'));
  const green = path.join(root, 'green.png');
  const yellow = path.join(root, 'yellow.png');
  fs.writeFileSync(green, solidPng(256, 256, [0, 210, 0]));
  fs.writeFileSync(yellow, solidPng(256, 256, [235, 225, 0]));
  return { root, green, yellow };
}

function makeNearLimitPngFixtures() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'clat-mm5-pwa-'));
  const source = fs.readFileSync(path.join(__dirname, '..', '..', 'icons', 'icon-512.png'));
  if (source.subarray(source.length - 8, source.length - 4).toString('ascii') !== 'IEND') {
    throw new Error('fixture source does not end in IEND');
  }
  const targetBytes = (8 * 1024 * 1024) - 4096;
  const payloadBytes = targetBytes - source.length - 12;
  const type = Buffer.from('rNDm', 'ascii'); // ancillary, private, reserved bit valid
  const paths = [];
  for (let imageIndex = 0; imageIndex < 4; imageIndex += 1) {
    const payload = Buffer.allocUnsafe(payloadBytes);
    let state = (0x9e3779b9 ^ imageIndex) >>> 0;
    for (let offset = 0; offset < payload.length; offset += 1) {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      payload[offset] = state & 0xff;
    }
    const chunk = Buffer.allocUnsafe(payload.length + 12);
    chunk.writeUInt32BE(payload.length, 0);
    type.copy(chunk, 4);
    payload.copy(chunk, 8);
    chunk.writeUInt32BE(crc32([type, payload]), chunk.length - 4);
    const output = Buffer.concat([source.subarray(0, source.length - 12), chunk, source.subarray(source.length - 12)]);
    const fixturePath = path.join(root, `near-limit-${imageIndex + 1}.png`);
    fs.writeFileSync(fixturePath, output);
    paths.push(fixturePath);
  }
  return { root, paths, rawBytes: targetBytes * paths.length };
}

async function dispatchImageFileGesture(page, selector, eventName, fixturePath) {
  const base64 = fs.readFileSync(fixturePath).toString('base64');
  await page.evaluate(({ targetSelector, type, bytes }) => {
    const binary = atob(bytes);
    const data = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) data[index] = binary.charCodeAt(index);
    const transfer = new DataTransfer();
    transfer.items.add(new File([data], 'gesture.png', { type: 'image/png' }));
    const event = type === 'paste'
      ? new ClipboardEvent(type, { bubbles: true, cancelable: true, clipboardData: transfer })
      : new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: transfer });
    document.querySelector(targetSelector).dispatchEvent(event);
  }, { targetSelector: selector, type: eventName, bytes: base64 });
}

// —— 验收①：一轮真实 run（流式 → 工具卡 → 审批 Allow → 终审）——
test.describe('acceptance ① approval + run lifecycle', () => {
  test('prompt → approval allow → tool executes → settled completed', async ({ page }) => {
    const entry = hostInfo('run-command');
    await openWorkbench(page, entry);

    await page.fill('#prompt', 'run echo please');
    await page.click('#send');
    await expect(page.locator('.msg.user .body')).toHaveText('run echo please', LIVE);

    const card = page.locator('.approval-card').first();
    await expect(card).toBeVisible(LIVE);
    await expect(card.locator('.title')).toContainText('run_command', LIVE);
    await card.locator('button.primary').click(); // Allow

    await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
    const results = page.locator('.tool-card', { hasText: 'run_command' });
    await expect(results).toHaveCount(2, LIVE);
    await revealWorkRecord(results.last());
    await expect(results.last()).toBeVisible(LIVE);
  });

  // —— 验收①的 Deny 腿 + 验收②：会话管理（新会话 → 拒绝 → 侧栏/重命名）
  test('fresh session → approval deny → no tool → sessions + rename', async ({ page }) => {
    const entry = hostInfo('run-command');
    await openWorkbench(page, entry);

    // 新会话（上一条测试的会话已含 run_command 结果——模型不再请求
    // 工具；新会话保证审批确定性）。等待重建完成的确定性屏障：历史
    // user 消息被清（重放骨架清空）+ composer 解锁（切换防抖）——
    // 点击不等待异步处理器，直接发 prompt 会与 session.new 跨连接
    // 竞态（e2e 实锤：prompt.send 先落地 → run 活跃 → new 被拒）。
    await page.click('#new-session');
    await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.fill('#prompt', 'try a command');
    await page.click('#send');

    const card = page.locator('.approval-card').first();
    await expect(card).toBeVisible(LIVE);
    await card.locator('button.ghost').click(); // Deny

    await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
    // 被拒调用「从不执行」：只有模型请求卡（1 张），没有执行结果卡
    //（tool_finished 才带结果体）。
    await expect(page.locator('.tool-card')).toHaveCount(1);

    // 当前会话进入侧栏，刷新后仍能从 journal 重建。宿主在其他
    // 用例中也会产生会话，不能把整个侧栏数量钉为 2。
    await expect(page.locator('#session-list li.active')).toHaveCount(1, LIVE);
    await page.reload();
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(page.locator('#session-list li.active')).toHaveCount(1, LIVE);
    await expect(page.locator('.msg.user .body').last()).toHaveText('try a command', LIVE);

    // 重命名（对话窗 handle prompt 对话框）。
    page.once('dialog', (dialog) => dialog.accept('renamed by e2e'));
    await page.click('#session-title');
    await expect(page.locator('#session-title')).toHaveText('renamed by e2e', LIVE);
  });
});

test('settings expose the default-deny WeChat binding surface', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);

  await page.click('#settings-open');
  await page.click('[data-settings-category="wechat"]');
  await expect(page.locator('#settings-dialog')).toBeVisible(LIVE);
  await expect(page.locator('.wechat-settings h3')).toHaveText('WeChat remote control');
  await expect(page.locator('#wechat-status')).toHaveText('Not bound', LIVE);
  await expect(page.locator('#wechat-counts')).toHaveText('0 paired · 0 chats');
  await expect(page.locator('#wechat-bind')).toHaveText('Bind WeChat');
  await expect(page.locator('#wechat-bind')).toBeEnabled();
  await expect(page.locator('#wechat-pair')).toBeDisabled();
  await expect(page.locator('#wechat-unbind')).toBeDisabled();
  await expect(page.locator('#wechat-qr')).toBeHidden();
  await expect(page.locator('#wechat-pairing')).toBeHidden();
});

// —— 验收③：run 进行中 F5 → 视图完整恢复、流式继续（INV-W3）——
test('refresh mid-run rebuilds the view and streaming continues', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);

  await page.fill('#prompt', 'long stream');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  // F5：视图只从重放 + 活流重建（不依赖任何本地会话状态）。
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('.msg.user .body')).toHaveText('long stream', LIVE);
  await expect(page.locator('.msg.assistant .body').first()).toBeVisible(LIVE);

  // 流式继续：重建后的正文仍在增长，最终 settled。
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 8_000;
    },
    null,
    LIVE,
  );
  await expect(page.locator('.verdict.completed')).toBeVisible({
    timeout: 60_000,
  });
});

// —— 回归：应用外壳高度约束——转录区在视口内滚动、composer 永不出屏 ——
// 2026-08-24 bug：.app 网格只定义列未定义行，唯一隐式行按内容 auto 计高；
// 转录一长整个会话列被撑出视口——转录无法滚动、composer（输入框）被推
// 出屏。修复：.app { grid-template-rows: minmax(0, 1fr) } 把行钉在容器高。
test('app shell stays bounded under long content', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);

  await page.fill('#prompt', 'long stream');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const bodies = document.querySelectorAll('.msg.assistant .body');
      return bodies.length > 0 && bodies[bodies.length - 1].textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  const shell = await page.evaluate(async () => {
    const sc = document.querySelector('.transcript-scroll');
    sc.style.scrollBehavior = 'auto';
    sc.scrollTop = sc.scrollHeight;
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const composer = document.querySelector('.composer').getBoundingClientRect();
    const messages = [...document.querySelectorAll('.transcript > .msg')];
    const lastMessage = messages.at(-1)?.getBoundingClientRect();
    return {
      transcriptClient: sc.clientHeight,
      transcriptScroll: sc.scrollHeight,
      composerBottom: composer.bottom,
      composerPosition: getComputedStyle(document.querySelector('.composer')).position,
      lastMessageBottom: lastMessage && lastMessage.bottom,
      composerTop: composer.top,
      viewport: window.innerHeight,
    };
  });
  expect(shell.transcriptClient).toBeLessThan(shell.viewport);
  expect(shell.transcriptScroll).toBeGreaterThan(shell.transcriptClient);
  expect(shell.composerBottom).toBeLessThanOrEqual(shell.viewport);
  expect(shell.composerPosition).toBe('absolute');
  expect(shell.lastMessageBottom).toBeLessThanOrEqual(shell.composerTop);
  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
});

test('tail history opens at the newest message and prepends without moving the reading anchor', async ({ page }) => {
  const entry = hostInfo('history');
  let historyRequests = 0;
  page.on('request', (request) => {
    if (request.url().endsWith('/api/session.history')) historyRequests += 1;
  });
  await openWorkbench(page, entry);

  await expect(page.locator('.transcript > .msg')).toHaveCount(50, LIVE);
  await expect(page.locator('.message-map-item')).toHaveCount(56, LIVE);
  await expect(page.locator('.msg.user .body').last()).toHaveText('history question 27', LIVE);
  await expect(page.locator('#history-status')).toBeVisible(LIVE);
  const anchor = await page.evaluate(() => {
    const viewport = document.querySelector('#transcript-scroll').getBoundingClientRect();
    const rows = [...document.querySelectorAll('.transcript > [data-seq]')];
    const row = rows.find((candidate) => candidate.getBoundingClientRect().bottom >= viewport.top);
    return { seq: row.dataset.seq, top: row.getBoundingClientRect().top };
  });

  await page.click('#history-status');
  await expect(page.locator('.transcript > .msg')).toHaveCount(56, LIVE);
  await expect(page.locator('#history-status')).toBeHidden(LIVE);
  const anchoredTop = await page.locator(`.transcript > [data-seq="${anchor.seq}"]`).evaluate(
    (row) => row.getBoundingClientRect().top,
  );
  expect(Math.abs(anchoredTop - anchor.top)).toBeLessThanOrEqual(2);
  expect(historyRequests).toBe(1);
});

test('conversation map previews a turn and loads an unloaded message before jumping', async ({ page }) => {
  const entry = hostInfo('history');
  let historyRequests = 0;
  page.on('request', (request) => {
    if (request.url().endsWith('/api/session.history')) historyRequests += 1;
  });
  await openWorkbench(page, entry);
  const firstBar = page.locator('.message-map-item').first();
  await firstBar.hover();
  await expect(page.locator('#message-map-preview')).toContainText('history question 00', LIVE);
  const seq = await firstBar.getAttribute('data-seq');
  await firstBar.click();
  await expect(page.locator(`.transcript > .msg[data-seq="${seq}"]`)).toBeVisible(LIVE);
  expect(historyRequests).toBe(1);
});

test('conversation map keeps a safe scrollbar gutter and a continuous hover target', async ({ page }) => {
  const entry = hostInfo('history');
  await openWorkbench(page, entry);
  const bars = page.locator('.message-map-item');
  await expect(bars).toHaveCount(56, LIVE);

  const geometry = await page.evaluate(() => {
    const viewport = document.querySelector('#transcript-scroll').getBoundingClientRect();
    const rail = document.querySelector('#message-map').getBoundingClientRect();
    return { scrollbarGutter: viewport.right - rail.right };
  });
  expect(geometry.scrollbarGutter).toBeGreaterThanOrEqual(20);

  await bars.nth(0).hover();
  await expect(page.locator('#message-map-preview')).toBeVisible();
  const first = await bars.nth(0).boundingBox();
  const second = await bars.nth(1).boundingBox();
  // Move through the visual space between two strokes. The hit rows must be
  // continuous, so the preview never blinks off while the pointer is on rail.
  await page.mouse.move(
    first.x + first.width / 2,
    (first.y + first.height + second.y) / 2,
  );
  await expect(page.locator('#message-map-preview')).toBeVisible();
  await page.mouse.move(second.x + second.width / 2, second.y + second.height / 2);
  await expect(page.locator('#message-map-preview')).toBeVisible();
});

test('message Markdown is readable without turning model text into executable HTML', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(() => {
    const bubble = addAssistantMessage();
    bubble.node.dataset.web2 = 'markdown';
    bubble.appendBody('**streaming**');
    window.web2Bubble = bubble;
  });
  const body = page.locator('[data-web2="markdown"] .body');
  await expect(body).toHaveText('**streaming**');
  await expect(body.locator('strong')).toHaveCount(0);
  await page.evaluate(() => {
    window.web2Bubble.appendBody('\n\n## Result\n- one\n- two\n\n```js\nconst x = 1\n```'
      + '\n\n<script id="web2-attack">alert(1)</script>'
      + '\n\n[bad](javascript:alert(1)) [good](https://example.com)'
      + '\n\n| Name | Count |\n| :--- | ---: |\n| **Ada** | 2 |'
      + '\n| `a\\|b` | <img id="table-attack" src=x onerror=alert(1)> |');
    window.web2Bubble.finishBody();
  });
  await expect(body.locator('strong').first()).toHaveText('streaming');
  await expect(body.locator('h3')).toHaveText('Result');
  await expect(body.locator('li')).toHaveCount(2);
  await expect(body.locator('pre code')).toHaveText('const x = 1');
  expect(await body.locator('pre code').evaluate((node) => getComputedStyle(node).borderTopWidth)).toBe('0px');
  await expect(body).toContainText('<script id="web2-attack">');
  await expect(page.locator('#web2-attack')).toHaveCount(0);
  await expect(body.locator('a')).toHaveCount(1);
  await expect(body.locator('a')).toHaveAttribute('href', 'https://example.com');
  await expect(body.locator('table')).toHaveCount(1);
  await expect(body.locator('th')).toHaveCount(2);
  await expect(body.locator('tbody tr')).toHaveCount(2);
  await expect(body.locator('tbody tr').last().locator('code')).toHaveText('a|b');
  await expect(body.locator('tbody tr').last()).toContainText('<img id="table-attack"');
  await expect(page.locator('#table-attack')).toHaveCount(0);
  expect(await body.locator('th').last().evaluate((node) => getComputedStyle(node).textAlign)).toBe('right');
  await page.screenshot({ path: testInfo.outputPath('web2-markdown.png'), fullPage: true });

  // A later model phase may resume the same bubble after an earlier response.
  await page.evaluate(() => {
    window.web2Bubble.appendBody('\n\nFinal `note`');
    window.web2Bubble.finishBody();
  });
  await expect(body.locator('code')).toHaveCount(3);
  await expect(body).toContainText('Final note');
});

test('long conversation map keeps bounded DOM and can jump from a virtual window', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('history'));
  const target = await page.locator('.msg.user[data-seq]').last().getAttribute('data-seq');
  await page.evaluate((targetSeq) => {
    state.history.outline = Array.from({ length: 5000 }, (_, index) => ({
      seq: index === 2500 ? Number(targetSeq) : 10000 + index,
      turn: Math.floor(index / 2) + 1,
      role: index % 2 ? 'assistant' : 'user',
      preview: index === 2500 ? '## **Jump** [here](https://example.com)' : `message ${index}`,
    }));
    renderMessageMap();
    const track = document.querySelector('#message-map-track');
    track.scrollTop = 2500 * MAP_ITEM_HEIGHT;
  }, target);
  const bars = page.locator('.message-map-item');
  await expect(page.locator(`.message-map-item[data-seq="${target}"]`)).toBeVisible(LIVE);
  expect(await bars.count()).toBeLessThanOrEqual(120);
  const selected = page.locator(`.message-map-item[data-seq="${target}"]`);
  await selected.hover();
  await expect(page.locator('#message-map-preview')).toContainText('Jump here');
  await expect(page.locator('#message-map-preview')).not.toContainText('**');
  const userPreview = page.locator('#message-map-preview .map-preview-row.is-user');
  const agentPreview = page.locator('#message-map-preview .map-preview-row.is-assistant');
  await expect(userPreview).toContainText('Jump here');
  await expect(agentPreview).toContainText('message 2501');
  const previewColors = await page.locator('#message-map-preview').evaluate((preview) => ({
    user: getComputedStyle(preview.querySelector('.is-user')).color,
    assistant: getComputedStyle(preview.querySelector('.is-assistant')).color,
  }));
  expect(previewColors.user).not.toBe(previewColors.assistant);
  await page.screenshot({ path: testInfo.outputPath('web2-map-preview.png'), fullPage: true });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await page.screenshot({ path: testInfo.outputPath('web2-map-preview-dark.png'), fullPage: true });
  await selected.click();
  await expect(page.locator(`.transcript > .msg[data-seq="${target}"]`)).toBeVisible(LIVE);
  expect(await bars.count()).toBeLessThanOrEqual(120);
  await page.locator('#message-map-track').focus();
  await page.keyboard.press('Home');
  await expect(page.locator('.message-map-item[data-seq="10000"]')).toBeVisible(LIVE);
  await page.keyboard.press('End');
  await expect(page.locator('.message-map-item[data-seq="14999"]')).toBeVisible(LIVE);
  await page.locator('#message-map-track').evaluate((track) => { track.scrollTop = 2500 * 9; });
  await page.setViewportSize({ width: 1440, height: 1100 });
  const bottomVisibleSeq = await page.locator('#message-map-track').evaluate((track) =>
    10000 + Math.ceil((track.scrollTop + track.clientHeight) / 9) - 1);
  await expect(page.locator(`.message-map-item[data-seq="${bottomVisibleSeq}"]`)).toBeAttached(LIVE);
});

test('scrolling within 512px of the top automatically loads one earlier page', async ({ page }) => {
  const entry = hostInfo('history');
  let historyRequests = 0;
  page.on('request', (request) => {
    if (request.url().endsWith('/api/session.history')) historyRequests += 1;
  });
  await openWorkbench(page, entry);
  await expect(page.locator('.transcript > .msg')).toHaveCount(50, LIVE);
  await page.locator('#transcript-scroll').evaluate((viewport) => { viewport.scrollTop = 400; });
  await expect(page.locator('.transcript > .msg')).toHaveCount(56, LIVE);
  expect(historyRequests).toBe(1);
});

test('Think disclosure follows the latest live line, then the first settled line', async ({ page }) => {
  const entry = hostInfo('reasoning');
  let historyRequests = 0;
  page.on('request', (request) => {
    if (request.url().endsWith('/api/session.history')) historyRequests += 1;
  });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openWorkbench(page, entry);
  await page.fill('#prompt', 'show reasoning');
  await page.click('#send');
  const reasoning = page.locator('.msg.assistant .reasoning').last();
  await expect(reasoning).toHaveAttribute('data-running', 'true', LIVE);
  expect(await reasoning.locator('summary').evaluate(
    (summary) => getComputedStyle(summary, '::before').animationName,
  )).toBe('none');
  await expect(reasoning.locator('.reasoning-preview')).toHaveText('latest thought', LIVE);
  await expect(reasoning).not.toHaveAttribute('data-running', 'true', LIVE);
  await expect(reasoning.locator('.reasoning-preview')).toHaveText('first thought', LIVE);
  await reasoning.locator('summary').click();
  await expect(reasoning.locator('.reasoning-copy')).toHaveText('first thought\nlatest thought', LIVE);
  await expect(page.locator('.message-map-item')).toHaveCount(2, LIVE);
  await expect(page.locator('.msg.user[data-seq]')).toHaveCount(1, LIVE);
  await expect(page.locator('.msg.assistant[data-seq]')).toHaveCount(1, LIVE);
  await page.locator('.message-map-item').last().click();
  expect(historyRequests).toBe(0);

  // Audit F1: this is a fresh replay, not the live node above. Settled Think
  // must still summarize with its first non-empty line.
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  const replayed = page.locator('.msg.assistant .reasoning').last();
  await expect(replayed.locator('.reasoning-preview')).toHaveText('first thought', LIVE);
  await expect(replayed.locator('.reasoning-copy')).toHaveText('first thought\nlatest thought', LIVE);
});

// —— 验收④：双标签页——同 run 双观察；首答即赢；次答者见 not-pending ——
test('dual tabs observe the same run; first answer wins', async ({ browser }) => {
  const entry = hostInfo('run-command');
  const tabA = await browser.newPage();
  const tabB = await browser.newPage();
  await openWorkbench(tabA, entry);
  await openWorkbench(tabB, entry);

  // 新会话（宿主历史里已有 run_command 结果，保证审批触发）；
  // 等待重建完成（同上：切换防抖屏障）。
  await tabA.click('#new-session');
  await expect(tabA.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(tabA.locator('#send')).toBeEnabled(LIVE);
  await tabA.fill('#prompt', 'dual tab run');
  await tabA.click('#send');

  const cardA = tabA.locator('.approval-card').first();
  const cardB = tabB.locator('.approval-card').first();
  await expect(cardA).toBeVisible(LIVE);
  await expect(cardB).toBeVisible(LIVE); // 双观察：两端都收到审批卡

  await cardA.locator('button.primary').click(); // A 先答
  await expect(tabA.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(tabB.locator('.verdict.completed')).toBeVisible(LIVE); // B 收敛

  // 广播已到达后仍可迟答；不能靠点在广播之前偶然通过。
  await expect(cardB.locator('.note.resolution')).toHaveText('answered or closed by host', LIVE);
  await expect(cardB.locator('button.primary')).toBeEnabled({ timeout: 5000 });

  await cardB.locator('button.primary').click(); // B 迟到应答 → not-pending
  await expect(cardB.locator('.note.resolution')).toHaveText('already answered elsewhere', LIVE);
  await expect(cardA.locator('.note.resolution')).toHaveText('answered: allow', LIVE);

  // 故意在 RPC 已确认后再交付一次通知，钉死另一种网络顺序。
  for (const tab of [tabA, tabB]) {
    await tab.evaluate(() => {
      const rpc_id = document.querySelector('.approval-card').dataset.rpcId;
      onApprovalResolved({ rpc_id });
    });
  }
  await expect(cardA.locator('.note.resolution')).toHaveText('answered: allow', LIVE);
  await expect(cardB.locator('.note.resolution')).toHaveText('already answered elsewhere', LIVE);

  await tabA.close();
  await tabB.close();
});

// —— Phase 4：公开市场只读投影；跨源请求绝不携带本地 Bearer token。——
test('plugin index panel is searchable, SVG-led, and never leaks the local token', async ({ page }) => {
  const entry = hostInfo('run-command');
  let catalogRequest;
  await page.route('https://pi.at.cn/catalog.json', async (route) => {
    catalogRequest = route.request();
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        schemaVersion: 1,
        market: { name: 'CLAT Plugin Index' },
        packages: [
          {
            id: 'dev.clat.digest',
            name: 'Digest Lab',
            runtime: 'wasm-component',
            status: 'preview',
            summary: 'Deterministic local digests.',
            tags: ['WASM', 'Rust'],
          },
          {
            id: 'cn.at.clat.dsh-port',
            name: 'DSH Porting Bridge',
            runtime: 'mcp-stdio',
            status: 'preview',
            summary: 'Cordis compatibility bridge.',
            tags: ['DSH', 'MCP'],
          },
        ],
      }),
    });
  });
  await openWorkbench(page, entry);
  await page.click('#market-open');
  await expect(page.locator('#market-dialog')).toBeVisible(LIVE);
  await expect(page.locator('.market-item')).toHaveCount(2);
  expect(catalogRequest).toBeTruthy();
  expect(catalogRequest.headers().authorization).toBeUndefined();
  expect(catalogRequest.headers().cookie).toBeUndefined();
  await page.fill('#market-search', 'DSH');
  await expect(page.locator('.market-item')).toHaveCount(1);
  await expect(page.locator('.market-item h3')).toHaveText('DSH Porting Bridge');
  await expect(page.locator('#market-dialog svg')).not.toHaveCount(0);
});

// 模型协议 ID 保留在 title 供诊断，视觉层显示可理解的事件名称。
test('model trace renders human-readable event names instead of raw protocol ids', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'trace labels');
  await page.click('#send');
  const trace = page.locator('.trace-event', { hasText: 'Model request started' }).first();
  await revealWorkRecord(trace);
  await expect(trace).toBeVisible(LIVE);
  await expect(trace).toHaveAttribute('title', 'Event ID: model_requested');
  await expect(trace).not.toContainText('model_requested');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);

  // PU-9（附加工单 2026-09-06 负责人令）：assistant 标记换品牌像素机
  // 器人剪影——fill 剪影（evenodd 挖出面部），不走全局描边管线。
  const agentGlyph = page.locator('.msg.assistant .marker svg path').first();
  await expect(agentGlyph).toHaveAttribute('d', /^M6 4h12v4/);
  await expect(agentGlyph).toHaveAttribute('fill-rule', 'evenodd');
  await expect(agentGlyph).toHaveAttribute('stroke', 'none');
});

test('image-capable model is marked in the top bar and model route row', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);

  for (const selector of ['#header-model', '#detail-model']) {
    const identity = page.locator(selector);
    await expect(identity.locator('.vision-capability')).toHaveCount(1);
    await expect(identity.locator('.vision-capability')).toHaveAttribute('aria-label', 'Accepts images');
    await expect(identity.locator('svg')).toHaveCount(1);
    const colors = await identity.locator('.vision-capability').evaluate((badge) => ({
      color: getComputedStyle(badge).color,
      stroke: getComputedStyle(badge.querySelector('svg')).stroke,
    }));
    expect(colors.stroke).toBe(colors.color);
  }
});

// FE-1：斜杠桥仍只返回 core 事实；PWA 负责格式化 context，并让 Plan Mode
// 在普通 notice 退场后仍有持续、可撤销的视觉状态。
test('context is readable and the plan-mode marker appears and clears', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);

  // /context 可在 Fresh 状态读取，但 durable Plan Mode 需要已物化会话。
  // 新建后跑一轮并拒绝 Execute，既得到真实 session，也不产生命令副作用。
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'materialize a session for plan mode');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('#detail-session')).not.toHaveText('Fresh', LIVE);

  await page.fill('#prompt', '/context');
  await page.click('#send');
  const context = page.locator('.context-notice').last();
  await expect(context).toBeVisible(LIVE);
  await expect(context).toContainText('Context estimate · tokens');
  for (const label of [
    'Base prompt:',
    'Project instructions:',
    'Plan policy:',
    'Skill catalog:',
    'Goal policy:',
    'Memory injection:',
    'Tool schemas:',
    'History / compaction view:',
    'Images:',
    'Images before projection:',
    'Older images omitted:',
    'Image bytes:',
    'Visual token estimate:',
    'Visual safety factor:',
    'Output reserve:',
    'Input estimate:',
    'Total estimate:',
  ]) {
    await expect(context).toContainText(label);
  }
  await expect(context).toContainText(/Goal policy: \d+ tokens · (injected|not injected)/);
  await expect(context).toContainText(/Memory injection: \d+ \/ \d+ bytes · (injected|not injected)/);
  await expect(context).not.toContainText('{"estimator"');

  await page.fill('#prompt', '/plan');
  await page.click('#send');
  const badge = page.locator('#plan-mode-badge');
  await expect(badge).toBeVisible(LIVE);
  await expect(badge).toHaveText('Plan');
  await expect(badge).toHaveCSS('color', 'rgb(240, 189, 90)');

  // 另一条命令完成后仍常显，不是转录区里一闪而过的 status notice。
  await page.fill('#prompt', '/context');
  await page.click('#send');
  await expect(page.locator('.context-notice')).toHaveCount(2, LIVE);
  await expect(badge).toBeVisible();

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(badge).toBeVisible(LIVE);
  // The second no-argument /plan toggles off, including after a cold reload.
  await page.fill('#prompt', '/plan');
  await page.click('#send');
  await expect(badge).toBeHidden(LIVE);
});

test('stale command completion preserves and submits newer composer input', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);

  // Plan Mode is durable, so materialize an otherwise fresh session first.
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'materialize the FL-F1 regression session');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('#detail-session')).not.toHaveText('Fresh', LIVE);

  let markRefreshBlocked;
  const refreshBlocked = new Promise((resolve) => { markRefreshBlocked = resolve; });
  let releaseRefresh;
  const refreshReleased = new Promise((resolve) => { releaseRefresh = resolve; });
  let holdNextRefresh = false;
  let heldRefresh = false;
  await page.route('**/api/workbench.info', async (route) => {
    if (!holdNextRefresh || heldRefresh) {
      await route.continue();
      return;
    }
    heldRefresh = true;
    const response = await route.fetch();
    const body = await response.json();
    body.value.session.title = 'FL-F1 refresh applied';
    markRefreshBlocked();
    await refreshReleased;
    await route.fulfill({ response, json: body });
  });

  holdNextRefresh = true;
  await page.fill('#prompt', '/context');
  await page.click('#send');
  await refreshBlocked;
  let sendPressed = false;
  try {
    await page.fill('#prompt', '/plan');
    const sendBox = await page.locator('#send').boundingBox();
    expect(sendBox).not.toBeNull();
    await page.mouse.move(sendBox.x + (sendBox.width / 2), sendBox.y + (sendBox.height / 2));
    await page.mouse.down();
    sendPressed = true;
    // Resume the stale /context tail between pointer-down and click. This
    // deterministically exercises the race seen in the FL-HUNT trace.
    releaseRefresh();

    // The title proves the old /context tail resumed and completed. It may clear
    // only the text that /context submitted, never the newer /plan generation.
    await expect(page.locator('#session-title')).toHaveText('FL-F1 refresh applied', LIVE);
    await expect(page.locator('#prompt')).toHaveValue('/plan');

    const planRequest = page.waitForRequest((request) => {
      if (!request.url().endsWith('/api/command.run')) return false;
      return request.postDataJSON().command === '/plan';
    });
    await page.mouse.up();
    sendPressed = false;
    await planRequest;
  } finally {
    releaseRefresh();
    if (sendPressed) await page.mouse.up();
  }
  await expect(page.locator('#plan-mode-badge')).toBeVisible(LIVE);
});

test('PU content notices and armed Goal badge follow core workflow state', async ({ page }, testInfo) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await openWorkbench(page, hostInfo('run-command'));
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await page.click('#new-session');
  await expect(page.locator('.msg.user')).toHaveCount(0, LIVE);
  await expect(page.locator('#plan-mode-badge')).toBeHidden();
  await expect(page.locator('#goal-badge')).toBeHidden();
  const command = async (text) => {
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.fill('#prompt', text);
    await page.click('#send');
    await expect(page.locator('#prompt')).toHaveValue('', LIVE);
  };
  await command('/help');
  await expect(page.locator('.notice-line', { hasText: '/help' }).last()).toBeVisible(LIVE);
  await command('/mem add project PU first line\nPU second line');
  await command('/mem list');
  const memory = page.locator('.content-notice[aria-label="memory"]').last();
  await expect(memory).toContainText('PU first line\nPU second line');
  await expect(memory.locator('span')).toHaveCSS('white-space', 'pre-wrap');
  await expect(memory).toContainText('Source:');
  await command('/sub on');
  await command('/sub status');
  const sub = page.locator('.content-notice[aria-label="subagent status"]').last();
  await expect(sub).toContainText('enabled');
  await expect(sub).toContainText('/sub off');
  await expect(sub).toContainText('delegate_readonly');
  await command('/goal create inspect PU changes --rounds 1');
  await command('/goal');
  await expect(page.locator('.content-notice[aria-label="goal"]').last()).toContainText('inspect PU changes');
  await expect(page.locator('#goal-badge')).toBeHidden();

  // Materialize before entering durable Plan Mode; an ordinary run disarms.
  await command('materialize PU workflow session');
  await expect(page.locator('.approval-card').last()).toBeVisible(LIVE);
  await page.locator('.approval-card').last().locator('button.ghost').click();
  // run_completed updates the UI before core settlement releases the run.
  // Wait for prompt.settled, not the transient Idle presentation alone.
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await command('/plan on');
  await page.fill('#prompt', '/goal run');
  await page.click('#send');
  await expect(page.locator('#run-state')).toContainText('exit plan mode first', LIVE);
  await expect(page.locator('#plan-mode-badge')).toBeVisible();
  await expect(page.locator('#goal-badge')).toBeHidden();
  await command('/plan off');
  await command('/goal run');
  const goalBadge = page.locator('#goal-badge');
  await expect(goalBadge).toBeVisible(LIVE);
  await expect(goalBadge).toHaveText('Goal');
  await expect(goalBadge).toHaveCSS('color', 'rgb(127, 219, 152)');
  await expect(page.locator('#plan-mode-badge')).toBeHidden();
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(goalBadge).toHaveCSS('color', 'rgb(33, 114, 59)');
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.screenshot({ path: testInfo.outputPath('pu-goal-workflow.png'), fullPage: true });
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(goalBadge).toBeVisible(LIVE);
  await page.click('#cancel');
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  await expect(goalBadge).toBeHidden(LIVE);
  await command('/goal');
  const goal = page.locator('.content-notice[aria-label="goal"]').last();
  await expect(goal).toContainText('paused');
  await expect(goal).toContainText('disarmed');
  await page.click('#new-session');
  await expect(page.locator('#goal-badge')).toBeHidden();
});

// WEB-3: workflow badges stay in context, permission is an always-visible
// toolbar control. Activating a badge must not move the permission control.
test('permission stays in the toolbar while Plan and Goal preserve their workflow order', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  const order = await page.evaluate(() => {
    const ids = ['composer-permission', 'plan-mode-badge', 'goal-badge'];
    const nodes = ids.map((id) => document.getElementById(id));
    if (nodes.some((node) => node === null)) return null;
    const follows = (a, b) =>
      Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
    return {
      permissionParent: nodes[0].parentElement.className,
      planParent: nodes[1].parentElement.className,
      goalParent: nodes[2].parentElement.className,
      planBeforeGoal: follows(nodes[1], nodes[2]),
    };
  });
  expect(order).toEqual({
    permissionParent: 'composer-row',
    planParent: 'composer-context',
    goalParent: 'composer-context',
    planBeforeGoal: true,
  });
  for (const id of ['plan-mode-badge', 'goal-badge']) {
    const badge = page.locator(`#${id}`);
    await expect(badge).toHaveCSS('margin-left', '7px');
    await expect(badge).toHaveCSS('margin-right', '0px');
  }
});

// 附加工单（2026-09-06 负责人验收附加）：侧栏连接指示与插件/设置
// 图标同尺寸同列位——前身 7px 圆点既不对齐也不随状态变色。状态色沿用
// 页眉 conn-status 同一套 success/warning/danger 语义。
test('sidebar connection indicator matches footer icon geometry and follows conn state', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  const entry = hostInfo('success');
  const footnote = page.locator('#sidebar-footnote');
  const footnoteIcon = page.locator('#sidebar-footnote > svg');

  // 无 token：HTML 初始态 connecting，警告色图标已在 17px 网格上。
  await page.goto(`${entry.origin}/`);
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'connecting');
  await expect(footnoteIcon).toHaveCSS('width', '17px');
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(240, 189, 90)');

  // 假 token：自动重连被 401 拒绝 → failed 态（真实断线路径）。
  await page.evaluate((token) => localStorage.setItem('clat.auth.v1', token), 'not-a-real-token');
  await page.reload();
  await expect(page.locator('#landing')).toBeVisible(LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'failed', LIVE);
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(255, 123, 112)');

  // 真 token：live 态 + 与上方两枚操作图标同宽同列、行高同网格。
  await page.fill('#connect-token', entry.token);
  await page.click('#connect-form button[type="submit"]');
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(footnote).toHaveAttribute('data-state', 'live');
  await expect(footnoteIcon).toHaveCSS('color', 'rgb(127, 219, 152)');
  // Solid inner dot + stroked outer ring: both track the state color.
  const [dotFill, ringStroke] = await footnoteIcon.evaluate((svg) => {
    const [dot, ring] = svg.querySelectorAll('circle');
    const dotStyle = getComputedStyle(dot);
    const ringStyle = getComputedStyle(ring);
    return [dotStyle.fill + ' ' + dotStyle.stroke, ringStyle.stroke + ' ' + ringStyle.fill];
  });
  expect(dotFill).toBe('rgb(127, 219, 152) none');
  expect(ringStroke).toBe('rgb(127, 219, 152) none');
  await expect(footnote).toHaveCSS('min-height', '36px');
  const [footBox, setBox, mktBox] = await Promise.all([
    footnoteIcon.boundingBox(),
    page.locator('#settings-open > svg').boundingBox(),
    page.locator('#market-open > svg').boundingBox(),
  ]);
  // The settings icon is the unbordered baseline column; the market entry's
  // own 1px border shifts its glyph right by a pixel by design.
  expect(footBox.x).toBeCloseTo(setBox.x, 1);
  expect(Math.abs(footBox.x - mktBox.x)).toBeLessThanOrEqual(1);
  expect(footBox.width).toBeCloseTo(setBox.width, 1);
  expect(footBox.width).toBeCloseTo(mktBox.width, 1);
});

test('collapsed sessions entry is a full tile and opens named sessions without changing the draft', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await openWorkbench(page, hostInfo('history'));
  await expect(page.locator('.session-item').first()).toBeVisible(LIVE);
  await page.fill('#prompt', '打开列表不是切换会话，也不能清草稿');
  const selected = await page.evaluate(() => state.sessionId);
  await page.click('#sidebar-toggle');
  await expect(page.locator('#app')).toHaveAttribute('data-sidebar', 'collapsed');
  await expect.poll(async () => (await page.locator('#sidebar').boundingBox()).width).toBe(68);
  await page.screenshot({ path: testInfo.outputPath('rail-before-open.png'), animations: 'disabled' });
  const entry = page.locator('#sessions-open');
  await expect(entry).toBeVisible({ timeout: 3000 });
  await expect(entry).toHaveAccessibleName('Open sessions');
  await expect(page.locator('.session-more').first()).toBeHidden();
  const tile = await entry.boundingBox();
  expect(tile.width).toBeGreaterThanOrEqual(44);
  expect(tile.height).toBeGreaterThanOrEqual(44);
  const original = await entry.evaluate((node) => getComputedStyle(node).backgroundColor);
  await page.mouse.move(tile.x + 3, tile.y + 3);
  await expect.poll(() => entry.evaluate((node) => getComputedStyle(node).backgroundColor)).not.toBe(original);
  expect(await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.closest('button')?.id,
    { x: tile.x + 3, y: tile.y + 3 })).toBe('sessions-open');
  await page.screenshot({ path: testInfo.outputPath('rail-entry-hover.png'), animations: 'disabled' });
  await page.mouse.click(tile.x + 3, tile.y + 3);
  await expect(page.locator('#app')).toHaveAttribute('data-sidebar', 'expanded');
  await expect(page.locator('#session-search')).toBeFocused();
  await expect(page.locator('.session-copy strong').first()).toBeVisible();
  expect(await page.evaluate(() => state.sessionId)).toBe(selected);
  await expect(page.locator('#prompt')).toHaveValue('打开列表不是切换会话，也不能清草稿');
  const more = page.locator('.session-more').first();
  await expect.poll(async () => (await page.locator('#sidebar').boundingBox()).width).toBe(258);
  const moreBox = await more.boundingBox();
  expect(moreBox.width).toBeGreaterThanOrEqual(32);
  expect(moreBox.height).toBeGreaterThanOrEqual(32);
  await page.mouse.click(moreBox.x + 3, moreBox.y + 3);
  await expect(page.getByRole('menu', { name: 'Session actions' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(more).toBeFocused();
  await page.click('#sidebar-toggle');
  await entry.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('#session-search')).toBeFocused();
  await page.click('#sidebar-toggle');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.click('#mobile-sidebar-open');
  await expect(page.locator('.session-copy strong').first()).toBeVisible();
  await expect(entry).toBeHidden();
  const search = await page.locator('.session-search').evaluate((node) => {
    const centers = [...node.children].map((child) => {
      const rect = child.getBoundingClientRect();
      return rect.y + rect.height / 2;
    });
    return Math.max(...centers) - Math.min(...centers);
  });
  expect(search).toBeLessThanOrEqual(1);
  await page.screenshot({ path: testInfo.outputPath('phone-named-sessions.png'), animations: 'disabled' });
  await expect(page.locator('#prompt')).toHaveValue('打开列表不是切换会话，也不能清草稿');
});

// PU-7 的图标轨对齐继续保持；2026-10-03 负责人反馈后，收起态
// 会话字块改为单一会话列表入口，单条会话只在展开列表中呈现。
test('collapsed rail centers the brand and toggle and unifies the icon grid', async ({ page }) => {
  await openWorkbench(page, hostInfo('success'));
  // Seed one real session so the tile row exists before collapsing.
  // The success host completes deterministically without a tool approval.
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'collapsed rail seed');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
  await expect(page.locator('.session-item').first()).toBeVisible(LIVE);

  await page.click('#sidebar-toggle');
  const probe = () => page.evaluate(() => {
    const box = (selector) => {
      const node = document.querySelector(selector);
      if (!node) throw new Error(`missing node: ${selector}`);
      const rect = node.getBoundingClientRect();
      return {
        left: rect.left,
        width: rect.width,
        height: rect.height,
        cx: rect.left + rect.width / 2,
        cy: rect.top + rect.height / 2,
      };
    };
    return {
      rail: box('#sidebar'),
      head: box('.sidebar-head'),
      brand: box('.brand-icon'),
      toggle: box('#sidebar-toggle'),
      fresh: box('#new-session'),
      tile: box('#sessions-open'),
      glyph: box('#sessions-open > svg'),
      settings: box('#settings-open'),
    };
  });
  // The rail resizes over a 180ms grid transition; retry until it settles.
  // Every icon shares one vertical axis (±1px absorbs the rail's 1px right
  // border shifting the content-box center by half a pixel). The toggle's
  // inner svg is measured too: UA button padding + the 180° rotation once
  // threw the glyph 5px off the shared axis while the button box stayed put.
  // The toggle also straddles the header's bottom border: its vertical
  // center sits on the divider line (box bottom minus the 1px border half).
  await expect(async () => {
    const g = await probe();
    const glyph = await page.locator('#sidebar-toggle > svg').boundingBox();
    const axis = g.brand.cx;
    const dividerY = g.head.cy + g.head.height / 2 - 0.5;
    const near = (a, b) => expect(Math.abs(a - b)).toBeLessThanOrEqual(1);
    near(axis, g.rail.left + g.rail.width / 2);
    near(g.toggle.cx, axis);
    near(glyph.x + glyph.width / 2, axis);
    near(g.toggle.cy, dividerY);
    near(g.fresh.cx, axis);
    near(g.tile.cx, axis);
    near(g.settings.cx, axis);
    expect(Math.abs(g.tile.width - g.fresh.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(g.tile.width - g.settings.width)).toBeLessThanOrEqual(1);
    expect(Math.abs(g.tile.height - g.fresh.height)).toBeLessThanOrEqual(1);
    near(g.glyph.cx, g.tile.cx);
    near(g.glyph.cy, g.tile.cy);
  }).toPass();
});

// PU-8（附加工单 2026-09-06 负责人验收附加）：Full Access 与 TUI 同语——
// 权限文字（页眉徽标 + 输入框上方 pill）转警示黄；其余模式保持默认色。
test('full access paints the permission text warning yellow', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await openWorkbench(page, hostInfo('success'));
  const pill = page.locator('#composer-permission');
  const badge = page.locator('#header-permission');

  await page.click('#settings-open');
  await page.click('[data-settings-category="permissions"]');
  await expect(page.locator('#settings-dialog')).toBeVisible(LIVE);
  // The dialog is a live view: an async workbench refresh re-renders the
  // radios from host state and can land between our clicks. Retry the whole
  // select→confirm→save unit until the save actually lands.
  const grantFullAccess = async () => {
    await page.click('#permission-options label:has(input[value="workspace-write"])', { timeout: 3000 });
    await page.click('#permission-options label:has(input[value="danger-full-access"])', { timeout: 3000 });
    await page.check('#full-access-confirm', { timeout: 3000 });
    await page.click('#permission-save', { timeout: 3000 });
  };
  await expect(grantFullAccess).toPass({ timeout: 30_000 });
  await expect(page.locator('#settings-saved')).toHaveText('Permission mode updated.', LIVE);
  await expect(pill).toHaveCSS('color', 'rgb(205, 205, 0)', LIVE);
  await expect(badge).toHaveCSS('color', 'rgb(205, 205, 0)', LIVE);

  // Restore the host's default mode; colors follow the state, not the dialog.
  const restoreWrite = async () => {
    await page.click('#permission-options label:has(input[value="workspace-write"])', { timeout: 3000 });
    await page.click('#permission-save', { timeout: 3000 });
  };
  await expect(restoreWrite).toPass({ timeout: 30_000 });
  await expect(page.locator('#settings-saved')).toHaveText('Permission mode updated.', LIVE);
  await expect(pill).toHaveCSS('color', 'rgb(155, 163, 150)', LIVE);
  await expect(badge).toHaveCSS('color', 'rgb(191, 197, 187)', LIVE);
});

// Manual history compaction is a first-class PWA surface, not a slash-command
// dead end. It must lock conflicting UI, persist the replacement family, and
// remain visible after a cold browser projection rebuild.
test('history compaction completes, cold-replays, and allows the next run', async ({ page }) => {
  const entry = hostInfo('success');
  await openWorkbench(page, entry);
  await openDetails(page);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.fill('#prompt', 'history seed 0');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);

  for (let turn = 1; turn < 5; turn += 1) {
    await page.fill('#prompt', `history seed ${turn}`);
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(turn + 1, LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  }

  const compact = page.locator('#compact-session');
  await expect(compact).toBeEnabled(LIVE);
  await compact.click();
  await expect(
    page.locator('.notice-line.trace-event', { hasText: 'History compacted' }).last(),
  ).toBeVisible(LIVE);
  await expect(compact).toHaveText('Compact history', LIVE);
  await expect(compact).toBeEnabled(LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(
    page.locator('.notice-line.trace-event', { hasText: 'History compacted' }).last(),
  ).toBeVisible(LIVE);
  await expect(compact).toBeEnabled(LIVE);

  await page.fill('#prompt', 'continue after compact');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
});

test('active history compaction survives refresh and remains cancellable', async ({ page }) => {
  const entry = hostInfo('compact-slow');
  await openWorkbench(page, entry);
  await openDetails(page);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  for (let turn = 0; turn < 5; turn += 1) {
    await page.fill('#prompt', `cancellable history ${turn}`);
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(turn + 1, LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
  }

  const compact = page.locator('#compact-session');
  await compact.click();
  await expect(compact).toHaveText('Cancel compaction', LIVE);
  await expect(page.locator('#prompt')).toBeDisabled(LIVE);
  await expect(page.locator('#new-session')).toBeDisabled(LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(compact).toHaveText('Cancel compaction', LIVE);
  await expect(page.locator('#detail-run')).toHaveText('Compacting', LIVE);
  await compact.click();

  await expect(compact).toHaveText('Compact history', LIVE);
  await expect(compact).toBeEnabled(LIVE);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  await expect(page.locator('.notice-line', { hasText: 'compaction' }).last()).toBeVisible(LIVE);
  await expect(page.locator('.notice-line.trace-event', { hasText: 'History compacted' })).toHaveCount(0);

  await page.fill('#prompt', 'continue after cancelled compaction');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
});

// MM-4：浏览器文件不以路径进入 RPC；先上传到 server-minted draft scope，
// prompt 只携带 opaque upload id。回放/实时消息再经受 Bearer 保护的
// attachment endpoint 取回 blob URL，页面不把 token 塞进图片 URL。
test('image draft stages, sends image-only, and rebuilds a protected history preview', async ({ page }, testInfo) => {
  const entry = hostInfo('run-command');
  const attachmentRequests = [];
  page.on('request', (request) => {
    if (request.url().includes('/api/attachments/')) attachmentRequests.push(request);
  });
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('ready', LIVE);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: testInfo.outputPath('mobile-image-draft.png'), animations: 'disabled' });

  await page.click('#send');
  const preview = page.locator('.msg.user .message-attachment-preview').last();
  await expect(preview).toBeVisible(LIVE);
  await expect(preview).toHaveAttribute('src', /^blob:/, LIVE);
  await page.screenshot({ path: testInfo.outputPath('mobile-image-message.png'), animations: 'disabled' });
  await preview.click();
  await expect(page.locator('#image-lightbox')).toBeVisible(LIVE);
  await expect(page.locator('#image-lightbox .image-lightbox-image')).toHaveAttribute('src', /^blob:/, LIVE);
  await page.locator('#image-lightbox .image-lightbox-close').click();
  await expect(page.locator('#image-lightbox')).toBeHidden(LIVE);
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  expect(attachmentRequests).not.toHaveLength(0);
  expect(attachmentRequests[0].headers().authorization).toBe(`Bearer ${entry.token}`);
  expect(attachmentRequests[0].url()).not.toContain(entry.token);

  // 服务端 TestProvider 会请求 run_command；拒绝即可收束本用例，不让
  // 图片 UI 验收依赖副作用获批。
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  const replayPreview = page.locator('.msg.user .message-attachment-preview').last();
  await expect(replayPreview).toHaveAttribute('src', /^blob:/, LIVE);
});

test('active run accepts an image steering draft without exposing a host path', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'hold at an approval boundary');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'use this screenshot for the next step');
  await page.click('#send');
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('steering queued · held for recovery', LIVE);
  await expect(page.locator('.notice-line', { hasText: 'steering queued' })).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('durable image steering claim can beat its RPC acknowledgement without restoring the draft', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'hold at an approval boundary for an early claim');
  await page.click('#send');
  const approval = page.locator('.approval-card').first();
  await expect(approval).toBeVisible(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'claim this image before the HTTP acknowledgement');

  let markSteerProcessed;
  const steerProcessed = new Promise((resolve) => { markSteerProcessed = resolve; });
  let releaseSteerResponse;
  const steerResponseReleased = new Promise((resolve) => { releaseSteerResponse = resolve; });
  await page.route('**/api/steer.send', async (route) => {
    const response = await route.fetch();
    markSteerProcessed();
    await steerResponseReleased;
    await route.fulfill({ response });
  });

  await page.click('#send');
  await steerProcessed;
  try {
    // Release the approval while the successful steer.send response remains
    // withheld. The next model turn claims and durably emits the steering SSE
    // first; pre-fix, settleQueuedDraft ignored that event because the local
    // state had not yet advanced from sending → queued.
    await approval.locator('button.ghost').click();
    await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
    await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  } finally {
    releaseSteerResponse();
  }
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
  await expect(page.locator('.attachment-chip')).toHaveCount(0);
});

test('cancelled unclaimed image steering restores the draft for a normal retry', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await page.fill('#prompt', 'hold at an approval boundary');
  await page.click('#send');
  await expect(page.locator('.approval-card').first()).toBeVisible(LIVE);

  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.fill('#prompt', 'keep this image for the next run');
  await page.click('#send');
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('steering queued · held for recovery', LIVE);

  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('restored', LIVE);

  // 输入框已在 steering 入队时清空；这里以图片-only prompt 验证恢复的
  // opaque upload 可直接复用，而不是要求浏览器重新读本地文件。
  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('.msg.user .message-attachment-preview').last()).toHaveAttribute('src', /^blob:/, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('multiple image draft preserves ordering through image-only admission', async ({ page }) => {
  const entry = hostInfo('run-command');
  const image = path.join(__dirname, '..', '..', 'icons', 'icon-192.png');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await page.setInputFiles('#attachment-input', [image, image]);
  await expect(page.locator('.attachment-chip')).toHaveCount(2, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(['staged locally', 'staged locally'], LIVE);
  await page.locator('.attachment-chip').nth(1).locator('button[title="Move image earlier"]').click();
  await expect(page.locator('.attachment-index')).toHaveText(['01', '02'], LIVE);

  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  const user = page.locator('.msg.user').last();
  await expect(user.locator('.message-attachment-preview')).toHaveCount(2, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('drop and clipboard image paste enter the same staged draft pipeline', async ({ page }) => {
  const entry = hostInfo('run-command');
  const image = path.join(__dirname, '..', '..', 'icons', 'icon-192.png');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);

  await dispatchImageFileGesture(page, '#composer-shell', 'drop', image);
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.locator('.attachment-remove').click();
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);

  await dispatchImageFileGesture(page, '#prompt', 'paste', image);
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.click('#send');
  await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(1, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('failed image staging keeps the draft and retry reuses the original file', async ({ page }) => {
  const entry = hostInfo('run-command');
  let rejectOnce = true;
  await page.route('**/api/drafts/**/images', async (route) => {
    if (rejectOnce) {
      rejectOnce = false;
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ ok: false, error: { message: 'temporary staging outage' } }),
      });
    } else {
      await route.continue();
    }
  });
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toContainText('not staged', LIVE);
  await expect(page.locator('#attachment-summary')).toContainText('need attention', LIVE);

  await page.locator('button.attachment-retry').click();
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
  await page.click('#send');
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  const approval = page.locator('.approval-card').last();
  await expect(approval).toBeVisible(LIVE);
  await approval.locator('button.ghost').click();
  await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });
});

test('switching sessions revokes the local image draft instead of carrying it across', async ({ page }) => {
  const entry = hostInfo('run-command');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-192.png'));
  await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);

  const previous = page.locator('#session-list li:not(.active) .session-item').first();
  await expect(previous).toBeVisible(LIVE);
  await previous.click();
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await expect(page.locator('.attachment-chip')).toHaveCount(0, LIVE);
  await expect(page.locator('#attachment-rail')).toBeHidden(LIVE);
});

// 对抗式性能腿：长流正在持续写入转录区时，浏览器仍须能完成本地预览、
// draft scope RPC 与 raw upload；上传不能被 O(n²) 文本渲染饿死。
test('image staging stays responsive while a long stream is updating the transcript', async ({ page }) => {
  const entry = hostInfo('long-stream');
  await openWorkbench(page, entry);
  await page.click('#new-session');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'long stream with an image draft');
  await page.click('#send');
  await page.waitForFunction(
    () => {
      const body = document.querySelector('.msg.assistant .body');
      return body && body.textContent.length > 4_000;
    },
    null,
    LIVE,
  );

  const before = await page.locator('.msg.assistant .body').textContent();
  await page.setInputFiles('#attachment-input', path.join(__dirname, '..', '..', 'icons', 'icon-512.png'));
  await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', { timeout: 5_000 });
  await page.waitForFunction(
    (previousLength) => {
      const body = document.querySelector('.msg.assistant .body');
      return body && body.textContent.length > previousLength;
    },
    (before || '').length,
    { timeout: 5_000 },
  );

  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
});

// MM-5 手工性能腿：真实 Chromium → PWA → streaming upload → core
// admission，再经 F5/SSE 重连恢复四张受保护历史图。默认跳过，避免把
// 近 32 MiB fixture 与高频 RSS 采样加入普通前端回归。
test('MM-5 PWA near-limit upload and reconnect RSS profile', async ({ page }) => {
  test.skip(process.env.CLAT_MM5_PERF !== '1', 'set CLAT_MM5_PERF=1 for the manual RSS profile');
  test.setTimeout(300_000);
  const entry = hostInfo('run-command');
  const fixtures = makeNearLimitPngFixtures();
  try {
    await openWorkbench(page, entry);
    await page.click('#new-session');
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    const upload = await measureRss(entry.pid, async () => {
      await page.setInputFiles('#attachment-input', fixtures.paths);
      await expect(page.locator('.attachment-chip')).toHaveCount(4, { timeout: 120_000 });
      await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(
        ['staged locally', 'staged locally', 'staged locally', 'staged locally'],
        { timeout: 120_000 },
      );
    });

    await page.click('#send');
    await expect(page.locator('.attachment-chip')).toHaveCount(0, { timeout: 120_000 });
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(4, LIVE);
    const approval = page.locator('.approval-card').last();
    await expect(approval).toBeVisible(LIVE);
    await approval.locator('button.ghost').click();
    await expect(page.locator('.verdict.completed')).toBeVisible({ timeout: 60_000 });

    const reconnect = await measureRss(entry.pid, async () => {
      await page.reload();
      await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
      await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(4, {
        timeout: 60_000,
      });
      for (const preview of await page.locator('.msg.user').last().locator('.message-attachment-preview').all()) {
        await expect(preview).toHaveAttribute('src', /^blob:/, LIVE);
      }
    });

    console.log(`MM5_PWA_PERF ${JSON.stringify({
      profile: process.env.CLAT_E2E_RELEASE === '1' ? 'release' : 'test',
      images: fixtures.paths.length,
      rawBytes: fixtures.rawBytes,
      upload,
      reconnect,
    })}`);
  } finally {
    fs.rmSync(fixtures.root, { recursive: true, force: true });
  }
});

// MM-5 paid product campaign: real Chromium/PWA, real CLAT Application and
// real GLM provider. It remains default-off and requires a process-local key.
test('MM-5 live GLM PWA multi-image, image-only history, and replay', async ({ page }) => {
  test.skip(process.env.CLAT_LIVE_GLM_E2E !== '1', 'set CLAT_LIVE_GLM_E2E=1 and provide the live key');
  test.setTimeout(300_000);
  const entry = hostInfo('live-glm');
  const fixtures = makeLiveColorFixtures();
  try {
    await openWorkbench(page, entry);
    await page.click('#new-session');
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    await page.setInputFiles('#attachment-input', [fixtures.green, fixtures.yellow]);
    await expect(page.locator('.attachment-chip .attachment-state')).toHaveText(
      ['staged locally', 'staged locally'],
      LIVE,
    );
    await page.fill('#prompt', "Two solid-color images are attached in order. Reply exactly '1=green;2=yellow' and nothing else.");
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(1, { timeout: 180_000 });
    const orderedReply = (await page.locator('.msg.assistant .body').last().textContent() || '').toLowerCase();
    expect(orderedReply).toMatch(/1\s*=\s*green/);
    expect(orderedReply).toMatch(/2\s*=\s*yellow/);
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(2, LIVE);

    await page.reload();
    await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
    await expect(page.locator('#detail-run')).toHaveText('Idle', LIVE);
    await expect(page.locator('.msg.user').last().locator('.message-attachment-preview')).toHaveCount(2, LIVE);
    const replayedReply = (await page.locator('.msg.assistant .body').last().textContent() || '').toLowerCase();
    expect(replayedReply).toContain('green');
    expect(replayedReply).toContain('yellow');

    await page.setInputFiles('#attachment-input', fixtures.green);
    await expect(page.locator('.attachment-chip .attachment-state')).toHaveText('staged locally', LIVE);
    await page.click('#send');
    await expect(page.locator('.verdict.completed, .verdict.failed')).toHaveCount(1, {
      timeout: 180_000,
    });
    await expect(page.locator('.verdict.completed')).toHaveCount(1);
    await expect(page.locator('#send')).toBeEnabled(LIVE);

    await page.fill(
      '#prompt',
      'What solid color filled the image in my immediately previous image-only message? Reply exactly HISTORY_OK_GREEN and nothing else.',
    );
    await page.click('#send');
    await expect(page.locator('.verdict.completed')).toHaveCount(2, { timeout: 180_000 });
    await expect(page.locator('.msg.assistant .body').last()).toContainText('HISTORY_OK_GREEN', {
      timeout: 180_000,
    });
  } finally {
    fs.rmSync(fixtures.root, { recursive: true, force: true });
  }
});
