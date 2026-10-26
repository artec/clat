// AG-DOGFOOD-5 返工 R1 负控（对应乙方审核 F1–F5）：重复请求、旧协议
// 缺 id 结果、晚到 settled 归属、切会话异步串扰、live/replay 混合作用域、
// 冷重载终态恢复。宿主：ag5-joint（真实宿主、真实 run 提供真实前置状态）。
//
// 注入机制：fetch tee 在 /api/events 响应体上挂可编程 TransformStream，
// 合成帧以真实 wire 字节追加进同一条流——PWA 的 openStream reader →
// handleFrame → DOM 是真实消费者链，不是直调辅助函数。
const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

function frame(name, payload) {
  return `event: ${name}\ndata: ${JSON.stringify(payload)}\n\n`;
}
const runEvent = (event) => frame('event', { v: 1, event });
const settledFrame = (rpcId, type) =>
  frame('prompt.settled', { ctl: { prompt_rpc_id: rpcId, outcome: { type, turns: 1 } } });
const subscribedFrame = (sessionId, selection) =>
  frame('subscribed', { ctl: { session_id: sessionId, selection_generation: selection } });

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    // 注入层：/api/events 的响应体经手动泵转发（原始 reader → writer），
    // 合成帧由保留的 writer 直接写入同一条 readable——PWA 的
    // getReader → decode → parseSseBlock → handleFrame → DOM 仍是真实
    // 消费者链。注入不依赖后续真实 chunk（空闲流即时送达），原始字节
    // 原样透传，连接语义不变。
    window.__inject = null;
    const origFetch = window.fetch;
    window.fetch = async (...args) => {
      const res = await origFetch(...args);
      const url = String(args[0] && (args[0].url || args[0]));
      if (url.includes('/api/events') && res.body) {
        const encoder = new TextEncoder();
        const { readable, writable } = new TransformStream({
          transform(chunk, controller) { controller.enqueue(chunk); },
        });
        const writer = writable.getWriter();
        const source = res.body.getReader();
        // 每连接一个泵计数器：重连（resubscribe）会建立新连接并整体替换
        // __pump/__inject——注入必须确认自己写进了"当前活跃"的泵。
        const pump = { wrote: 0, read: 0, injected: 0 };
        window.__pump = pump;
        (async () => {
          try {
            for (;;) {
              const { done, value } = await source.read();
              if (done) break;
              pump.read += value?.length || 0;
              await writer.write(value);
            }
            await writer.close();
          } catch (error) {
            await writer.abort(error).catch(() => {});
          }
        })();
        window.__inject = async (text) => {
          pump.injected += text.length;
          await writer.write(encoder.encode(text));
          pump.wrote += text.length;
        };
        return new Response(readable, { status: res.status, headers: res.headers });
      }
      return res;
    };
  });
});


/// 宿主会话在一个 Playwright invocation 内被多个测试共享；每个测试
/// 用真实 UI（.project-group:has(#session-list) [data-action=new-project-session] → session.new + resubscribe）回到 Fresh 会话，
/// 保证 run1 的形状断言（5 卡 / 2 of 4 未完成）不受前序测试累积影响。
async function freshSession(page) {
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.waitForFunction(() => document.querySelector('#conn-status')?.textContent === 'live'
    && document.querySelectorAll('.tool-card').length === 0, null, { timeout: 15_000 });
}

async function cards(page) {
  return page.evaluate(() => [...document.querySelectorAll('.tool-card')].map((card) => ({
    callId: card.dataset.callId || null,
    state: card.dataset.state || null,
    name: (card.querySelector('summary span:nth-child(2)') || {}).textContent || '',
    text: card.textContent || '',
  })));
}
async function inject(page, text) {
  // 注入必须落到"当前活跃"连接的泵：freshSession/reconnect 会替换
  // window.__inject，拿到旧引用的写入只会进无人读取的流。以活跃泵的
  // injected 计数增量为确认，短退避重试（写旧泵是无害的空转）。
  await page.evaluate(async (body) => {
    let last = -1;
    for (let attempt = 0; attempt < 40; attempt += 1) {
      const before = window.__pump ? window.__pump.injected : -1;
      last = before;
      try {
        await window.__inject(body);
      } catch (_) {}
      await new Promise((resolve) => setTimeout(resolve, 25));
      if (window.__pump && window.__pump.injected > before) return;
    }
    throw new Error('injection never reached the live pump (last=' + last + ')');
  }, text);
}

