const { test, expect } = require('@playwright/test');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { hostInfo, openWorkbench, fetchBrowserRequest, LIVE, addWorkbenchProject: addProject } = require('../helpers/workbench');
test.setTimeout(45_000);

test('same-window projects retain drafts and reconcile late responses with their original owner', async ({ page }, info) => {
  const entry = hostInfo('workbench');
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-context-')));
  let release;
  try {
    await openWorkbench(page, entry);
    await page.evaluate(() => { window.projectDocumentWitness = crypto.randomUUID(); });
    const witness = await page.evaluate(() => window.projectDocumentWitness);
    await page.fill('#prompt', 'original project draft');
    await page.setInputFiles('#attachment-input', path.resolve(__dirname, '../../icons/icon-192.png'));
    await expect(page.locator('.attachment-chip')).toHaveCount(1, LIVE);
    await addProject(page, root);
    await expect(page.locator('#prompt')).toHaveValue('');
    await expect(page.locator('.attachment-chip')).toHaveCount(0);
    expect(await page.evaluate(() => window.projectDocumentWitness)).toBe(witness);
    await page.fill('#prompt', 'second project draft');
    const secondUrl = page.url();
    await page.click('[data-project-route="default"]');
    await expect(page.locator('#prompt')).toHaveValue('original project draft');
    await expect(page.locator('#settings-dialog')).toBeVisible();
    await page.click('#nav-workbench');
    await expect(page.locator('.attachment-chip')).toHaveCount(1);

    // Hold an actual old-project read while the visible project changes.
    const delayed = new Promise(resolve => { release = resolve; });
    let issued;
    const started = new Promise(resolve => { issued = resolve; });
    await page.route('**/api/workbench.info', async route => {
      const reply = await fetchBrowserRequest(route);
      issued(); await delayed; await route.fulfill({ response: reply });
    }, { times: 1 });
    await page.click('#inspector-toggle');
    await page.locator('.session-technical > summary').click();
    await page.click('#inspector-refresh');
    await started;
    const secondRoute = new URL(secondUrl).searchParams.get('workspace');
    await page.click(`[data-project-route="${secondRoute}"]`);
    release();
    await expect(page.locator('#project-root')).toHaveText(root, LIVE);
    await expect(page.locator('#prompt')).toHaveValue('second project draft');
    await expect.poll(() => page.evaluate(() => CLATWorkbenchState.snapshot().text)).toBe(true);
    await page.goBack();
    await expect(page.locator('#prompt')).toHaveValue('original project draft');
    await page.goForward();
    await expect(page.locator('#prompt')).toHaveValue('second project draft');
    await page.setViewportSize({ width: 1487, height: 1058 });
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await page.click('#inspector-toggle');

    await page.click('#project-list-toggle');

    await page.setViewportSize({ width: 390, height: 844 });
    await page.click('#inspector-close');
    await page.click('#mobile-sidebar-open');

    await page.reload();
    await expect(page.locator('#project-root')).toHaveText(root, LIVE);
  } finally {
    release?.();
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('native settings follow the active project and rejected installation leaves every project editable', async ({ page }) => {
  const entry = hostInfo('success');
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-native-')));
  try {
    await openWorkbench(page, entry);
    await page.evaluate(() => {
      window.__TAURI__ = { event: { listen: async () => () => {} }, core: { invoke: async () => ({
        desktopVersion: 'test', backendVersion: 'test', hostVersion: 'test', hostConnected: true,
      }) } };
    });
    await page.route('**/desktop-bridge-fixture.js', route => route.fulfill({
      contentType: 'application/javascript', body: fs.readFileSync(path.resolve(__dirname, '../../../desktop/ui/bridge.js'), 'utf8'),
    }));
    await page.addScriptTag({ url: entry.origin + '/desktop-bridge-fixture.js' });
    await page.fill('#prompt', 'protected in the inactive project');
    const second = await addProject(page, root);
    await page.evaluate(() => CLATDesktop.showSettings());
    await expect(page.locator('#settings-dialog')).toBeVisible();
    await expect(page.locator('#clat-desktop-settings')).toBeVisible();
    await page.click('#nav-workbench');
    const rejected = await page.evaluate(() => window.dispatchEvent(new CustomEvent('clat-desktop-install-lock', { cancelable: true })));
    expect(rejected).toBe(false);
    await expect(page.locator('#prompt')).toBeEnabled();
    await page.click('[data-project-route="default"]');
    await expect(page.locator('#prompt')).toHaveValue('protected in the inactive project');
    await page.click('#nav-workbench');
    await page.fill('#prompt', '');
    await page.click(`[data-project-route="${second}"]`);
    expect(await page.evaluate(() => window.dispatchEvent(new CustomEvent('clat-desktop-install-lock', { cancelable: true })))).toBe(true);
    await expect(page.locator('#prompt')).toBeDisabled();
    await page.click('[data-project-route="default"]');
    await expect(page.locator('#project-root')).toHaveText(root);
    const lockedUrl = page.url();
    await page.goBack();
    await expect(page).toHaveURL(lockedUrl);
    await expect(page.locator('#project-root')).toHaveText(root);
    await page.evaluate(() => window.dispatchEvent(new Event('clat-desktop-install-unlock')));
    await page.click('[data-project-route="default"]');
    await expect(page.locator('#prompt')).toBeEnabled();
    await page.evaluate(() => CLATDesktop.showSettings());
    await expect(page.locator('#clat-desktop-settings')).toBeVisible();
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('an accepted prompt receipt updates its original project while another project is being edited', async ({ page }) => {
  const entry = hostInfo('run-command');
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-receipt-')));
  let release;
  try {
    await openWorkbench(page, entry);
    let committed;
    const accepted = new Promise(resolve => { committed = resolve; });
    const delayed = new Promise(resolve => { release = resolve; });
    await page.route('**/api/prompt.send', async route => {
      const reply = await fetchBrowserRequest(route);
      expect(reply.ok()).toBe(true); committed(); await delayed;
      await route.fulfill({ response: reply });
    }, { times: 1 });
    await page.fill('#prompt', 'submitted in the original project');
    await page.click('#send'); await accepted;
    await page.fill('#prompt', 'new text after submitting');
    const second = await addProject(page, root);
    await page.fill('#prompt', 'independent second project text');
    release();
    await expect(page.locator('#prompt')).toHaveValue('independent second project text');
    await expect(page.locator('.msg.user')).toHaveCount(0);
    await page.click('[data-project-route="default"]');
    await page.click('#nav-workbench');
    await expect(page.locator('.msg.user')).toContainText('submitted in the original project', LIVE);
    await expect(page.locator('#prompt')).toHaveValue('new text after submitting');
    await expect(page.locator('#send')).toBeEnabled(LIVE);
    await page.click(`[data-project-route="${second}"]`);
    await expect(page.locator('#prompt')).toHaveValue('independent second project text');
  } finally { release?.(); fs.rmSync(root, { recursive: true, force: true }); }
});

test('reading position survives visiting a function page and switching projects', async ({ page }) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-reading-')));
  const anchor = () => {
    const viewport = document.querySelector('#transcript-scroll').getBoundingClientRect();
    return [...document.querySelectorAll('#transcript > .msg')].find(node => {
      const rect = node.getBoundingClientRect(); return rect.bottom > viewport.top && rect.top < viewport.bottom;
    })?.dataset.seq;
  };
  try {
    await openWorkbench(page, hostInfo('history'));
    await expect(page.locator('#transcript > .msg').first()).toBeVisible(LIVE);
    await page.locator('#transcript-scroll').evaluate(node => { node.style.scrollBehavior = 'auto'; node.scrollTop = node.scrollHeight * 0.45; });
    const before = await page.evaluate(anchor);
    expect(before).toBeTruthy();
    await addProject(page, root);
    await page.click('[data-project-route="default"]');
    await page.click('#nav-workbench');
    await expect.poll(() => page.evaluate(anchor)).toBe(before);
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('inactive project interactions retain their original cards and custom answer drafts', async ({ page }) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-interaction-')));
  let release;
  try {
    await openWorkbench(page, hostInfo('workbench'));
    await page.evaluate(() => {
      window.originalProjectQuestion = onQuestionNotice;
      onQuestionNotice({ kind: 'question_requested', payload: { rpc_id: 'project-first',
        question: { question: 'First original project question', options: [], allow_custom: true } } });
    });
    await page.locator('[data-rpc-id="project-first"] textarea').fill('unsubmitted custom answer');
    await page.locator('[data-rpc-id="project-first"]').evaluate(node => { window.originalAnswerCard = node; });
    let answerIssued;
    const issued = new Promise(resolve => { answerIssued = resolve; });
    const delayed = new Promise(resolve => { release = resolve; });
    await page.route('**/api/question.respond', async route => {
      answerIssued(); await delayed;
      await route.fulfill({ status: 503, json: { ok: false, error: { code: 'internal', message: 'fixture temporary failure' } } });
    }, { times: 1 });
    await page.locator('[data-rpc-id="project-first"] button[type="submit"]').click();
    await issued;
    const second = await addProject(page, root);
    release();
    await expect.poll(() => page.evaluate(() => originalAnswerCard.dataset.sending)).toBe('false');
    await page.evaluate(() => originalProjectQuestion({ kind: 'question_requested', payload: {
      rpc_id: 'project-second', question: { question: 'Second original project question', options: [], allow_custom: true },
    } }));
    await expect(page.locator('.question-card')).toHaveCount(0);
    await page.click('[data-project-route="default"]');
    await page.click('#nav-workbench');
    await expect(page.locator('[data-rpc-id="project-first"] textarea')).toHaveValue('unsubmitted custom answer');
    await expect(page.locator('[data-rpc-id="project-first"] textarea')).toBeEnabled();
    await expect(page.locator('[data-rpc-id="project-second"]')).toHaveCount(1);
    await page.click(`[data-project-route="${second}"]`);
    await expect(page.locator('[data-project-route="default"] small')).toBeVisible();
    await page.evaluate(() => {
      for (const rpc_id of ['project-first', 'project-second']) originalProjectQuestion({ kind: 'question_resolved', payload: { rpc_id } });
    });
    await expect(page.locator('[data-project-route="default"] small')).toHaveCount(0);
  } finally { release?.(); fs.rmSync(root, { recursive: true, force: true }); }
});

test('a completed project run clears a previously running directory snapshot', async ({ page }) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'clat-project-running-')));
  let capturedRunning = null;
  try {
    await openWorkbench(page, hostInfo('long-stream'));
    // Hold a genuine earlier activity snapshot while the local stream advances.
    await page.route('**/api/workspace.directory', async route => {
      const reply = await fetchBrowserRequest(route), body = await reply.json();
      const index = body.value.projects.findIndex(row => row.route_id === 'default');
      if (body.value.projects[index]?.run_active) capturedRunning = body.value.projects[index];
      if (capturedRunning) body.value.projects[index] = capturedRunning;
      await route.fulfill({ response: reply, json: body });
    });
    await page.fill('#prompt', 'keep streaming while another project opens');
    await page.click('#send');
    await expect(page.locator('#cancel')).toBeVisible(LIVE);
    const second = await addProject(page, root);
    expect(capturedRunning).not.toBeNull();
    await expect(page.locator('[data-project-route="default"] small')).toBeVisible();
    await page.click('[data-project-route="default"]');
    await page.click('#nav-workbench');
    await page.click('#cancel');
    await expect(page.locator('#cancel')).toBeHidden(LIVE);
    await expect(page.locator('[data-project-route="default"] small')).toHaveCount(0);
    await page.click(`[data-project-route="${second}"]`);
    await expect(page.locator('[data-project-route="default"] small')).toHaveCount(0);
  } finally {
    await page.unrouteAll({ behavior: 'wait' });
    fs.rmSync(root, { recursive: true, force: true });
  }
});
