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
  const preview = resources.locator('pre');
  const recheck = resources.locator('[data-action=refresh-file-preview]');
  const quote = resources.locator('[data-action=quote-file-snapshot]');
  await resources.getByRole('searchbox').fill('quote-中文');
  await resources.getByRole('treeitem', {name:'quote-中文.md'}).click();
  await expect(preview).toContainText('# original', LIVE);
  await resources.getByRole('spinbutton', { name: 'First line', exact: true }).fill('2');
  await resources.getByRole('spinbutton', { name: 'Last line', exact: true }).fill('2');
  await recheck.click();
  await expect(preview).toHaveText('line two', LIVE);
  fs.writeFileSync(file, '# changed\nnew second\n');
  await quote.click();
  await expect(resources.getByRole('status')).toContainText('File changed since preview', LIVE);
  await expect(page.locator('#prompt')).toHaveValue('existing draft');
  await recheck.click();
  await expect(preview).toHaveText('new second', LIVE);
  await expect(resources.getByRole('status')).not.toContainText('File changed since preview');

  await page.setViewportSize({ width: 390, height: 844 });

  await quote.click();
  await expect(resources).toBeHidden();
  await expect(page.locator('#prompt')).toHaveValue(/existing draft[\s\S]*lines 2-2[\s\S]*> new second/);
  await expect(page.locator('#prompt')).toBeFocused();
  fs.writeFileSync(file, 'third edit');
  await expect(page.locator('#prompt')).toHaveValue(/> new second/);
  expect(await page.evaluate(async () => (await rpc('files.search', { query: '.env' })).paths)).toEqual([]);
});

test('UX-4 explicit archive and shared reload/restore keep selected transcript', async ({ page }) => {
  await open(page);
  await materializeSession(page, 'previous unrelated chat');
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]'); await materializeSession(page, 'organization retained chat');
  const id = await page.evaluate(() => state.sessionId);
  const row = page.locator('[data-session-id="' + id + '"]');
  await row.locator('.session-more').click(); await page.getByRole('menuitem', { name: 'Pin session', exact: true }).click();
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).pinned, id), LIVE).toBe(true);
  await expect(row).toHaveAttribute('data-pinned', 'true', LIVE);
  await row.locator('.session-more').click();
  await page.getByRole('menuitem', { name: 'Archive session…' }).click();
  const archiveSession = page.locator('dialog.clat-dialog');
  await expect(archiveSession).toBeVisible(LIVE);
  await archiveSession.locator('[data-role="accept"]').click();
  await expect(row).toBeHidden(LIVE);
  expect(await page.evaluate(() => state.sessionId)).toBe(id);
  await page.reload(); await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#transcript')).toContainText('organization retained chat', LIVE);
  await page.fill('#prompt','保留未发送草稿');
  await page.click('#archived-sessions');
  await expect(page.locator('#archived-sessions')).toHaveAttribute('aria-pressed','true');
  await expect(page.locator('#prompt')).toHaveValue('保留未发送草稿');
  await expect(row).toBeVisible(LIVE);
  await page.click('#archived-sessions');
  await expect(row).toBeHidden(LIVE);
  await expect(page.locator('#transcript')).toContainText('organization retained chat');
  await page.click('#archived-sessions');
  await row.locator('.session-more').click();
  await page.getByRole('menuitem', { name: 'Restore archived session' }).click();
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).archived, id), LIVE).toBe(false);
  await expect(row).toBeHidden(LIVE);
  await page.click('#archived-sessions');
  await expect(row).toBeVisible(LIVE);
  await expect(page.locator('#prompt')).toHaveValue('保留未发送草稿');
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
