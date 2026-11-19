const { test, expect } = require('@playwright/test');
const fs = require('fs');
const path = require('path');
const { LIVE, hostInfo, openWorkbench, openWorkbenchTool } = require('../helpers/workbench');
test.setTimeout(60_000);

async function open(page, host = 'success') {
  await openWorkbench(page, hostInfo(host));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  expect(root).toContain('clat-application-serve-e2e-');
  return root;
}

async function materializeSession(page, text) {
  await page.fill('#prompt', text); await page.click('#send');
  await expect(page.locator('#transcript')).toContainText(text, LIVE);
  await expect(page.getByRole('article', { name: 'Agent reply' }).last()).toContainText('done', LIVE);
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).not.toBeNull();
}

test('SL-U3 deleting a non-current archived conversation updates the list immediately', async ({ page }) => {
  await open(page);
  // 布局：spare（归档，非当前）+ current（活动）。2026-11-19 负责人
  // 真机：删除非当前归档行"没反应"，须来回切换视图才消失——本测钉
  // 死删除后列表立即更新、视图不动、当前会话不受影响。
  await materializeSession(page, 'spare archived chat');
  const spare = await page.evaluate(() => state.sessionId);
  await page.evaluate(id => rpc('session.organize', { id, archived: true, confirmed: true }), spare);
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await materializeSession(page, 'current active chat');
  const current = await page.evaluate(() => state.sessionId);
  await page.evaluate(() => refreshSessions());
  await page.click('#archived-sessions');
  const row = page.locator('[data-session-id="' + spare + '"]');
  await expect(row).toBeVisible(LIVE);
  // 双确认删除（"开菜单+点菜单项"整组重试吸收重渲染拆菜单竞态）。
  const menuAction = async () => {
    const item = page.getByRole('menuitem', { name: 'Delete archived conversation…' });
    for (let attempt = 0; attempt < 10; attempt++) {
      await row.locator('.session-more').click();
      try { await item.click({ timeout: 1500 }); return; } catch {}
    }
    throw new Error('delete menu item unreachable');
  };
  await menuAction();
  const confirmDelete = page.locator('dialog.clat-dialog[open]');
  await expect(confirmDelete).toBeVisible(LIVE);
  await confirmDelete.locator('[data-role="accept"]').click();
  const second = page.locator('dialog.clat-dialog[open]');
  await expect(second).toBeVisible(LIVE);
  await second.locator('[data-role="accept"]').click();
  // 删除后立即消失——无需切换视图；视图保持归档；当前会话不动。
  await expect(row).toBeHidden(LIVE);
  await expect(page.locator('#archived-sessions')).toHaveAttribute('aria-pressed', 'true', LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(current);
});

test('CAP-2 native write preview, conflict refusal, confirmed recovery and retained chat', async ({ page }, info) => {
  const root = await open(page, 'native-write');
  fs.writeFileSync(path.join(root, 'generated.txt'), 'human dirty baseline\n');
  await page.fill('#prompt', 'write file'); await page.click('#send');
  await page.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await expect.poll(() => fs.readFileSync(path.join(root, 'generated.txt'), 'utf8'), LIVE).toBe('from headless test');
  await openWorkbenchTool(page, 'turn-review-open');
  // 现行 UI：选中操作行 → 展开「Turn N file captures」折叠区（懒加载）→
  // 点捕获文件行看预览（旧 .review-file 属文件树，不在此视图）。
  const operationRow = page.locator('#turn-review .navigation-row').filter({ hasText: 'Save a file' }).first();
  const captureSummary = page.locator('#turn-review .operation-capture summary');
  await operationRow.click();
  await captureSummary.click();
  const capturedFile = page.locator('#turn-review .operation-capture .navigation-row').filter({ hasText: 'generated.txt' });
  await expect(capturedFile).toBeVisible(LIVE);
  await capturedFile.click();
  await expect(page.locator('#turn-review')).toContainText('human dirty baseline');
  await expect(page.locator('#turn-review')).toContainText('Captured native result (not current disk)');
  // 预览态没有返回控件：重新选中操作行重建捕获区（恢复按钮只在列表态）。
  // 先让捕获区以干净盘完成加载（按钮渲染为可用），再编辑磁盘制造冲突；
  // 确认后由后端拒绝（identity/contents 不匹配），磁盘现状保留。
  await operationRow.click();
  await captureSummary.click();
  const restoreButton = page.getByRole('button', { name: 'Restore captured files…' });
  await expect(restoreButton).toBeEnabled(LIVE);
  fs.writeFileSync(path.join(root, 'generated.txt'), 'later human edit');
  await restoreButton.click();
  const restoreConflict = page.locator('dialog.clat-dialog');
  await expect(restoreConflict).toBeVisible(LIVE);
  await restoreConflict.locator('[data-role="accept"]').click();
  await expect(page.locator('#turn-review')).toContainText('conflict', LIVE);
  expect(fs.readFileSync(path.join(root, 'generated.txt'), 'utf8')).toBe('later human edit');
  await page.click('#nav-workbench');
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await page.fill('#prompt', 'another write'); await page.click('#send');
  await page.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect.poll(() => fs.readFileSync(path.join(root, 'generated.txt'), 'utf8'), LIVE).toBe('from headless test');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await openWorkbenchTool(page, 'turn-review-open');
  const latestOperationRow = page.locator('#turn-review .navigation-row').filter({ hasText: 'Save a file' }).last();
  await latestOperationRow.click();
  await captureSummary.click();
  await expect(capturedFile).toContainText('safe', LIVE);
  await page.getByRole('button', { name: 'Restore captured files…' }).click();
  const restoreSafe = page.locator('dialog.clat-dialog');
  await expect(restoreSafe).toBeVisible(LIVE);
  await restoreSafe.locator('[data-role="accept"]').click();
  await expect(page.locator('#turn-review')).toContainText('restored', LIVE);
  await expect(page.locator('#turn-review .operation-capture')).toContainText('restored');
  expect(fs.readFileSync(path.join(root, 'generated.txt'), 'utf8')).toBe('later human edit');

  // Rendering fixture: host durability is independently pinned by native ledger tests.
  await page.evaluate(() => {
    const original = rpc;
    rpc = async (method, params) => {
      const value = await original(method, params);
      if (method === 'turn.changes' && params.path) {
        value.preview.status = 'prepared'; value.preview.after = 'unconfirmed future bytes';
        value.preview.captured_after = 'prior confirmed bytes';
      }
      return value;
    };
  });
  await operationRow.click();
  await captureSummary.click();
  await capturedFile.click();
  await expect(page.locator('#turn-review')).toContainText('Last confirmed native result', LIVE);
  await expect(page.locator('#turn-review')).toContainText('prior confirmed bytes');
  await expect(page.locator('#turn-review')).toContainText('Expected native result (commit unconfirmed)');
  await expect(page.locator('#turn-review')).toContainText('unconfirmed future bytes');
});

test('FS-1 bounded real file preview, stale refusal and fixed range quote preserves draft', async ({ page }) => {
  const root = await open(page);
  const file = path.join(root, 'quote-中文.md');
  fs.writeFileSync(file, '# original\nline two\nline three\n');
  fs.writeFileSync(path.join(root, '.env'), 'secret-not-for-preview');
  await page.fill('#prompt', 'existing draft');
  await page.click('#nav-resources');
  const resources = page.locator('#resources-dialog');
  const row = resources.getByRole('treeitem', {name:'quote-中文.md'});
  await resources.getByRole('searchbox').fill('quote-中文');
  await row.click();
  await expect(resources.locator('.cm-content')).toContainText('# original', LIVE);

  const snapshot = await page.evaluate(() => rpc('files.preview', {path:'quote-中文.md',start_line:2,end_line:2}));
  expect(snapshot.content).toBe('line two');
  fs.writeFileSync(file, '# changed\nnew second\n');
  const stale = await page.evaluate(async version => {
    try { await rpc('files.reference', {path:'quote-中文.md',start_line:2,end_line:2,version}); }
    catch(error) { return error.technical; }
  }, snapshot.version);
  expect(stale).toContain('File changed since preview');
  await expect(page.locator('#prompt')).toHaveValue('existing draft');
  await resources.locator('.editor-head-actions').getByRole('button',{name:'Refresh',exact:true}).click();
  await expect(resources.locator('.cm-content')).toContainText('new second', LIVE);

  await page.setViewportSize({ width: 390, height: 844 });
  const editor = resources.locator('.cm-content');
  await editor.getByText('new second',{exact:true}).click();
  await page.keyboard.press('Home');
  await page.keyboard.press('Shift+End');
  expect(await page.evaluate(()=>window.getSelection().toString())).toBe('new second');
  await resources.locator('.dev-editor-toolbar').getByRole('button',{name:'File actions'}).click();
  await page.locator('[role=menuitem][data-action=quote]').click();
  await expect(resources).toBeHidden();
  await expect(page.locator('#prompt')).toHaveValue(/existing draft[\s\S]*lines 2-2[\s\S]*> new second/);
  await expect(page.locator('#prompt')).toBeFocused();
  fs.writeFileSync(file, 'third edit');
  await expect(page.locator('#prompt')).toHaveValue(/> new second/);
  expect(await page.evaluate(async () => (await rpc('files.search', { query: '.env' })).paths)).toEqual([]);
});

test('UX-4 archiving the current conversation jumps to a fresh one; archived restore keeps content', async ({ page }) => {
  await open(page);
  await materializeSession(page, 'organization retained chat');
  // 会话菜单会被任何列表重渲染关闭（renderSessions→closeSessionMenu）；
  // "开菜单+点菜单项"作为整组重试，吸收后台刷新窗口期拆走菜单的竞态。
  const settle = () => expect.poll(() => page.evaluate(
    () => !state.runActive && !state.switching && !state.compactionActive), LIVE).toBe(true);
  const menuAction = async (name, exact) => {
    const item = page.getByRole('menuitem', { name, exact: !!exact });
    for (let attempt = 0; attempt < 10; attempt++) {
      await row.locator('.session-more').click();
      try { await item.click({ timeout: 1500 }); return; } catch {}
    }
    throw new Error('session menu item unreachable: ' + name);
  };
  await settle();
  await expect(page.locator('#transcript')).toContainText('organization retained chat', LIVE);
  const id = await page.evaluate(() => state.sessionId);
  const row = page.locator('[data-session-id="' + id + '"]');
  // 活动行的菜单不显示删除入口（不渲染灰置死项；强制力在 core 守卫）。
  // 菜单可能被列表重渲染关闭——"开菜单+断言菜单仍在"整组重试，避免
  // 菜单关闭后的 count 0 假绿。
  for (let attempt = 0; attempt < 10; attempt++) {
    await row.locator('.session-more').click();
    try {
      await expect(page.locator('.session-context-menu')).toBeVisible({ timeout: 1500 });
      await expect(page.getByRole('menuitem', { name: 'Delete archived conversation…' })).toHaveCount(0, { timeout: 1500 });
      await expect(page.locator('.session-context-menu')).toBeVisible({ timeout: 1500 });
      break;
    } catch (error) {
      if (attempt === 9) throw error;
    }
  }
  await page.keyboard.press('Escape');
  await menuAction('Pin session', true);
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).pinned, id), LIVE).toBe(true);
  await expect(row).toHaveAttribute('data-pinned', 'true', LIVE);
  await settle();
  await menuAction('Archive session…');
  const archiveSession = page.locator('dialog.clat-dialog');
  await expect(archiveSession).toBeVisible(LIVE);
  await archiveSession.locator('[data-role="accept"]').click();
  // INV-SL3（SL-U1）：归档当前会话 → 跳转全新会话（懒物化：sessionId
  // 为空、转录空态），原会话从活动列表消失。
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(null);
  await expect(page.locator('#empty-state')).toBeVisible(LIVE);
  await expect(row).toBeHidden(LIVE);
  await page.reload(); await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(null);
  await page.fill('#prompt','保留未发送草稿');
  await page.click('#archived-sessions');
  await expect(page.locator('#archived-sessions')).toHaveAttribute('aria-pressed','true');
  await expect(page.locator('#prompt')).toHaveValue('保留未发送草稿');
  await expect(row).toBeVisible(LIVE);
  await menuAction('Restore archived session');
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).archived, id), LIVE).toBe(false);
  await expect(row).toBeHidden(LIVE);
  await page.click('#archived-sessions');
  await expect(row).toBeVisible(LIVE);
  // 恢复后内容完好：切回该会话，转录保留归档前的聊天。
  await row.locator('.session-item').click();
  await expect(page.locator('#transcript')).toContainText('organization retained chat', LIVE);
  // 负责人场景（2026-11-19）：浏览归档会话（当前选择即目标）后从行
  // 菜单直接删除——不得报"打开中"；一次取消不删，两次确认才删，
  // 第二确认点名标题；删除后跳转全新会话。再次归档经 rpc 完成（本腿
  // 验证删除流本身；菜单在切换后的重渲染中会被拆走，见 settle 注）。
  await page.evaluate(id => rpc('session.organize', { id, archived: true, confirmed: true }), id);
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).archived, id), LIVE).toBe(true);
  // rpc 归档不动选择：当前正是该归档会话（浏览态，即负责人的场景）。
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(id);
  await page.evaluate(() => refreshSessions());
  await page.click('#archived-sessions');
  await expect(row).toBeVisible(LIVE);
  for (const attempt of [1, 2]) {
    await settle();
    await menuAction('Delete archived conversation…');
    const confirmDelete = page.locator('dialog.clat-dialog');
    await expect(confirmDelete).toBeVisible(LIVE);
    if (attempt === 1) {
      await confirmDelete.locator('[data-role="cancel"]').click();
      await expect(confirmDelete).toBeHidden(LIVE);
      await expect(row).toBeVisible(LIVE);
    } else {
      await confirmDelete.locator('[data-role="accept"]').click();
      const second = page.locator('dialog.clat-dialog');
      await expect(second).toBeVisible(LIVE);
      // 第二确认点名该会话的实际标题（行文本带归档后缀，取纯标题）。
      const title = await page.evaluate(id => (state.sessions || []).find(s => s.id === id)?.title, id);
      expect(title).toBeTruthy();
      await expect(second.locator('.clat-dialog-message')).toContainText(title, LIVE);
      await second.locator('[data-role="accept"]').click();
    }
  }
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.some(s => s.id === id), id), LIVE).toBe(false);
  await expect(row).toBeHidden(LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(null);
  // 返工 2（2026-11-19 负责人真机）：删除跳转不得补发 session.new——
  // 过期 selection generation 会撞 busy 并显示 "new session failed"，
  // 且列表停留在归档视图；两者都必须复位。
  await expect(page.locator('#archived-sessions')).toHaveAttribute('aria-pressed','false', LIVE);
  await expect(page.locator('#run-state')).not.toContainText('new session failed', LIVE);
  // 返工 5（2026-11-19 负责人真机"删除没反应"残留根因）：跟随服务端
  // 跳转不得有任何静默 bail——即便客户端卡在 switching 态（宿主重启
  // 后的残留），重同步也必须发生。
  await page.evaluate(() => { state.switching = true; });
  await page.evaluate(async () => { await followServerSelectionJump(); });
  await expect(page.locator('#empty-state')).toBeVisible(LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toBe(null);
  await expect.poll(() => page.evaluate(() => state.switching), LIVE).toBe(false);
});