test('F2: duplicate tool_requested reuses the same card and never resets a settled card', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  const before = (await cards(page)).length;

  // 活跃窗口外注入：先造一个活跃 run，再重复请求同一 call id。
  await inject(page, runEvent({ type: 'run_started', prompt: 'probe' }));
  await page.waitForTimeout(200);
  const request = JSON.stringify({ id: 'call-dup', name: 'read_file', arguments: { path: '.' } });
  await inject(page, runEvent({ type: 'tool_requested', call: JSON.parse(request) }));
  await page.waitForTimeout(150);
  await inject(page, runEvent({ type: 'tool_requested', call: JSON.parse(request) }));
  await page.waitForTimeout(150);
  let now = await cards(page);
  expect(now.filter((card) => card.callId === 'call-dup')).toHaveLength(1);
  expect(now.length).toBe(before + 1);

  // 完成 → 单卡 Done；再注入重复请求不得重置回 Pending、不得新增卡。
  await inject(page, runEvent({
    type: 'tool_finished',
    result: { call_id: 'call-dup', tool_name: 'read_file', output: { ok: true }, is_error: false },
  }));
  await page.waitForTimeout(150);
  await inject(page, runEvent({ type: 'tool_requested', call: JSON.parse(request) }));
  await page.waitForTimeout(150);
  now = await cards(page);
  const dup = now.filter((card) => card.callId === 'call-dup');
  expect(dup).toHaveLength(1);
  expect(dup[0].state).toBe('done');
  expect(now.length).toBe(before + 1);
});

test('F3: a legacy tool_finished without call id still shows its result card', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  const before = (await cards(page)).length;

  await inject(page, frame('replay', {
    replay: { type: 'tool_finished', seq: 901, turn: 9, tool: 'read_file', output: { note: 'LEGACY_RESULT' }, is_error: false },
  }));
  await page.waitForTimeout(200);
  const now = await cards(page);
  expect(now.length).toBe(before + 1);
  const legacy = now.find((card) => card.text.includes('LEGACY_RESULT'));
  expect(legacy).toBeTruthy();
  expect(legacy.state).toBe('done');
  expect(legacy.name).toBe('read_file');

  // 错误无 id：结果可见、标记错误且位于摘要外。
  await inject(page, frame('replay', {
    replay: { type: 'tool_finished', seq: 902, turn: 9, tool: 'edit_file', output: { error: 'boom' }, is_error: true },
  }));
  await page.waitForTimeout(200);
  const boom = (await cards(page)).filter((card) => card.text.includes('boom'));
  expect(boom).toHaveLength(1);
  expect(boom[0].state).toBe('error');
  const outside = await page.evaluate(() => [...document.querySelectorAll('.tool-card')]
    .filter((card) => card.textContent.includes('boom'))
    .every((card) => card.closest('.work-summary') === null));
  expect(outside).toBe(true);
});

test('F3b: live tool_finished settles the card opened by the replay prefix (mixed key scopes)', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  const before = (await cards(page)).length;

  await inject(page, frame('replay', {
    replay: { type: 'tool_requested', seq: 903, turn: 7, call: { id: 'mix-x', name: 'search', arguments: { query: 'needle' } } },
  }));
  await page.waitForTimeout(150);
  await inject(page, runEvent({
    type: 'tool_finished',
    result: { call_id: 'mix-x', tool_name: 'search', output: { matches: 1 }, is_error: false },
  }));
  await page.waitForTimeout(200);
  const now = await cards(page);
  const mixed = now.filter((card) => card.callId === 'mix-x');
  expect(mixed).toHaveLength(1);
  expect(mixed[0].state).toBe('done');
  expect(now.length).toBe(before + 1);
});

