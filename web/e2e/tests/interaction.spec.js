const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const { LIVE, hostInfo, openWorkbench, openWorkbenchTool } = require('../helpers/workbench');
test.setTimeout(45_000);

async function open(page, host = 'success') {
  const info = hostInfo(host);
  await openWorkbench(page, info);
  return info;
}

for (const steering of [false, true]) {
test(`UX-1 late acknowledged ${steering ? 'steering' : 'prompt'} never reappears as an unsent session draft`, async ({ page }) => {
  await open(page);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  const result = await page.evaluate(async (steering) => {
    state.runActive = steering; state.switching = false; state.compactionActive = false;
    const select = async (id, selection) => {
      observeComposerSession(id, selection);
      await hydrateComposerDraft(state.workbench);
    };
    await select(null, 900);
    dom.prompt.value = 'acknowledged first submission'; state.composerGeneration += 1; saveComposerText();
    const original = rpc;
    let acknowledge;
    rpc = (method, params) => method === (steering ? 'steer.send' : 'prompt.send') || method === 'input.submit'
      ? new Promise((resolve) => { acknowledge = resolve; }) : original(method, params);
    const pending = submitPrompt();
    // 提交路径含 await（modelSetup→generation 守卫）：必须等分发真正挂到
    // mock 上、提交已在途，再让 B 开始编辑——否则 generation 变更会让守卫
    // 直接丢弃提交（rpc 永不发出），「晚确认」场景就不成立。
    for (let i = 0; i < 400 && typeof acknowledge !== 'function'; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
    await select('materialized-A', 900);
    await select('session-B', 901);
    dom.prompt.value = 'B is still editing'; state.composerGeneration += 1; saveComposerText();
    acknowledge(steering ? { outcome: 'queued' } : {}); await pending; rpc = original;
    const current = dom.prompt.value;
    await select('materialized-A', 902);
    return { current, restored: dom.prompt.value };
  }, steering);
  expect(result.current).toBe('B is still editing');
  expect(result.restored).toBe('');
});
}

test('queued input feedback clears on receipt or expiry without hiding newer status', async ({ page }) => {
  await open(page);
  await page.clock.install();
  const submit = () => page.evaluate(async () => {
    state.runActive = true; state.switching = false; state.compactionActive = false;
    state.workbench = { ...state.workbench, methods: ['input.submit'] };
    dom.prompt.value = 'queued feedback test'; state.composerGeneration += 1;
    const original = rpc;
    let item;
    rpc = (method, params) => {
      if (method === 'input.submit') {
        item = { id: params.clientMessageId, version: 1, text: params.text, draft: { uploads: [], references: [] } };
        return Promise.resolve({ outcome: 'queued' });
      }
      if (method === 'input.pending') return Promise.resolve({ items: [item], paused: false, interrupting: false });
      return original(method, params);
    };
    try { await submitPrompt(); }
    finally { rpc = original; }
    return item.id;
  });
  const status = page.locator('#run-state');
  const id = await submit();
  await expect(status).toHaveAttribute('data-presentation', 'input.accepted');
  await page.evaluate(id => {
    const original = rpc;
    rpc = (method, params) => method === 'input.pending'
      ? Promise.reject(new Error('pending refresh unavailable'))
      : original(method, params);
    try { handleLive({ type: 'steering_applied', client_message_id: id, text: 'queued feedback test' }); }
    finally { rpc = original; }
  }, id);
  await expect(status).toBeEmpty({ timeout: 1000 });

  await submit();
  await expect(status).toHaveAttribute('data-presentation', 'input.accepted');
  await page.evaluate(() => {
    const original = rpc;
    rpc = (method, params) => method === 'input.pending'
      ? Promise.resolve({ items: [], paused: false, interrupting: false })
      : original(method, params);
    try { handleLive({ type: 'steering_applied', client_message_id: 'other-input', text: 'another interjection' }); }
    finally { rpc = original; }
  });
  await expect(status).toBeEmpty({ timeout: 1000 });

  await submit();
  await expect(status).toHaveAttribute('data-presentation', 'input.accepted');
  await page.clock.fastForward(5000);
  await expect(status).toBeEmpty();
  await expect(page.locator('#pending-input-trigger')).toBeEnabled();

  await submit();
  await page.evaluate(() => updateRunState(CLATI18n.msg('input.failed')));
  await page.clock.fastForward(5000);
  await expect(status).toHaveAttribute('data-presentation', 'input.failed');
});

// 乐观回显（docs/todo/web-steering-optimistic-echo-2026-11-07.md）：插话被
// 接受即在转录出现 dim provisional 用户块，claim（steering_applied）原地
// 转正，未 claim 的 run 终态移除。三腿分别在 legacy steer.send 与现代
// input.submit 路径上锁定 INV-OE1/2/3。
test('UX-1b queued text steering echoes into the transcript immediately and settles in place when claimed', async ({ page }) => {
  await open(page);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  const result = await page.evaluate(async () => {
    state.runActive = true; state.switching = false; state.compactionActive = false;
    // 宿主已广告 input.submit，提交默认走现代路径；本腿钉住 legacy
    // steer.send 路径（现代路径由 UX-1d 覆盖）。
    state.workbench = { ...state.workbench, methods: [] };
    dom.prompt.value = 'mid-run interjection echo'; state.composerGeneration += 1; saveComposerText();
    const original = rpc;
    let sentId = null;
    rpc = (method, params) => {
      if (method !== 'steer.send') return original(method, params);
      sentId = params.clientMessageId;
      return Promise.resolve({ outcome: 'queued' });
    };
    await submitPrompt();
    rpc = original;
    const provisional = document.querySelector('.msg.user.provisional');
    const echoed = Boolean(provisional) && provisional.textContent.includes('mid-run interjection echo');
    handleLive({ type: 'steering_applied', client_message_id: sentId, text: 'mid-run interjection echo' });
    const matches = [...document.querySelectorAll('.msg.user')]
      .filter((node) => node.textContent.includes('mid-run interjection echo'));
    return {
      echoed,
      settledInPlace: matches.length === 1 && matches[0] === provisional && !matches[0].classList.contains('provisional'),
      promptCleared: dom.prompt.value === '',
    };
  });
  expect(result.echoed).toBe(true);
  expect(result.settledInPlace).toBe(true);
  expect(result.promptCleared).toBe(true);
});

test('UX-1c unclaimed steering echo leaves the transcript when the run ends and restores the draft', async ({ page }) => {
  await open(page);
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  const result = await page.evaluate(async () => {
    state.runActive = true; state.switching = false; state.compactionActive = false;
    // 钉住 legacy steer.send 路径：本腿断言的「未 claim 退草稿」是 legacy
    // 语义（现代路径条目留在 Host 队列，由 UX-1d 覆盖）。
    state.workbench = { ...state.workbench, methods: [] };
    dom.prompt.value = 'never claimed interjection'; state.composerGeneration += 1; saveComposerText();
    const original = rpc;
    rpc = (method) => (method === 'steer.send' ? Promise.resolve({ outcome: 'queued' }) : original(method));
    await submitPrompt();
    rpc = original;
    const echoed = [...document.querySelectorAll('.msg.user.provisional')]
      .some((node) => node.textContent.includes('never claimed interjection'));
    handleLive({ type: 'run_completed' });
    const ghost = [...document.querySelectorAll('.msg.user')]
      .some((node) => node.textContent.includes('never claimed interjection'));
    return { echoed, ghost, restored: dom.prompt.value.includes('never claimed interjection') };
  });
  expect(result.echoed).toBe(true);
  expect(result.ghost).toBe(false);
  expect(result.restored).toBe(true);
});

test('UX-1d deferred pending-input steering echoes immediately and is swept at run end', async ({ page }) => {
  await open(page);
  const result = await page.evaluate(async () => {
    state.runActive = true; state.switching = false; state.compactionActive = false;
    state.workbench = { ...(state.workbench || {}), methods: [...((state.workbench && state.workbench.methods) || []), 'input.submit'] };
    const original = rpc;
    const queue = [];
    rpc = (method, params) => {
      if (method === 'input.submit') {
        queue.push({ id: params.clientMessageId, version: 1, text: params.text, draft: { uploads: [], references: [] } });
        return Promise.resolve({});
      }
      if (method === 'input.pending') return Promise.resolve({ items: [...queue], paused: false, interrupting: false });
      return original(method, params);
    };
    dom.prompt.value = 'deferred input echo'; state.composerGeneration += 1; saveComposerText();
    await submitPrompt();
    rpc = original;
    const provisional = document.querySelector('.msg.user.provisional');
    const echoed = Boolean(provisional) && provisional.textContent.includes('deferred input echo');
    handleLive({ type: 'run_completed' });
    const ghost = [...document.querySelectorAll('.msg.user')]
      .some((node) => node.textContent.includes('deferred input echo'));
    // 现代 input.submit 路径：条目留在 Host 队列（不由前端退草稿）。
    return { echoed, stillQueued: queue.length === 1, ghost, promptCleared: dom.prompt.value === '' };
  });
  expect(result.echoed).toBe(true);
  expect(result.stillQueued).toBe(true);
  expect(result.ghost).toBe(false);
  expect(result.promptCleared).toBe(true);
});

// 事故回归（2026-11-08 插话冻结事故）：任何事件处理异常只损失该事件的
// UI 效果，绝不冒泡进 openStream 泵循环 reject 事件流——否则一次
// ReferenceError 就触发「重连→全量重放→再抛」风暴（会话运行中↔停止横
// 跳、界面与队列冻结、host serve 层被重放锤死需强杀）。
test('UX-1e a throwing event handler is isolated and never kills the event pipeline', async ({ page }) => {
  await open(page);
  const result = await page.evaluate(() => {
    state.runActive = true; state.switching = false; state.compactionActive = false;
    // 注入毒 handler：steering_applied case 内的 addNoticeLine 必抛。
    const original = addNoticeLine;
    let poisoned = false;
    addNoticeLine = () => { poisoned = true; throw new Error('poisoned handler'); };
    let propagated = false;
    try {
      handleFrame({ event: 'event', data: JSON.stringify({ event: { type: 'steering_applied', client_message_id: 'iso-probe', text: 'isolation probe' } }) });
    } catch (_) { propagated = true; }
    addNoticeLine = original;
    // 流未死：后续事件照常处理（run_completed 走完终态）。
    handleLive({ type: 'run_completed' });
    return { poisoned, propagated, pipelineAlive: state.runActive === false };
  });
  expect(result.poisoned).toBe(true);
  expect(result.propagated).toBe(false);
  expect(result.pipelineAlive).toBe(true);
});

test('UX-2 commands select without running and dedicated navigation opens its UI', async ({ page }) => {
  await open(page);
  await page.fill('#prompt', '/new');
  await expect(page.getByRole('option').filter({ hasText: '/new' })).toBeVisible(LIVE);
  const generation = await page.evaluate(() => state.selectionGeneration);
  await page.locator('#prompt').press('Enter');
  await expect(page.locator('#prompt')).toHaveValue('/new ');
  expect(await page.evaluate(() => state.selectionGeneration)).toBe(generation);
  await expect(page.locator('#command-panel')).toBeHidden();
  await page.fill('#prompt', '/model');
  await expect(page.getByRole('option').filter({ hasText: '/model' })).toBeVisible(LIVE);
  await page.locator('#prompt').press('Tab');
  await expect(page.locator('#model-picker')).toBeVisible(LIVE);
  await expect(page.locator('#prompt')).toHaveValue('');
});

test('UX-2 structured skills, unavailable reason, paste and composition do not execute', async ({ page }) => {
  await open(page);
  await expect.poll(() => page.evaluate(() => commandCatalog !== null), LIVE).toBe(true);
  await page.evaluate(() => {
    commandCatalogRequest += 1;
    commandCatalog = { commands: [{ name: 'rename', description: 'title', unavailable_reason: 'No active conversation' }],
      skills: { entries: [{ name: '中文审计', description: '检查边界', source: 'project', requires_execution: true }] } };
  });
  await page.fill('#prompt', '/rename');
  await expect(page.getByRole('option')).toHaveAttribute('aria-disabled', 'true');
  await expect(page.getByRole('option')).toContainText('No active conversation');
  await page.locator('#prompt').press('Enter');
  await expect(page.locator('#prompt')).toHaveValue('/rename');
  await page.fill('#prompt', '/中文');
  await expect(page.getByRole('option')).toContainText('requires-execution');
  await page.locator('#prompt').press('Tab');
  await expect(page.locator('#prompt')).toHaveValue('/skill 中文审计 ');
  await page.evaluate(() => {
    dom.prompt.value = '/rename';
    dom.prompt.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertFromPaste' }));
  });
  await expect(page.locator('#command-panel')).toBeHidden();
  await page.evaluate(() => {
    dom.prompt.dispatchEvent(new CompositionEvent('compositionstart'));
    dom.prompt.value = '/中文';
    dom.prompt.dispatchEvent(new InputEvent('input', { bubbles: true, isComposing: true }));
    dom.prompt.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true }));
  });
  await expect(page.locator('#prompt')).toHaveValue('/中文');
  await expect(page.locator('#command-panel')).toBeHidden();
  await expect(page.locator('#cancel')).toBeHidden();
});