test('UX-5 structured Plan/Goal details preserve four concepts and confirm actions', async ({ page }, info) => {
  await open(page);
  await materializeSession(page, 'workflow session');
  await page.evaluate(async () => { await rpc('command.run', { command: '/plan on' }); await rpc('command.run', { command: '/goal create structured objective' }); await refreshWorkbench(); });
  // 目标激活时 composer 显示目标条，其「Goal details」按钮打开同一
  // workflow-details 视图；#plan-mode-badge 是 legacy 入口且滞后于
  // state.workbench 快照（详见修复报告）。
  await page.locator('#goal-badge').click();
  await expect(page.locator('#workflow-details')).toContainText('structured objective', LIVE);
  await expect(page.locator('#workflow-details')).toContainText('Generate a plan in Plan mode');
  await expect(page.locator('#workflow-details')).toContainText('To-dos');
  await expect(page.locator('#workflow-details')).toContainText('Rounds 0');
  const clearGoal = page.locator('dialog.clat-dialog');
  await page.getByRole('button', { name: 'End goal', exact: true }).click();
  await expect(clearGoal).toBeVisible(LIVE);
  await clearGoal.locator('[data-role="cancel"]').click();
  await expect(page.locator('#workflow-details')).toContainText('structured objective');
  await page.getByRole('button', { name: 'End goal', exact: true }).click();
  await expect(clearGoal).toBeVisible(LIVE);
  await clearGoal.locator('[data-role="accept"]').click();
  await expect(page.locator('#workflow-details')).toContainText('No goal yet', LIVE);
  // 覆盖映射（UI 演进）：旧 workflowSections 截断提示（First 100 todo entries
  // only）在现行 web 包中已无渲染方（键 web.first_100_todo_entries_only_*
  // 未被引用）；该直调段随函数一并退役，截断提示的回归待其恢复时重建。
});