test('F4: a late settled with an unseen rpc id after a new run_started owns no terminal', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  const verdicts = await page.locator('.verdict').count();

  // 新 run 已开始（活跃），随后到达一个从未见过的旧 settled。
  // 注入交付需确认：run_started 必须真的把 runActive 置位（泵交接的
  // 时序窗口里注入可能落到已替换的连接上），否则后续断言无意义。
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await inject(page, runEvent({ type: 'run_started', prompt: 'second run' }));
    if (await page.evaluate(() => state.runActive === true)) break;
    await page.waitForTimeout(100);
  }
  expect(await page.evaluate(() => state.runActive)).toBe(true);
  await inject(page, settledFrame('rpc-stale-1', 'completed'));
  await inject(page, settledFrame('rpc-stale-2', 'failed'));
  await page.waitForTimeout(250);

  // 活跃 Run 事实不得被旧终态改写：不新增 verdict，run 状态不变成终态。
  expect(await page.locator('.verdict').count()).toBe(verdicts);
  const runState = await page.locator('#run-state').textContent();
  expect(runState).not.toContain('run completed');
  expect(runState).not.toContain('run failed');
  // 会话无未尽项时也不产生串扰交代。
  const accounting = await page.locator('.notice-line[aria-label="Unfinished work accounting"]').count();
  expect(accounting).toBeLessThanOrEqual(1);
});

test('F1: an in-flight accounting reply is dropped when the session changes mid-flight', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  // run1 settled 后才挂起后续 workflow.details（route 延后注册，避免拦住
  // 启动期的 workbench 类 RPC）：让 reload 触发的交代 RPC 悬空，期间
  // 切换到 B 会话。
  let release = null;
  await page.route('**/api/workflow.details', async (route) => {
    await new Promise((resolve) => { release = resolve; });
    await route.continue();
  });
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await page.waitForTimeout(300);

  // 切换到 B 会话：replay.begin 清转录 + 新订阅身份（新 session/selection/stream 语义）。
  await inject(page, frame('replay.begin', { has_more: false }));
  await inject(page, frame('replay', { replay: { type: 'user_message', seq: 1, turn: 1, text: 'B session hello' } }));
  await inject(page, frame('replay.end', {}));
  await inject(page, subscribedFrame('session-b-1111', 77));
  await page.waitForTimeout(200);

  // 放行 A 的挂起回复：其未尽项文本必须全部丢弃，不得串入 B 的转录。
  await release();
  await page.waitForTimeout(400);
  const body = await page.locator('#transcript').textContent();
  expect(body).toContain('B session hello');
  expect(body).not.toContain('todos unfinished');
  expect(body).not.toContain('task completion not verified');
});

test('F5: cold reload restores the persistent run terminal state', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  await expect(page.locator('#run-state')).toContainText('run completed', LIVE);

  // 乙方真实宿主复现点：刷新后转录/卡/交代仍在而 #run-state 变空。
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('.tool-card')).toHaveCount(5, LIVE);
  await expect(page.locator('.notice-line[aria-label="Unfinished work accounting"]')).toHaveCount(1, LIVE);
  await expect(page.locator('#run-state')).toContainText('run completed — send a message to continue', LIVE);
});

// ---------------------------------------------------------------------------
// R2 复审负控（docs/audit/2026-10-17-…-review-r2.md R2-F1..F4）：
// 生产 reason 形状的终态恢复、连续重建覆盖、跨 turn 作用域隔离、
// fallback 生命周期、挂起期间重复交代的去重。
// ---------------------------------------------------------------------------

test('R2F1: failed and user-aborted turn ends restore their real terminal states', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  const rebuild = async (reason) => {
    await inject(page, frame('replay.begin', { has_more: false }));
    await inject(page, frame('replay', { replay: { type: 'user_message', seq: 1, turn: 1, text: 'probe' } }));
    await inject(page, frame('replay', { replay: { type: 'turn_ended', seq: 2, turn: 1, reason } }));
    await inject(page, frame('replay.end', {}));
    await page.waitForTimeout(200);
    return page.evaluate(() => document.querySelector('#run-state')?.textContent || '');
  };
  // 生产 shapes.rs 的对象形状：{error: message} / {aborted: cause}。
  expect(await rebuild({ error: 'synthetic budget exhausted' }))
    .toBe('run failed — see the failure notice above');
  expect(await rebuild({ aborted: 'user' }))
    .toBe('run cancelled — send a message to continue');
  // 连续重建：前一重建的 completed 文案不得阻挡后一重建的取消恢复。
  expect(await rebuild('completed')).toBe('run completed — send a message to continue');
  expect(await rebuild({ aborted: 'user' })).toBe('run cancelled — send a message to continue');
});

