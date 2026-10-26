// AG-DOGFOOD-8 / HIST-PWA-1（W1/W2/W3 浏览器腿）：PWA composer ↑/↓ 从
// 当前 Session 的 transcript 提交历史（与 TUI recent_inputs 同一 Host 权威）
// 召回：新→旧、越过最新项准确还原原草稿、多行光标/选区/IME/命令 picker
// 优先、跨 Session 隔离、提交失败不入史、浏览器存储不落历史明文。
//
// 宿主：`ag8-hist`（TestBehavior::Success，真实 Run/持久化路径）；
// steering 归属腿复用 `ag8-orderlive`（3 次模型请求之间确定性 claim）。
const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench, fetchBrowserRequest } = require('../helpers/workbench');

async function freshSession(page) {
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.waitForFunction(() => document.querySelector('#conn-status')?.textContent === 'live'
    && document.querySelectorAll('.tool-card').length === 0, null, { timeout: 15_000 });
}

/// 召回断言前的就绪门：prompt.send 的接纳回执与 settled 后的 workbench
/// 刷新是两条独立腿，↑ 必须等权威列表（至少 n 条）落地再按。
async function historyReady(page, n) {
  await page.waitForFunction((count) => inputHistory.list.length >= count
    && state.runActive === false, n, LIVE);
}

async function sendAndSettle(page, text, verdictCount) {
  await page.fill('#prompt', text);
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(verdictCount, LIVE);
}

test('HIST-PWA-1 W1/W2: ↑ recalls committed inputs newest-first and ↓ restores the untouched draft', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'alpha one', 1);
  await sendAndSettle(page, 'beta two', 2);
  await historyReady(page, 2);

  // 提交后 composer 已清空；↑→最新、再↑→更旧、↓→回新、越过最新→原草稿。
  await expect(page.locator('#prompt')).toHaveValue('');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('beta two');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('alpha one');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('beta two');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('');

  // 有内容的草稿在浏览后必须原样还原。
  await page.fill('#prompt', 'my draft');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('beta two');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('my draft');
});

test('HIST-PWA-1 W1: cold reload rebuilds the recall list from the host journal', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'reload alpha', 1);
  await sendAndSettle(page, 'reload beta', 2);

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await historyReady(page, 2);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('reload beta');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('reload alpha');
});

test('HIST-PWA-1 W1 isolation: another session cannot recall this session text', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'secret alpha one', 1);
  await freshSession(page);

  // 空 Session：↑ 不拦截（草稿保持原样）。
  await page.fill('#prompt', 'scratch draft');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('scratch draft');

  // 本 Session 提交后，只能召回本 Session 的输入。
  await sendAndSettle(page, 'gamma three', 1);
  await historyReady(page, 1);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('gamma three');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt'), 'no older cross-session entry may appear').toHaveValue('gamma three');
});

test('HIST-PWA-1 W2: multiline caret movement, selections and IME never hijack history', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'multiline anchor', 1);
  await historyReady(page, 1);

  // 多行：光标在第 2 行行首时 ↑ 是行内移动，不改写正文。
  await page.fill('#prompt', 'line one\nline two\nline three');
  await page.evaluate(() => {
    dom.prompt.setSelectionRange(9, 9); // line two 行首
  });
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('line one\nline two\nline three');
  expect(await page.evaluate(() => dom.prompt.selectionStart)).toBe(0);
  // 到达首行后再 ↑：进入历史召回。
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('multiline anchor');

  // 有选区时不触发历史（原生行为优先）。
  await page.fill('#prompt', 'selected text');
  await page.evaluate(() => dom.prompt.setSelectionRange(0, 5));
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('selected text');

  // IME 组合键（isComposing / keyCode 229）不触发历史。
  await page.evaluate(() => {
    dom.prompt.value = '中文组合中';
    dom.prompt.dispatchEvent(new CompositionEvent('compositionstart'));
    dom.prompt.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'ArrowUp', isComposing: true, keyCode: 229, bubbles: true, cancelable: true,
    }));
  });
  await expect(page.locator('#prompt')).toHaveValue('中文组合中');
  await page.evaluate(() => dom.prompt.dispatchEvent(new CompositionEvent('compositionend')));
});