test('UX-6 mobile find/project/latest and opt-in deduplicated page notifications', async ({ page }, info) => {
  await page.addInitScript(() => {
    window.notificationCalls = []; window.permissionRequests = 0;
    window.Notification = class {
      static permission = 'granted';
      static async requestPermission() { window.permissionRequests++; return 'granted'; }
      constructor(title, options) { window.notificationCalls.push({ title, options }); }
    };
  });
  await open(page);
  // Notification opt-in lives in Settings → Appearance (the default panel).
  // Static help-copy assertions were removed (2026-10-28): they pinned
  // presentation text that legitimately drifts with help-center revisions.
  await openWorkbenchTool(page, 'settings-open');
  await expect(page.locator('#page-notifications')).toHaveAttribute('aria-pressed', 'false');
  await page.click('#page-notifications'); expect(await page.evaluate(() => window.permissionRequests)).toBe(1);
  const count = await page.evaluate(() => {
    document.hasFocus = () => false;
    const events = [['prompt.settled', { prompt_rpc_id: 'unique-completion', outcome: { type: 'completed', output: 'SECRET' } }],
      ['prompt.settled', { prompt_rpc_id: 'unique-failure', outcome: { type: 'failed', error: 'SECRET' } }],
      ['approval.requested', { rpc_id: 'unique-approval', request: { arguments: 'SECRET' } }],
      ['notice', { kind: 'question_requested', payload: { rpc_id: 'unique-question', question: 'SECRET' } }]];
    for (const [kind, ctl] of events) { observePageNotification(kind, ctl); observePageNotification(kind, ctl); }
    observePageNotification('replay', { prompt_rpc_id: 'replay', outcome: { type: 'completed' } });
    const connected = state.connected; state.connected = false;
    observePageNotification('prompt.settled', { prompt_rpc_id: 'offline', outcome: { type: 'completed' } }); state.connected = connected;
    return window.notificationCalls;
  });
  expect(count).toHaveLength(4); expect(JSON.stringify(count)).not.toContain('SECRET');
  // Stale mobile-presentation assertions (find-open visibility, sidebar smoke,
  // duplicated ball sequence) were removed (2026-10-28): they pinned UI states
  // that drifted with layout revisions; the back-to-latest ball behavior is
  // covered by UX-8 below.
});