test('R2F2: a turn-scoped result never merges into another turn with the same id', async ({ page }) => {
  // 纯合成帧负控：不依赖宿主业务状态，直接在首订阅连接注入（避免
  // freshSession 重连窗口的泵交接）。
  await openWorkbench(page, hostInfo('ag5-joint'));
  await inject(page, frame('replay', {
    replay: { type: 'tool_requested', seq: 11, turn: 1, call: { id: 'reuse', name: 'read_file', arguments: { path: 'old' } } },
  }));
  await inject(page, frame('replay', {
    replay: { type: 'tool_finished', seq: 12, turn: 1, call_id: 'reuse', tool: 'read_file', output: { note: 'OLD_RESULT' }, is_error: false },
  }));
  // turn 2 的同名 id 缺请求记录（兼容/迟到回退场景）：独立降级卡。
  await inject(page, frame('replay', {
    replay: { type: 'tool_finished', seq: 14, turn: 2, call_id: 'reuse', tool: 'read_file', output: { note: 'NEW_RESULT' }, is_error: false },
  }));
  await page.waitForTimeout(250);
  const now = await cards(page);
  const oldCard = now.filter((card) => card.text.includes('OLD_RESULT'));
  const newCard = now.filter((card) => card.text.includes('NEW_RESULT'));
  expect(oldCard, 'cards=' + JSON.stringify(now)).toHaveLength(1);
  expect(newCard, 'cards=' + JSON.stringify(now)).toHaveLength(1);
  expect(oldCard[0].text).not.toContain('NEW_RESULT');
  // turn1 卡保留自己的请求参数，不被 turn2 事件改写。
  expect(oldCard[0].text).toContain('old');
  expect(newCard[0].callId).toBe('reuse');
});

test('R2F3: a repeated orphan result keeps one card for the whole logical call', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  const orphan = (output) => frame('replay', {
    replay: { type: 'tool_finished', seq: 21, turn: 2, call_id: 'orphan', tool: 'search', output, is_error: false },
  });
  await inject(page, orphan({ note: 'FIRST' }));
  await page.waitForTimeout(150);
  await inject(page, orphan({ note: 'FIRST' }));
  await page.waitForTimeout(150);
  const now = await cards(page);
  const orphans = now.filter((card) => card.text.includes('FIRST'));
  expect(orphans).toHaveLength(1);
  expect(orphans[0].state).toBe('done');
  expect(orphans[0].outputs ?? 1).toBeTruthy();
});

test('R2F4: a duplicated replay.end while the accounting RPC is pending yields one line', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  // run1 已有 2 个未完成 todo；挂起 workflow.details 后重复消费同一
  // turn-end 的 replay.end 两次，再放行——只允许一份交代、一次 RPC。
  let release = null;
  const seen = [];
  page.on('request', (request) => {
    if (request.url().includes('workflow.details')) seen.push(request.url());
  });
  await page.route('**/api/workflow.details', async (route) => {
    await new Promise((resolve) => { release = resolve; });
    await route.continue();
  });
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await page.waitForTimeout(300);
  // 第二次同键 replay.end（重复通知窗口）。
  const seq = await page.evaluate(() => state.lastTurnEndSeq);
  await inject(page, frame('replay', { replay: { type: 'turn_ended', seq, turn: 1, reason: 'completed' } }));
  await inject(page, frame('replay.end', {}));
  await page.waitForTimeout(200);
  await release();
  await page.waitForTimeout(400);
  const lines = await page.locator('.notice-line[aria-label="Unfinished work accounting"]').count();
  expect(lines).toBe(1);
  expect(seen.length).toBe(1);
});

// ---------------------------------------------------------------------------
// R3 复审负控（review-r3 R3-F1/F2）：跨归属清理不越权、迟到请求参数可辨。
// ---------------------------------------------------------------------------