test('HIST-PWA-1 W3: the open command picker owns the arrow keys', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'picker anchor', 1);
  await historyReady(page, 1);

  // '/re' 命中多个命令：picker 独占方向键且选择移动。
  await page.fill('#prompt', '/re');
  await expect(page.locator('#command-panel [role="option"][aria-selected="true"]')).toBeVisible(LIVE);
  const before = await page.evaluate(() => ({
    selected: document.querySelector('#command-panel [aria-selected="true"]')?.textContent,
    value: dom.prompt.value,
  }));
  await page.locator('#prompt').press('ArrowUp');
  const after = await page.evaluate(() => ({
    selected: document.querySelector('#command-panel [aria-selected="true"]')?.textContent,
    value: dom.prompt.value,
  }));
  expect(after.value).toBe(before.value);
  expect(after.selected).not.toBe(before.selected);
  await page.locator('#prompt').press('Escape');
});

test('HIST-PWA-1 W1 negative: a failed prompt.send never becomes recallable history', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'committed anchor', 1);
  await historyReady(page, 1);

  await page.route('**/api/prompt.send', (route) => route.abort());
  await page.fill('#prompt', 'never committed');
  await page.click('#send');
  await expect(page.locator('#run-state')).toContainText('send failed', LIVE);
  await page.unroute('**/api/prompt.send');

  await expect(page.locator('#prompt')).toHaveValue('never committed');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('committed anchor');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt'), 'the failed text must not pollute history').toHaveValue('committed anchor');
});

test('HIST-PWA-1 W3 privacy: recalled history never lands in browser storage', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'alpha one', 1);
  await sendAndSettle(page, 'beta two', 2);
  await historyReady(page, 2);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('beta two');

  const leaks = await page.evaluate(() => {
    const hits = [];
    for (const storage of [window.localStorage, window.sessionStorage]) {
      for (let index = 0; index < storage.length; index += 1) {
        const key = storage.key(index) || '';
        const value = storage.getItem(key) || '';
        if (value.includes('alpha one') || value.includes('beta two') || key.includes('alpha one')) {
          hits.push(key);
        }
      }
    }
    return hits;
  });
  expect(leaks).toEqual([]);
});

test('HIST-PWA-1 B8-R1: a delayed accepted response never writes the old session text into the new session history', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);

  // 会话 A：提交真实到达 Host（fetchBrowserRequest 转发完整浏览器头，含
  // Fetch Metadata——裸 route.fetch 会丢头被 CSRF 栅栏拒收），但 HTTP 成
  // 功响应被暂缓（SSE 正常、Run 照常完成）——只延迟回执，不伪造宿主状态。
  let releaseResponse;
  const gate = new Promise((resolve) => { releaseResponse = resolve; });
  await page.route('**/api/prompt.send', async (route) => {
    if (!(route.request().postData() || '').includes('b8 private session A prompt')) {
      return route.continue();
    }
    const response = await fetchBrowserRequest(route);
    await gate;
    await route.fulfill({ response });
  });
  await page.fill('#prompt', 'b8 private session A prompt');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);

  // 切新会话：等待围栏状态（不含 A 的文本）。并发套件里同一宿主被多页共
  // 享、其他测试的会话切换会广播到本页——断言只需锁定“旧会话文本不得
  // 进入当前列表/召回”。
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.waitForFunction(
    (probe) => !state.switching && !inputHistory.list.includes(probe),
    'b8 private session A prompt',
    LIVE,
  );

  // 断言窗口：在写入点观测（包装生产函数，不 mock 行为）——迟到的非法
  // 写入只存活到下一次在途权威刷新，状态级采样无法确定性命中；而“是否
  // 发起过写入”正是缺陷本身。同时保留终态断言作双保险。
  await page.evaluate(() => {
    window.__rememberCalls = [];
    const original = rememberSubmittedInput;
    rememberSubmittedInput = (text) => { window.__rememberCalls.push(String(text)); original(text); };
  });

  // 放行旧回执：接纳成功也不得把 A 的提示词写进新会话历史。
  await releaseResponse();
  await page.waitForTimeout(500);
  const writes = await page.evaluate(
    (probe) => window.__rememberCalls.filter((text) => text === probe),
    'b8 private session A prompt',
  );
  expect(writes, 'the late accepted receipt must never attempt the old-session write').toEqual([]);
  const leaked = await page.evaluate(
    (probe) => inputHistory.list.filter((text) => text === probe),
    'b8 private session A prompt',
  );
  expect(leaked, 'the list must stay free of the old session text').toEqual([]);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).not.toHaveValue('b8 private session A prompt');
});