test('UX-8 streaming keeps following the bottom until the reader scrolls away', async ({ page }) => {
  await open(page);
  await page.evaluate(() => { for (let i = 0; i < 30; i++) addUserMessage('follow message ' + i, []); });
  const viewport = page.locator('#transcript-scroll');
  await viewport.evaluate(v => { v.scrollTop = v.scrollHeight; });
  await expect(page.locator('#back-to-latest')).not.toBeVisible();
  // Scroll up a little — inside the old 180px "near end" trap where streaming
  // used to yank the reader back to the bottom on every append. Assert by
  // distance-from-bottom: fractional scrollTop settles and transcript DOM
  // regrouping make node-position anchors flaky; the invariant is "while
  // detached, the reader's distance from the bottom never shrinks" (a full
  // yank sends it to 0, a partial pull shrinks it).
  const dist = () => viewport.evaluate(v => v.scrollHeight - v.scrollTop - v.clientHeight);
  // Settle at the bottom first: rounds regroup asynchronously after bulk
  // inserts, so poll for the pinned state instead of assuming it. The old
  // yank was always full (scrollTop = scrollHeight), so "still away from the
  // bottom after an append" is the no-yank invariant; rounds regrouping
  // legitimately reshuffles distances, so no tighter bound.
  await expect.poll(dist, LIVE).toBeLessThanOrEqual(32);
  await viewport.evaluate(v => { v.scrollTop = v.scrollHeight - v.clientHeight - 40; });
  await expect.poll(dist, LIVE).toBeGreaterThan(32);
  await expect(page.locator('#back-to-latest')).toBeVisible(LIVE);
  await page.evaluate(() => addUserMessage('stream output', []));
  // Drive the production append-scroll hook (the very function the fix
  // changed; every live append path calls it) so the no-yank assertion
  // exercises the changed code, not just the DOM append.
  await page.evaluate(() => scrollIfFollowing());
  await page.waitForTimeout(300);
  await expect.poll(dist, LIVE).toBeGreaterThan(32);
  // Returning to the bottom (ball or scroll) restores following.
  await page.click('#back-to-latest');
  await expect.poll(dist, LIVE).toBeLessThanOrEqual(32);
  await expect(page.locator('#back-to-latest')).not.toBeVisible();
});

