// AG-DOGFOOD-5 / ORDER-1 + CONT-1（J1 浏览器腿）：同一 Session 的多步
// 任务、同 call 单卡、正文真实序、真实终态与未尽项交代、用户明确续办、
// 冷恢复（整页重载 → journal 回放重建）。宿主：`ag5-joint`（脚本模型经
// 真实 Run/工具/持久化路径，见 src/test_support.rs Ag5JointScript）。
const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');


/// 宿主会话在一个 Playwright invocation 内被多个测试共享；每个测试
/// 用真实 UI（#new-session → session.new + resubscribe）回到 Fresh 会话，
/// 保证 run1 的形状断言（5 卡 / 2 of 4 未完成）不受前序测试累积影响。
async function freshSession(page) {
  await page.click('#new-session');
  await page.waitForFunction(() => document.querySelector('#conn-status')?.textContent === 'live'
    && document.querySelectorAll('.tool-card').length === 0, null, { timeout: 15_000 });
}

const TASK = 'please produce the two artifacts a.md and b.md';
const RUN1_ASSISTANT = 'phase one done';
const RUN2_ASSISTANT = 'all artifacts verified and reported';

// 断言用 DOM 概览：每张工具卡的 call-id、错误位、是否位于过程摘要外。
function toolCardFacts() {
  const cards = [...document.querySelectorAll('.tool-card')];
  return cards.map((card) => ({
    callId: card.dataset.callId || null,
    state: card.dataset.state || null,
    error: card.classList.contains('is-error'),
    stateText: (card.querySelector('.tool-state') || {}).textContent || '',
    outsideSummary: !card.closest('.work-summary'),
    outputs: card.querySelectorAll('.tool-output').length,
    summaryText: (card.querySelector('summary span:nth-child(2)') || {}).textContent || '',
  }));
}