test('HIST-PWA-1 B8-R3: a same-session committed append keeps the pre-browse draft recoverable', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'b8 committed anchor', 1);
  await historyReady(page, 1);

  const draft = 'untouched 中文 draft';
  await page.fill('#prompt', draft);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 committed anchor');

  // 另一客户端（同一 wire）追加提交；等 journal 入账 + 权威列表更新。
  await page.evaluate(() => rpc('prompt.send', { text: 'b8 other client commit' }));
  await expect(page.locator('.verdict.completed')).toHaveCount(2, LIVE);
  await historyReady(page, 2);

  // 游标按值锚迁移：仍显示 anchor，↓ 到达新条目，再 ↓ 越过最新项还原原草稿。
  await expect(page.locator('#prompt')).toHaveValue('b8 committed anchor');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('b8 other client commit');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt'), 'the pre-browse draft must stay recoverable').toHaveValue(draft);
});

test('HIST-PWA-1 B8-R4: soft-wrapped body keeps native caret movement; only visual boundaries recall', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'b8 wrapped anchor', 1);
  await historyReady(page, 1);

  // 长单段无 LF 正文，压缩 composer 宽度使其实际软折行显示多行。
  await page.locator('#prompt').evaluate((node) => { node.style.width = '220px'; });
  const paragraph = 'soft wrapped body '.repeat(64).trim();
  await page.fill('#prompt', paragraph);
  expect(
    await page.evaluate(() => dom.prompt.scrollHeight),
    'the paragraph must render as multiple visual lines',
  ).toBeGreaterThan(60);

  // 光标在正文中部：↑/↓ 是原生行内移动，不改写正文。
  await page.evaluate(() => dom.prompt.setSelectionRange(500, 500));
  const before = await page.evaluate(() => dom.prompt.selectionStart);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue(paragraph);
  expect(await page.evaluate(() => dom.prompt.selectionStart)).toBeLessThan(before);
  await page.locator('#prompt').press('ArrowDown');
  expect(await page.evaluate(() => dom.prompt.selectionStart)).toBe(before);

  // 真实视觉边界仍可召回：光标到正文首部（视觉首行）↑；越过最新项 ↓
  // 还原浏览前正文（即被暂存的原草稿）。
  await page.evaluate(() => dom.prompt.setSelectionRange(0, 0));
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 wrapped anchor');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue(paragraph);
});

test('HIST-PWA-1 B8-R2-1: a session switch retires the old session pending history draft', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'b8 committed anchor', 1);
  await historyReady(page, 1);

  // 会话 A：进入浏览（暂存 A 的未发送草稿），随后切新会话。
  await page.fill('#prompt', 'b8 private A unsent draft');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 committed anchor');
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.waitForFunction(
    (probe) => !state.switching && !inputHistory.list.includes(probe)
      && !inputHistory.list.includes('b8 committed anchor'),
    'b8 private A unsent draft',
    LIVE,
  );

  // 另一客户端（同一 wire）在新会话提交；等入账 + 权威列表更新。
  await page.evaluate(() => rpc('prompt.send', { text: 'b8 B external commit' }));
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await historyReady(page, 1);

  // B 会话内：↓ 不得恢复 A 的未发送草稿（输入保持为空）。
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt'), 'the old session draft must be retired').toHaveValue('');
  // ↑ 只召回 B 自己的提交；越过最新项还原空，而非 A 的草稿/锚点。
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 B external commit');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 B external commit');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('');
});