test('UX-3 finds earlier message data and keeps the selected message after prefix insertion and new tokens', async ({ page }) => {
  await open(page, 'history');
  await expect(page.locator('#history-status')).toBeVisible(LIVE);
  await page.click('#find-open');
  await page.fill('#find-query', 'history question 00');
  await expect(page.locator('#find-status')).toContainText('0 matches');
  await expect(page.locator('#find-status')).toContainText('loaded window only');
  await page.click('#find-earlier');
  await expect(page.locator('#find-status')).toContainText('1/1', LIVE);
  await expect(page.locator('#find-status')).toContainText('all messages loaded', LIVE);
  await page.locator('#find-query').press('Enter');
  await expect(page.locator('.msg.find-current')).toContainText('history question 00');
  const id = await page.evaluate(() => findActive.id);
  await page.evaluate(() => {
    handleReplay({ type: 'user_message', seq: 0, text: 'history question 00 older prefix' });
    const bubble = addAssistantMessage(); bubble.appendBody('history question 00 new token');
  });
  expect(await page.evaluate(() => findActive.id)).toBe(id);
  await expect(page.locator('#find-status')).toContainText('/3');
  await page.locator('#find-query').press('Enter');
  expect(await page.evaluate(() => findActive.id)).not.toBe(id);
  await page.locator('#find-query').press('Escape');
  await expect(page.locator('#find-bar')).toBeHidden();
});