test('ORDER-1/CONT-1 J1: one card per call, honest terminals, explicit continuation, cold reload', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  // 宿主带会话权限档（Project Write 默认，与生产日常档一致）：文件写免
  // 逐次审批；permission_checked 通知仍进转录（O1：请求/批准过程可见）。

  // —— Run 1：两个产物 + 延迟结果 + 可控制失败 + 带未尽工作的 Completed ——
  await page.fill('#prompt', TASK);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await expect(page.locator('.verdict')).toHaveCount(1, LIVE);

  // O1：每个 call_id 恰一张卡：todo_write、write a.md、write b.md、edit_file。
  const run1 = await page.evaluate(toolCardFacts);
  expect(run1.filter((card) => card.callId)).toHaveLength(5);
  const ids = run1.map((card) => card.callId);
  expect(new Set(ids).size).toBe(5);
  // 请求与结果都在同一张卡上（args 在 openToolCard 时写入，结果落定同卡）。
  for (const card of run1) {
    if (card.callId && card.callId.includes('call-ag5-')) expect(card.outputs).toBe(1);
  }
  // 可控制失败：edit_file 卡 is-error、位于过程摘要外（WEB-3 不变量），
  // 且 run 未被失败打断（仍得到 completed 终态）。
  const failed = run1.find((card) => card.summaryText === 'edit_file');
  expect(failed).toBeTruthy();
  expect(failed.error).toBe(true);
  expect(failed.state).toBe('error');
  expect(failed.outsideSummary).toBe(true);
  // 成功卡保留在过程摘要分组内（不为修三卡而回退既有布局）。
  const writeB = run1.find((card) => card.callId === 'call-ag5-write-b');
  expect(writeB.outsideSummary).toBe(false);
  expect(writeB.state).toBe('done');

  // O2（受控权威对照）：用户正文在前、assistant 收尾正文在后。
  const order = await page.evaluate(() => {
    const nodes = [...document.querySelectorAll('.msg.user, .msg.assistant')];
    return nodes.map((node) => ({ cls: node.className, text: node.textContent || '' }));
  });
  const userIndex = order.findIndex((node) => node.text.includes('two artifacts'));
  const assistantIndex = order.findIndex((node) => node.text.includes(RUN1_ASSISTANT));
  expect(userIndex).toBeGreaterThanOrEqual(0);
  expect(assistantIndex).toBeGreaterThan(userIndex);

  // AG-DOGFOOD-8 / ORDER-LIVE-1 永久断言（J4）：最终回复必须在每张工具卡
  // 之后——live DOM 不得把 final 文本回填到工具前的旧回复项。
  const timeline = await page.evaluate(() => [...document.querySelectorAll('.msg.user, .msg.assistant, .tool-card')]
    .map((node) => ({
      kind: node.classList.contains('msg') ? (node.classList.contains('user') ? 'user' : 'assistant') : 'tool',
      text: (node.textContent || '').trim(),
    })));
  const lastTool = timeline.map((entry) => entry.kind === 'tool').lastIndexOf(true);
  const finalAssistant = timeline.findIndex((entry) => entry.kind === 'assistant' && entry.text.includes(RUN1_ASSISTANT));
  expect(finalAssistant, 'live final answer must come after every tool card').toBeGreaterThan(lastTool);

  // C2：RunCompleted ≠ 任务已核验完成——未尽项交代可见且只出现一次。
  const accounting = page.locator('.notice-line[aria-label="Unfinished work accounting"]');
  try {
    await expect(accounting).toHaveCount(1, LIVE);
  } catch (error) {
    const dump = await page.evaluate(() => ({
      count: document.querySelectorAll('.notice-line').length,
      ariaCount: document.querySelectorAll('[aria-label]').length,
      session: state.sessionId, sel: state.selectionGeneration,
      accounted: [...state.accountedKeys], settled: [...state.settledRpcIds],
      runActive: state.runActive, replaying: state.history.replaying,
      runState: document.querySelector('#run-state')?.textContent,
      transcriptTail: (document.querySelector('#transcript')?.textContent || '').slice(-400),
    }));
    require('fs').writeFileSync('/tmp/ag5-acc-dump.json', JSON.stringify(dump, null, 1));
    throw error;
  }
  await expect(accounting).toHaveCount(1, LIVE);
  await expect(accounting).toContainText('2 of 4 todos unfinished', LIVE);
  await expect(accounting).toContainText('task completion not verified', LIVE);
  await expect(accounting).toContainText('verify outputs', LIVE);
  // C1：settled 后的常驻 run 状态可见（不是瞬时 4s 提示）。
  await expect(page.locator('#run-state')).toContainText('run completed', LIVE);

  // —— O3/J1 冷恢复：整页重载，转录只能从 journal 回放重建 ——
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('.tool-card')).toHaveCount(5, LIVE);
  const reloaded = await page.evaluate(toolCardFacts);
  expect(reloaded.filter((card) => card.callId)).toHaveLength(5);
  expect(new Set(reloaded.map((card) => card.callId)).size).toBe(5);
  const reloadedFailed = reloaded.find((card) => card.callId === 'call-ag5-edit-fail');
  expect(reloadedFailed.error).toBe(true);
  expect(reloadedFailed.outsideSummary).toBe(true);
  const reloadedOrder = await page.evaluate(() => {
    const nodes = [...document.querySelectorAll('.msg.user, .msg.assistant')];
    return nodes.map((node) => ({ cls: node.className, text: node.textContent || '' }));
  });
  expect(reloadedOrder.findIndex((node) => node.text.includes('two artifacts')))
    .toBeLessThan(reloadedOrder.findIndex((node) => node.text.includes(RUN1_ASSISTANT)));
  // AG-DOGFOOD-8 / ORDER-LIVE-1 永久断言（J4）：冷刷新 replay 后同样必须
  // 工具在前、最终回复在后（live/replay 等价的另一半）。
  const reloadedTimeline = await page.evaluate(() => [...document.querySelectorAll('.msg.user, .msg.assistant, .tool-card')]
    .map((node) => ({
      kind: node.classList.contains('msg') ? (node.classList.contains('user') ? 'user' : 'assistant') : 'tool',
      text: (node.textContent || '').trim(),
    })));
  const reloadedLastTool = reloadedTimeline.map((entry) => entry.kind === 'tool').lastIndexOf(true);
  const reloadedFinal = reloadedTimeline.findIndex((entry) => entry.kind === 'assistant' && entry.text.includes(RUN1_ASSISTANT));
  expect(reloadedFinal, 'replayed final answer must come after every tool card').toBeGreaterThan(reloadedLastTool);
  // 重载后未尽项交代可再现（turn seq 去重，恰一条）。
  {
    const dump = await page.evaluate(() => [...document.querySelectorAll('.notice-line[aria-label]')]
      .map((line) => line.getAttribute('aria-label') + ' :: ' + (line.textContent || '').slice(0, 90)));
    require('fs').writeFileSync('/tmp/ag5-acc-lines.json', JSON.stringify(dump, null, 1));
    await expect(page.locator('.notice-line[aria-label="Unfinished work accounting"]')).toHaveCount(1, LIVE);
  }

  // —— C3/J1 续办：用户明确选择继续；新 Run 只执行剩余动作 ——
  await page.fill('#prompt', 'continue');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await expect(page.locator('.verdict')).toHaveCount(1, LIVE);
  await expect(page.locator('.tool-card')).toHaveCount(8, LIVE);
  const run2 = await page.evaluate(toolCardFacts);
  // 仅新增 todo 收口卡（3 次：跨 run 工具结果从表面修剪，脚本按计数
  // 重发直至收口完成）；产物未被重写（write_file 卡数不变）。
  expect(run2.filter((card) => card.summaryText === 'write_file')).toHaveLength(2);
  expect(run2.filter((card) => (card.callId || '').startsWith('call-ag5-cont-todos')).length).toBe(3);
  const contCard = run2.find((card) => (card.callId || '').startsWith('call-ag5-cont-todos'));
  expect(contCard.state).toBe('done');
  // 终态数量：每个 run 恰一张 completed verdict（run1 经 reload 恢复，
  // run2 来自 live settled；重复 settled/replay 不叠加）。
  const run2Order = await page.evaluate(() => {
    const nodes = [...document.querySelectorAll('.msg.user, .msg.assistant')];
    return nodes.map((node) => ({ cls: node.className, text: node.textContent || '' }));
  });
  expect(run2Order.filter((node) => node.text.includes(RUN2_ASSISTANT))).toHaveLength(1);
  // 续办后无未尽项：不再新增交代行（仍为 run 1 的那一条）。
  await expect(page.locator('.notice-line[aria-label="Unfinished work accounting"]')).toHaveCount(1, LIVE);
  await expect(page.locator('#run-state')).toContainText('run completed', LIVE);
});
