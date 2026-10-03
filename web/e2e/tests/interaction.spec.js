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
  const result = await page.evaluate(async (steering) => {
    state.runActive = steering; state.switching = false; state.compactionActive = false;
    observeComposerSession(null, 900);
    dom.prompt.value = 'acknowledged first submission'; state.composerGeneration += 1; saveComposerText();
    const original = rpc;
    let acknowledge;
    rpc = (method, params) => method === (steering ? 'steer.send' : 'prompt.send')
      ? new Promise((resolve) => { acknowledge = resolve; }) : original(method, params);
    const pending = submitPrompt();
    observeComposerSession('materialized-A', 900);
    observeComposerSession('session-B', 901);
    dom.prompt.value = 'B is still editing'; state.composerGeneration += 1; saveComposerText();
    acknowledge(steering ? { outcome: 'queued' } : {}); await pending; rpc = original;
    const current = dom.prompt.value;
    observeComposerSession('materialized-A', 902);
    return { current, restored: dom.prompt.value };
  }, steering);
  expect(result.current).toBe('B is still editing');
  expect(result.restored).toBe('');
});
}

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
  await page.click('#new-session');
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
  await page.screenshot({ path: testInfo.outputPath('workspace-review-desktop.png') });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: testInfo.outputPath('workspace-review-phone.png') });
  await page.getByRole('button', { name: 'Close workspace review' }).click();
  await page.click('#find-open');
  await page.fill('#find-query', 'long stream');
  await expect(page.locator('#find-status')).toContainText('1/1');
  await page.screenshot({ path: testInfo.outputPath('conversation-find-phone.png') });
});