test('UX-3 late history page cannot enter a new selection', async ({ page }) => {
  await open(page, 'history');
  await expect(page.locator('#history-status')).toBeVisible(LIVE);
  const result = await page.evaluate(async () => {
    const original = rpc;
    let finish;
    rpc = async (method, params) => method === 'session.history'
      ? new Promise((resolve) => { finish = resolve; }) : original(method, params);
    const loading = loadOlderHistory();
    state.selectionGeneration += 1;
    const before = document.querySelectorAll('.msg').length;
    finish({ events: [{ type: 'user_message', seq: 1, text: 'WRONG OWNER' }], has_more: false });
    await loading; rpc = original;
    return { before, after: document.querySelectorAll('.msg').length, wrong: document.querySelector('#transcript').textContent.includes('WRONG OWNER') };
  });
  expect(result.after).toBe(result.before);
  expect(result.wrong).toBe(false);
});

test('UX-3 late history failure cannot replace a newer session notice', async ({ page }) => {
  await open(page, 'history');
  await expect(page.locator('#history-status')).toBeVisible(LIVE);
  const result = await page.evaluate(async () => {
    const original = rpc;
    let fail;
    rpc = (method, params) => method === 'session.history'
      ? new Promise((_, reject) => { fail = reject; }) : original(method, params);
    const loading = loadOlderHistory();
    state.selectionGeneration += 1;
    updateRunState('NEW SESSION NOTICE');
    fail(new Error('OLD SESSION FAILURE'));
    await loading; rpc = original;
    return dom['run-state'].textContent;
  });
  expect(result).toBe('NEW SESSION NOTICE');
});

