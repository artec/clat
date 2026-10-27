// AG-DOGFOOD-8 / ORDER-LIVE-1（J1/J4 浏览器腿）：同一 Run 的 live DOM 因果
// 顺序 —— user → 有正文的第一步 assistant → 工具卡 → 无正文中间步（不渲染
// 空气泡）→ 第三张工具卡 → 工具之后的最终回复；冷刷新后 journal replay 与
// live 等价；同 Session 第二跑（重复 call-id）顺序不回归。
//
// 宿主：`ag8-orderlive`（Ag8OrderLiveScript：3 次模型请求 + 3 次工具调用，
// 经真实 Run/工具/持久化路径）。AG-DOGFOOD-7 在 v1.7.9 隔离 Chromium 实证
// 的 tool-card 排在最终 assistant 之后的缺陷，本 spec 用永久 DOM 断言钉死。
const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

const PROMPT = 'orderlive run: create the two files and report';
const OPENING = 'opening two work files';
const FINAL = 'final answer after tools';
const RERUN_PROMPT = 'orderlive again: rerun the choreography';
const RERUN_FINAL = 'final answer after rerun';

async function freshSession(page) {
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.waitForFunction(() => document.querySelector('#conn-status')?.textContent === 'live'
    && document.querySelectorAll('.tool-card').length === 0, null, { timeout: 15_000 });
}

/// 有意义节点的 DOM 概览：user/assistant 消息与工具卡的相对顺序和文本。
function timelineFacts() {
  const nodes = [...document.querySelectorAll('.msg.user, .msg.assistant, .tool-card')];
  return nodes.map((node) => ({
    kind: node.classList.contains('msg') ? (node.classList.contains('user') ? 'user' : 'assistant')
      : (node.classList.contains('tool-card') ? 'tool' : 'other'),
    text: (node.textContent || '').trim(),
    callId: node.dataset.callId || null,
    cardState: node.dataset.state || null,
    inSummary: Boolean(node.closest('.work-summary')),
  }));
}

function indexOfText(facts, needle, kind) {
  return facts.findIndex((entry) => entry.kind === kind && entry.text.includes(needle));
}

async function expectJournalMessageIdentity(page) {
  const messages=await page.evaluate(()=>[...dom.transcript.querySelectorAll('.msg')].map(node=>({
    text:node.querySelector('.body')?.textContent || '', time:Number(node.dataset.time),
    source:state.history.outline.find(item=>String(item.seq)===node.dataset.seq),
  })));
  for(const message of messages) {
    expect(message.source?.renderable).toBe(true);
    expect(message.source.preview.length).toBeGreaterThan(0);
    expect(message.text).toContain(message.source.preview.split('\n')[0]);
    expect(message.time).toBe(message.source.time_ms);
  }
}

test('ORDER-LIVE-1 live DOM: tools stay before the final answer, middle no-text step renders no bubble', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-orderlive'));
  await freshSession(page);

  await page.fill('#prompt', PROMPT);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);

  const facts = await page.evaluate(timelineFacts);
  // O1：提问 → 第一步有内容的 assistant → 3 张工具卡 → 最终回复。
  const userIndex = indexOfText(facts, PROMPT, 'user');
  const openingIndex = indexOfText(facts, OPENING, 'assistant');
  const firstCard = facts.findIndex((entry) => entry.kind === 'tool');
  const lastCard = facts.map((entry) => entry.kind === 'tool').lastIndexOf(true);
  const finalIndex = indexOfText(facts, FINAL, 'assistant');
  expect(userIndex).toBeGreaterThanOrEqual(0);
  expect(openingIndex).toBeGreaterThan(userIndex);
  expect(firstCard).toBeGreaterThan(openingIndex);
  expect(finalIndex, 'final answer must come after every tool card (live DOM)').toBeGreaterThan(lastCard);
  expect(lastCard).toBeGreaterThanOrEqual(0);

  // 恰两步有正文：无正文的中间模型步不渲染空回复气泡。
  const assistants = facts.filter((entry) => entry.kind === 'assistant');
  expect(assistants).toHaveLength(2);

  // 三张卡、各自落定、成功卡仍在过程摘要分组（AG5 布局不回归）。
  const cards = facts.filter((entry) => entry.kind === 'tool');
  expect(cards).toHaveLength(3);
  for (const card of cards) {
    expect(card.cardState).toBe('done');
    expect(card.inSummary).toBe(true);
  }
  await expectJournalMessageIdentity(page);
});

test('ORDER-LIVE-1 reload replay: journal replay keeps the same causal order', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-orderlive'));
  await freshSession(page);
  await page.fill('#prompt', PROMPT);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);

  // O2 冷恢复：整页重载，转录只能由 journal replay 重建。
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('.tool-card')).toHaveCount(3, LIVE);

  const facts = await page.evaluate(timelineFacts);
  const openingIndex = indexOfText(facts, OPENING, 'assistant');
  const lastCard = facts.map((entry) => entry.kind === 'tool').lastIndexOf(true);
  const finalIndex = indexOfText(facts, FINAL, 'assistant');
  expect(openingIndex).toBeGreaterThanOrEqual(0);
  expect(finalIndex, 'replayed final answer must stay after every tool card').toBeGreaterThan(lastCard);
  expect(facts.filter((entry) => entry.kind === 'assistant')).toHaveLength(2);
  // O2 等价：replay 的 assistant/工具相对顺序与 live 断言（上一测试）同形
  // ——opening 在首卡前、final 在末卡后，且无多余空气泡。
  const firstCard = facts.findIndex((entry) => entry.kind === 'tool');
  expect(openingIndex).toBeLessThan(firstCard);
});

test('ORDER-LIVE-1 second run in the same session keeps the order with repeated call ids', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-orderlive'));
  await freshSession(page);
  await page.fill('#prompt', PROMPT);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);

  // 同 Session 第二跑：同 call-id 复用（跨 run 配对不串卡），顺序仍成立。
  await page.fill('#prompt', RERUN_PROMPT);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(2, LIVE);

  const facts = await page.evaluate(timelineFacts);
  const rerunUser = indexOfText(facts, RERUN_PROMPT, 'user');
  const rerunCards = facts.map((entry, index) => ({ entry, index }))
    .filter(({ entry, index }) => entry.kind === 'tool' && index > rerunUser);
  const rerunFinal = facts.map((entry, index) => ({ entry, index }))
    .find(({ entry, index }) => entry.kind === 'assistant' && index > rerunUser && entry.text.includes(RERUN_FINAL));
  expect(rerunUser).toBeGreaterThanOrEqual(0);
  expect(rerunCards).toHaveLength(3);
  expect(rerunFinal.index, 'rerun final answer must come after the rerun tool cards')
    .toBeGreaterThan(rerunCards[rerunCards.length - 1].index);
  // 全转录共 6 张卡、4 个有正文 assistant（两跑各 3 卡/2 步正文）。
  expect(facts.filter((entry) => entry.kind === 'tool')).toHaveLength(6);
  expect(facts.filter((entry) => entry.kind === 'assistant')).toHaveLength(4);
});