test('HIST-PWA-1 B8-R2-2: a dropped bounded-window anchor still permits newer history recall', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'b8 window anchor', 1);
  await historyReady(page, 1);

  const draft = '窗口滑动后的待还原草稿';
  await page.fill('#prompt', draft);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 window anchor');

  // 受控注入同 Session 的合法有界窗口（旧锚点不在窗口），沿真实
  // refreshWorkbench → observeInputHistory 消费链；不冒充真实 compaction。
  await page.route('**/api/workbench.info', async (route) => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.session.recent_inputs = ['b8 newer surviving input'];
    await route.fulfill({
      status: response.status(),
      headers: response.headers(),
      contentType: 'application/json',
      body: JSON.stringify(body),
    });
  });
  await page.evaluate(() => refreshWorkbench());
  await page.waitForFunction(
    () => inputHistory.list.length === 1 && inputHistory.list[0] === 'b8 newer surviving input',
    null,
    LIVE,
  );
  await page.unroute('**/api/workbench.info');

  // 锚点失效归一到无游标态：↑ 可重新进入历史并召回幸存记录。
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt'), 'recall must re-enter at the surviving entry').toHaveValue('b8 newer surviving input');
  // 越过最新项仍精确还原原草稿。
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue(draft);
});

test('HIST-PWA-1 B8-R3-1: an emptied same-session window still restores the pending draft via a real ArrowDown', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await sendAndSettle(page, 'b8 committed anchor', 1);
  await historyReady(page, 1);

  const draft = 'b8 empty-window untouched 中文 draft';
  await page.fill('#prompt', draft);
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('b8 committed anchor');

  // 受控提供同 Session 的合法空历史窗口（仅改 recent_inputs），沿生产
  // refreshWorkbench → observeInputHistory 消费；不冒充实际 compaction。
  const sessionAtBrowse = await page.evaluate(() => state.sessionId);
  await page.route('**/api/workbench.info', async (route) => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.session.recent_inputs = [];
    await route.fulfill({
      status: response.status(),
      headers: response.headers(),
      contentType: 'application/json',
      body: JSON.stringify(body),
    });
  });
  await page.evaluate(() => refreshWorkbench());
  await page.waitForFunction(() => inputHistory.list.length === 0, null, LIVE);
  await page.unroute('**/api/workbench.info');
  expect(await page.evaluate(() => state.sessionId)).toBe(sessionAtBrowse);

  // 真实 ArrowDown（单行 anchor 末尾光标）：原草稿逐字恢复。
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt'), 'an emptied same-session window must still restore the draft').toHaveValue(draft);
  // 恢复仅一次：再按 ↓ 不拦截（正文保持草稿）。
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue(draft);
});

test('HIST-PWA-1 B8-R3-1 negative: empty history with no stash never hijacks the arrow keys', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-hist'));
  await freshSession(page);
  await page.waitForFunction(() => inputHistory.list.length === 0, null, LIVE);

  // 空历史且无暂存：↑/↓ 均不被拦截（正文不变、无幻影还原）。
  await page.fill('#prompt', 'no stash draft');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('no stash draft');
  await page.locator('#prompt').press('ArrowDown');
  await expect(page.locator('#prompt')).toHaveValue('no stash draft');
});

test('HIST-PWA-1 T2 steering: queued steering joins history only after the durable claim', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag8-orderlive'));
  await freshSession(page);

  // 慢驻留脚本腿：首请求流完正文后 hold 3s——Run 真实活跃期间提交
  // steering（Enter 走 steer.send），在下一步边界被宿主 claim。
  await page.fill('#prompt', 'orderlive slowsteer: hold the run open then finish');
  await page.click('#send');
  await expect(page.locator('.msg.assistant').filter({ hasText: 'holding the run open' })).toBeVisible(LIVE);
  await page.fill('#prompt', 'hist steer probe');
  await page.locator('#prompt').press('Enter');

  // 被宿主 claim 后成为 durable user 消息（journal 权威），出现在转录里。
  await expect(page.locator('.msg.user').filter({ hasText: 'hist steer probe' })).toBeVisible(LIVE);
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await historyReady(page, 2);

  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('hist steer probe');
  await page.locator('#prompt').press('ArrowUp');
  await expect(page.locator('#prompt')).toHaveValue('orderlive slowsteer: hold the run open then finish');
});