test('UX-3 Unicode case expansion keeps offsets in the original source', async ({ page }) => {
  await open(page);
  const source = 'İ 中文 needle';
  await page.evaluate((text) => addUserMessage(text, []), source);
  await page.click('#find-open');
  await page.fill('#find-query', 'needle');
  const offset = await page.evaluate(() => findActive.offset);
  expect(offset).toBe(source.indexOf('needle'));
  await expect(page.locator('#find-preview')).toContainText(source);
});

test('CAP-1 non-Git is explicit and file diffs stay safe and lazy', async ({ page }) => {
  await open(page);
  await openWorkbenchTool(page, 'review-open');
  await expect(page.locator('#review-files')).toContainText('not_repository', LIVE);
  await page.getByRole('button', { name: 'Close workspace review' }).click();
  await page.evaluate(() => {
    window.reviewCalls = [];
    const original = rpc;
    rpc = async (method, params) => {
      if (method === 'workspace.changes') return { state: 'available', note: 'Existing edits; not agent attribution',
        files: [{ path: '<script>.txt', status: 'MM', staged: true, unstaged: true }] };
      if (method === 'workspace.diff') {
        window.reviewCalls.push(params.path);
        return { path: params.path, state: 'available', note: 'Workspace snapshot', staged: '+<script>window.pwned=true</script>', unstaged: '-previous\n+current' };
      }
      return original(method, params);
    };
  });
  await openWorkbenchTool(page, 'review-open');
  await expect(page.locator('.review-file')).toContainText('<script>.txt');
  expect(await page.evaluate(() => window.reviewCalls)).toEqual([]);
  await page.locator('.review-file').click();
  await expect(page.locator('#review-patch')).toContainText('<script>window.pwned=true</script>');
  await expect(page.locator('.review-diff')).toHaveCount(2);
  expect(await page.evaluate(() => Boolean(window.pwned))).toBe(false);
  expect(await page.evaluate(() => window.reviewCalls)).toEqual(['<script>.txt']);
});

