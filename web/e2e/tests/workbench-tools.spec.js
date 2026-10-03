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
  await expect(page.locator('#turn-review .review-file')).toContainText('generated.txt', LIVE);
  await page.locator('#turn-review .review-file').click();
  await expect(page.locator('#turn-review')).toContainText('human dirty baseline');
  await expect(page.locator('#turn-review')).toContainText('Captured native result (not current disk)');
  fs.writeFileSync(path.join(root, 'generated.txt'), 'later human edit');
  page.once('dialog', d => d.accept());
  await page.getByRole('button', { name: 'Restore captured files…' }).click();
  await expect(page.locator('#turn-review')).toContainText('conflict', LIVE);
  expect(fs.readFileSync(path.join(root, 'generated.txt'), 'utf8')).toBe('later human edit');
  await page.getByRole('button', { name: 'Close captured operations' }).click();
  await page.click('#new-session');
  await page.fill('#prompt', 'another write'); await page.click('#send');
  await page.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect.poll(() => fs.readFileSync(path.join(root, 'generated.txt'), 'utf8'), LIVE).toBe('from headless test');
  await expect(page.locator('#cancel')).toBeHidden(LIVE);
  await openWorkbenchTool(page, 'turn-review-open');
  await expect(page.locator('#turn-review .review-file')).toContainText('safe', LIVE);
  page.once('dialog', d => d.accept());
  await page.getByRole('button', { name: 'Restore captured files…' }).click();
  await expect(page.locator('#turn-review')).toContainText('restored', LIVE);
  await expect(page.locator('#turn-review .review-file')).toContainText('restored');
  expect(fs.readFileSync(path.join(root, 'generated.txt'), 'utf8')).toBe('later human edit');
  await page.screenshot({ path: info.outputPath('captured-operations-desktop.png') });
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
  await page.locator('#turn-review .review-file').click();
  await expect(page.locator('#turn-review')).toContainText('Last confirmed native result', LIVE);
  await expect(page.locator('#turn-review')).toContainText('prior confirmed bytes');
  await expect(page.locator('#turn-review')).toContainText('Expected native result (commit unconfirmed)');
  await expect(page.locator('#turn-review')).toContainText('unconfirmed future bytes');
});

test('FS-1 bounded real file preview, stale refusal and fixed range quote preserves draft', async ({ page }, info) => {
  const root = await open(page);
  const file = path.join(root, 'quote-中文.md');
  fs.writeFileSync(file, '# original\nline two\nline three\n');
  fs.writeFileSync(path.join(root, '.env'), 'secret-not-for-preview');
  await page.fill('#prompt', 'existing draft');
  await openWorkbenchTool(page, 'files-open');
  await page.getByRole('searchbox', { name: 'Find project file' }).fill('quote-中文');
  await page.locator('#file-browser .review-file').click();
  await expect(page.locator('#file-browser')).toContainText('# original', LIVE);
  await page.getByRole('spinbutton', { name: 'First line', exact: true }).fill('2');
  await page.getByRole('spinbutton', { name: 'Last line', exact: true }).fill('2');
  await page.getByRole('button', { name: 'Read / recheck' }).click();
  await expect(page.locator('#file-browser pre')).toHaveText('line two', LIVE);
  fs.writeFileSync(file, '# changed\nnew second\n');
  await page.getByRole('button', { name: 'Quote snapshot to draft' }).click();
  await expect(page.locator('#file-browser')).toContainText('File changed since preview', LIVE);
  await expect(page.locator('#prompt')).toHaveValue('existing draft');
  await page.getByRole('button', { name: 'Read / recheck' }).click();
  await expect(page.locator('#file-browser pre')).toHaveText('new second', LIVE);
  await expect(page.locator('#file-browser')).not.toContainText('File changed since preview');
  await expect(page.locator('#file-browser')).toContainText('Modified');
  await page.screenshot({ path: info.outputPath('files-desktop.png') });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: info.outputPath('files-phone.png') });
  await page.getByRole('button', { name: 'Quote snapshot to draft' }).click();
  await expect(page.locator('#file-browser')).not.toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue(/existing draft[\s\S]*lines 2-2[\s\S]*> new second/);
  await expect(page.locator('#prompt')).toBeFocused();
  fs.writeFileSync(file, 'third edit');
  await expect(page.locator('#prompt')).toHaveValue(/> new second/);
  expect(await page.evaluate(async () => (await rpc('files.search', { query: '.env' })).paths)).toEqual([]);
});