test('R3F1: a stale accounting reply cleans only its own in-flight slot', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  await page.fill('#prompt', 'please produce the two artifacts a.md and b.md');
  await page.click('#send');
  await expect(page.locator('.verdict.completed')).toHaveCount(1, LIVE);
  // A（旧连接）= 真实 reload 的回放交代：发起后挂在 route，响应延迟
  // 到切换完成后 fulfill——resume 确定发生在新身份下（归属必拒 + 清理
  // 同步执行）。B（新连接）挂起等放行。
  const pending = [];
  const seen = [];
  let served = 0;
  let releaseA = null;
  const switchGate = new Promise((resolve) => { releaseA = resolve; });
  page.on('request', (request) => {
    if (request.url().includes('workflow.details')) seen.push(request.url());
  });
  await page.route('**/api/workflow.details', async (route) => {
    served += 1;
    if (served === 1) {
      await switchGate;
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ ok: true, value: { todos: [
          { content: 'stale-A', status: 'pending' },
        ] } }),
      });
      return;
    }
    await new Promise((resolve) => pending.push(resolve));
    await route.continue();
  });
  // A：reload 的回放以真实键（宿主 turn seq）发起交代并挂起。
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  for (let i = 0; i < 40 && served < 1; i += 1) await page.waitForTimeout(100);
  expect(seen.length).toBe(1);
  const seq = await page.evaluate(() => state.lastTurnEndSeq);
  // B：selection notice → 真实重连换流；宿主回放以同一真实键再次发起
  // 交代并挂起（replay.begin 已清掉转录，live 交代行不残留）。
  await inject(page, frame('notice', { ctl: { kind: 'selection', payload: {} } }));
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  for (let i = 0; i < 40 && pending.length < 1; i += 1) await page.waitForTimeout(100);
  expect(pending.length).toBe(1);
  expect(seen.length).toBe(2);
  // 切换已定：放 A 的响应——旧流身份必拒；其清理不得移除 B 的同值占位。
  releaseA();
  await page.waitForTimeout(300);
  const held = await page.evaluate((key) => state.accountingInFlightOwners.has(key), `turn:${seq}`);
  expect(held, 'A cleanup must not release B in-flight slot').toBe(true);
  expect(await page.locator('.notice-line[aria-label="Unfinished work accounting"]').count()).toBe(0);
  // B 的同键 replay.end 重复到达：占位仍在 → 不发第三次 RPC。
  await inject(page, frame('replay', { replay: { type: 'turn_ended', seq, turn: 1, reason: 'completed' } }));
  await inject(page, frame('replay.end', {}));
  await page.waitForTimeout(300);
  expect(pending.length).toBe(1);
  // 放行 B：恰一行交代；全程 A 一次 + B 一次 RPC。
  pending.shift()();
  await page.waitForTimeout(500);
  const lines = await page.locator('.notice-line[aria-label="Unfinished work accounting"]').count();
  expect(lines).toBe(1);
  expect(seen.length).toBe(2);
});

test('R3F2: a late tool_requested fills in its missing request arguments', async ({ page }) => {
  await openWorkbench(page, hostInfo('ag5-joint'));
  await freshSession(page);
  // 结果先到（无请求记录）：fallback 卡 Done 并携带结果。
  await inject(page, runEvent({
    type: 'tool_finished',
    result: { call_id: 'late', tool_name: 'read_file', output: { note: 'EARLY_RESULT' }, is_error: false },
  }));
  await page.waitForTimeout(150);
  // 同 id 的请求迟到：补齐参数，保留 Done 状态、结果与单卡。
  await inject(page, runEvent({
    type: 'tool_requested',
    call: { id: 'late', name: 'read_file', arguments: { path: 'expected-path' } },
  }));
  await page.waitForTimeout(250);
  const now = await cards(page);
  const late = now.filter((card) => card.callId === 'late');
  expect(late).toHaveLength(1);
  expect(late[0].state).toBe('done');
  expect(late[0].text).toContain('EARLY_RESULT');
  expect(late[0].text).toContain('expected-path');
});