test('CAP-1 real Git dirty files and external edits survive a cancelled run', async ({ page }, testInfo) => {
  await open(page, 'long-stream');
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  expect(root).toContain('clat-application-serve-e2e-long-stream-');
  const git = (...args) => execFileSync('git', ['-c', 'commit.gpgSign=false', ...args], { cwd: root, timeout: 5000 });
  git('init', '-q');
  fs.writeFileSync(path.join(root, 'prior-user-edit.txt'), 'staged user content\n');
  git('add', 'prior-user-edit.txt');
  fs.writeFileSync(path.join(root, 'prior-user-edit.txt'), 'unstaged user content\n');
  const before = fs.readFileSync(path.join(root, '.git/index'));
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt', 'long stream');
  await page.click('#send');
  await expect(page.locator('#cancel')).toBeVisible(LIVE);
  // Real separate process effect, intentionally without a model edit event.
  execFileSync(process.execPath, ['-e', 'require("fs").writeFileSync(process.argv[1], "external process edit\\n")', path.join(root, 'external-edit.txt')], { timeout: 5000 });
  await page.click('#cancel');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await openWorkbenchTool(page, 'review-open');
  await expect(page.locator('#review-files')).toContainText('prior-user-edit.txt', LIVE);
  await expect(page.locator('#review-files')).toContainText('external-edit.txt');
  await expect(page.locator('#review-status')).toContainText('not agent or run attribution');
  await page.locator('.review-file').filter({ hasText: 'prior-user-edit.txt' }).click();
  await expect(page.locator('#review-patch')).toContainText('+staged user content');
  await expect(page.locator('#review-patch')).toContainText('+unstaged user content');
  expect(fs.readFileSync(path.join(root, '.git/index'))).toEqual(before);

  await page.setViewportSize({ width: 390, height: 844 });

  await page.getByRole('button', { name: 'Close workspace review' }).click();
  await page.click('#find-open');
  await page.fill('#find-query', 'long stream');
  await expect(page.locator('#find-status')).toContainText('1/1');

});