test('UX-4 explicit archive and shared reload/restore keep selected transcript', async ({ page }) => {
  await open(page);
  await materializeSession(page, 'previous unrelated chat');
  await page.click('#new-session'); await materializeSession(page, 'organization retained chat');
  const id = await page.evaluate(() => state.sessionId);
  const row = page.locator('[data-session-id="' + id + '"]');
  await row.locator('.session-more').click(); await page.getByRole('menuitem', { name: 'Pin session', exact: true }).click();
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).pinned, id), LIVE).toBe(true);
  await row.locator('.session-more').click(); page.once('dialog', d => d.accept());
  await page.getByRole('menuitem', { name: 'Archive session…' }).click();
  await expect(row).toContainText('archived', LIVE);
  expect(await page.evaluate(() => state.sessionId)).toBe(id);
  await page.reload(); await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(page.locator('#transcript')).toContainText('organization retained chat', LIVE);
  await page.click('#archived-sessions'); await row.locator('.session-more').click();
  await page.getByRole('menuitem', { name: 'Restore archived session' }).click();
  await expect.poll(() => page.evaluate(async (id) => (await rpc('session.list', {})).sessions.find(s => s.id === id).archived, id), LIVE).toBe(false);
});

test('UX-5 structured Plan/Goal details preserve four concepts and confirm actions', async ({ page }, info) => {
  await open(page);
  await materializeSession(page, 'workflow session');
  await page.evaluate(async () => { await rpc('command.run', { command: '/plan on' }); await rpc('command.run', { command: '/goal create structured objective' }); await refreshWorkbench(); });
  await page.click('#plan-mode-badge');
  await expect(page.locator('#workflow-details')).toContainText('structured objective', LIVE);
  await expect(page.locator('#workflow-details')).toContainText('Approved plan (durable text)');
  await expect(page.locator('#workflow-details')).toContainText('Todo (model-maintained list; not Goal completion)');
  await expect(page.locator('#workflow-details')).toContainText('Rounds 0/8');
  page.once('dialog', d => d.dismiss()); await page.getByRole('button', { name: 'Clear goal', exact: true }).click();
  await expect(page.locator('#workflow-details')).toContainText('structured objective');
  page.once('dialog', d => d.accept()); await page.getByRole('button', { name: 'Clear goal', exact: true }).click();
  await expect(page.locator('#workflow-details')).toContainText('No current goal', LIVE);
  expect(await page.evaluate(async () => {
    const facts = await rpc('workflow.details', {}); facts.todos_truncated = true;
    const detail = el('section'); workflowSections(detail, facts);
    return detail.textContent.includes('First 100 todo entries only');
  })).toBe(true);
  await page.screenshot({ path: info.outputPath('workflow-desktop.png') });
});

test('UX-6 help, mobile find/project/latest and opt-in deduplicated page notifications', async ({ page }, info) => {
  await page.addInitScript(() => {
    window.notificationCalls = []; window.permissionRequests = 0;
    window.Notification = class {
      static permission = 'granted';
      static async requestPermission() { window.permissionRequests++; return 'granted'; }
      constructor(title, options) { window.notificationCalls.push({ title, options }); }
    };
  });
  await open(page);
  await openWorkbenchTool(page, 'help-open');
  await expect(page.locator('#help-dialog')).toContainText('no Web Push');
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
  await page.getByRole('button', { name: 'Close help', exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator('#find-open')).toBeVisible();
  await page.click('#mobile-sidebar-open');
  await page.click('#project-navigation'); await expect(page.locator('#settings-dialog')).toBeVisible();
  await page.locator('#settings-dialog').press('Escape');
  await page.evaluate(() => { for (let i = 0; i < 30; i++) addUserMessage('navigation message ' + i, []); });
  await page.locator('#transcript-scroll').evaluate(v => { v.scrollTop = v.scrollHeight; });
  await expect(page.locator('#back-to-latest')).not.toBeVisible();
  await page.locator('#transcript-scroll').evaluate(v => { v.scrollTop = 0; });
  await expect(page.locator('#back-to-latest')).toBeVisible(LIVE); await page.click('#back-to-latest');
  await expect(page.locator('#back-to-latest')).not.toBeVisible();
  await page.screenshot({ path: info.outputPath('navigation-phone.png') });
});

test('UX-7 readonly task facts expose bounded logs and cleanup explicitly, without new controls', async ({ page }) => {
  await open(page);
  await openWorkbenchTool(page, 'tasks-open');
  await expect(page.locator('#task-review')).toContainText('Not background jobs', LIVE);
  await page.getByRole('button', { name: 'Close tasks' }).click();
  await page.evaluate(() => {
    const original = rpc;
    rpc = async (method, params) => {
      if (method === 'tasks.list') return { tasks: [{ id: 7, command: '<script>command</script>', state: 'completed', exit_code: 2 }], generation: 4, note: 'Current run only' };
      if (method === 'tasks.logs') return { command: '<script>command</script>', session: state.sessionId, generation: 4, state: 'completed', exit_code: 2,
        stdout: { text: 'bounded log', truncated: true }, stderr: { text: 'error', truncated: false }, pty: { text: '', truncated: false } };
      return original(method, params);
    };
  });
  await openWorkbenchTool(page, 'tasks-open'); await page.locator('#task-review .review-file').click();
  await expect(page.locator('#task-review')).toContainText('bounded log', LIVE);
  await expect(page.locator('#task-review')).toContainText('exit 2');
  await expect(page.locator('#task-review')).toContainText('earlier omitted');
  await expect(page.locator('#task-review button')).toHaveCount(3);
  expect(await page.evaluate(() => document.querySelector('#task-review script') !== null)).toBe(false);
});