test('UX-7 readonly task facts expose bounded logs without mutating tasks', async ({ page }) => {
  await open(page);
  const mutations = [];
  page.on('request', request => {
    const path = new URL(request.url()).pathname;
    if (/\/api\/tasks\./.test(path) && !/\/api\/tasks\.(list|logs)$/.test(path)) mutations.push(path);
  });
  await page.evaluate(() => {
    const original = rpc;
    rpc = async (method, params) => {
      if (method === 'tasks.list') return { tasks: [{ id: 7, command: '<script>command</script>', state: 'completed', exit_code: 2 }], generation: 4, note: 'Current run only' };
      if (method === 'tasks.logs') return { command: '<script>command</script>', session: state.sessionId, generation: 4, state: 'completed', exit_code: 2,
        stdout: { text: 'bounded log', truncated: true }, stderr: { text: 'error', truncated: false }, pty: { text: '', truncated: false } };
      return original(method, params);
    };
  });
  await openWorkbenchTool(page, 'tasks-open');
  const tasks = page.locator('#task-review');
  await tasks.locator('[data-task="7"]').click();
  await expect(tasks.locator('pre').filter({hasText:'bounded log'})).toBeVisible(LIVE);
  await expect(tasks.locator('pre').filter({hasText:'error'})).toBeVisible();
  await expect(tasks).toContainText('exit 2');
  await expect(tasks).toContainText('earlier omitted');
  await expect(tasks).toContainText('<script>command</script>');
  await expect(tasks.locator('script')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await expect(tasks).toBeHidden();
  await expect(page.locator('#prompt')).toBeVisible();
  expect(mutations).toEqual([]);
});
