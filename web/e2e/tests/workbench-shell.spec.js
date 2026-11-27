const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench, openWorkbenchTool, fetchBrowserRequest } = require('../helpers/workbench');

test('a fresh backend connection opens Computer Use before any manual reload', async ({ page }) => {
  const entry = hostInfo('workbench');
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  // Desktop opens a new document with the new Host instance and this fragment.
  await page.goto(`${entry.origin}/?host=fixture-replacement#/settings/computer-use`);
  await expect(page.locator('#landing')).toBeHidden(LIVE);
  await expect(page.locator('#conn-status')).toHaveAttribute('data-state', 'live', LIVE);
  const settings = page.locator('[data-settings-panel="computer-use"]');
  await expect(settings).toBeVisible();
  await expect(settings.locator('[data-action="refresh-accessibility"]')).toBeVisible();
  expect(errors).toEqual([]);
});

test('Computer Use refreshes a connected Windows Host without requesting macOS access', async ({ page }) => {
  const entry = hostInfo('workbench');
  let desktop = 'locked';
  const commands = [];
  await page.route('**/api/workbench.info', async route => {
    const response = await fetchBrowserRequest(route);
    const body = await response.json();
    body.value.builtin_availability['builtin.native_ui'] = { enabled: true };
    await route.fulfill({ response, json: body });
  });
  await page.route('**/api/command.run', async route => {
    const { command } = route.request().postDataJSON();
    if (!command.startsWith('/ui-consents')) return route.continue();
    commands.push(command);
    await route.fulfill({ json: { ok: true, value: {
      kind: 'ui_consents', system: { platform: 'windows', desktop }, consents: [],
    } } });
  });
  await page.goto(`${entry.origin}/#/settings/computer-use`);
  const settings = page.locator('[data-settings-panel="computer-use"]');
  await expect(settings.locator('[data-system-status="windows_locked"]')).toBeVisible(LIVE);
  await expect(settings.locator('[data-action="request-accessibility"]')).toBeHidden();
  desktop = 'ready';
  await settings.locator('[data-action="refresh-accessibility"]').click();
  await expect(settings.locator('[data-system-status="windows_ready"]')).toBeVisible(LIVE);
  expect(commands.length).toBeGreaterThanOrEqual(2);
  expect(new Set(commands)).toEqual(new Set(['/ui-consents']));
});

test('resources and the conversation files tool share the open editor and unsaved changes', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  await openWorkbench(page, hostInfo('workbench'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'global-extension-example.md');
  fs.writeFileSync(file, 'Original on disk');
  try {
    const editor = page.locator('#file-browser');
    const resources = page.locator('#resources-dialog');
    const content = editor.locator('.cm-content');
    await page.fill('#prompt', 'Unsent conversation draft');
    const nav = page.locator('#nav-resources');
    await expect(nav.locator('.shell-label')).toBeHidden();
    await nav.hover();
    await expect(nav.locator('.shell-label')).toBeVisible();
    await nav.click();
    await expect(resources.locator('#file-browser')).toBeVisible();
    await editor.getByRole('searchbox').fill('global-extension-example');
    await editor.getByRole('treeitem',{name:'global-extension-example.md'}).click();
    await expect(content).toContainText('Original on disk', LIVE);
    await content.fill('Unsaved in the shared editor');
    const section=resources.locator('#resource-section');
    for(const value of ['materials','bookmarks']){
      await section.click();
      await page.locator(`[role=menuitem][data-action=${value}]`).click();
      await expect(editor).toBeHidden();
    }
    await section.click();
    await page.locator('[role=menuitem][data-action=files]').click();
    await expect(content).toHaveText('Unsaved in the shared editor');

    await page.click('#nav-workbench');
    await openWorkbenchTool(page, 'files-open');
    await expect(page.locator('#workspace-tool-host #file-browser')).toBeVisible();
    await expect(content).toHaveText('Unsaved in the shared editor');
    expect(await editor.count()).toBe(1);
    expect(fs.readFileSync(file,'utf8')).toBe('Original on disk');
    await content.press('ControlOrMeta+s');
    await expect.poll(()=>fs.readFileSync(file,'utf8'),LIVE).toBe('Unsaved in the shared editor');

    await page.click('#nav-resources');
    await expect(resources.locator('#file-browser')).toBeVisible();
    await expect(content).toHaveText('Unsaved in the shared editor');
    await page.click('#nav-workbench');
    await expect(page.locator('#prompt')).toHaveValue('Unsent conversation draft');
  } finally { fs.rmSync(file, { force: true }); }
});

test('resources collapses expanded directories from the file actions menu', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  await openWorkbench(page, hostInfo('workbench'));
  await page.evaluate(() => CLATI18n.setLanguage('en'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const folder = path.join(root, 'resources-collapse-fixture');
  fs.mkdirSync(folder);
  fs.writeFileSync(path.join(folder, 'nested.md'), 'Nested file');
  try {
    await page.click('#nav-resources');
    const resources = page.locator('#resources-dialog');
    const directory = resources.getByRole('treeitem', { name: 'resources-collapse-fixture' });
    await directory.click();
    const nested = resources.getByRole('treeitem', { name: 'nested.md' });
    await expect(nested).toBeVisible(LIVE);
    await resources.locator('.resource-section-row').hover();
    const actions = resources.locator('.resource-section-actions');
    await expect(actions.getByRole('button', { name: 'Collapse all folders' })).toHaveCount(0);
    const more = actions.getByRole('button', { name: 'File actions' });
    await more.click();
    await expect(more).toHaveAttribute('aria-expanded', 'true');
    await page.getByRole('menuitem', { name: 'Collapse all folders' }).click();
    await expect(more).toBeFocused();
    await expect(directory).toHaveAttribute('aria-expanded', 'false');
    await expect(nested).toBeHidden();
  } finally {
    fs.rmSync(folder, { recursive: true, force: true });
  }
});

test('completed replies open project files and let users copy paths and preview URLs', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  const entry = hostInfo('workbench');
  await openWorkbench(page, entry);
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const directory = 'reply-artifact-fixture';
  const bare = 'application-breadcrumbs.js';
  const relative = `${directory}/${bare}`;
  const absolute = path.join(root, relative);
  const preview = entry.origin + '/reply_preview?view=latest';
  const localHost = 'http://127.0.0.1:2691/';
  const otherRoot = root + '-other';
  fs.mkdirSync(path.dirname(absolute));
  fs.writeFileSync(absolute, 'File made by the reply');
  try {
    await page.evaluate(({relative, bare, preview, localHost, otherRoot}) => {
      // Replay can deliver the transcript before the project metadata.
      const metadata = state.workbench;
      state.workbench = null;
      const reply = addAssistantMessage();
      reply.node.dataset.replyFixture = 'true';
      reply.appendBody(`File \`${relative}\`. Preview (${preview}).\n\nLocal Host: \`${localHost}\`\n\n[Outside](<${otherRoot}/other file.pdf>)`
        + `\n\n| Item | Result |\n| --- | --- |\n| Filename | \`${bare}\` |\n\n- Capability \`builtin.development\`\n- Unresolved \`./missing-from-reply.js\``);
      reply.finishBody();
      state.workbench = metadata;
    }, {relative, bare, preview, localHost, otherRoot});
    const reply = page.locator('[data-reply-fixture]');
    const fileLink = reply.getByRole('button', {name:relative, exact:true});
    const urlLink = reply.getByRole('button', {name:preview, exact:true});
    const menu = page.getByRole('menu');
    const action = key => menu.locator(`[data-action="${key}"]`);
    const clipboard = () => page.evaluate(() => navigator.clipboard.readText());

    await expect(fileLink).toBeVisible(LIVE);
    for (const reference of [bare, 'builtin.development', './missing-from-reply.js']) {
      await expect(reply.getByText(reference, {exact:true})).toBeVisible();
      await expect(reply.getByRole('button', {name:reference, exact:true})).toHaveCount(0);
    }
    for (const link of [fileLink, urlLink]) {
      await link.click({ button: 'right' });
      await expect(menu).toBeHidden();
      // Chromium may select a word for its own context menu.
      await page.evaluate(() => window.getSelection()?.removeAllRanges());
      await expect(page.locator('.selection-quote')).toBeHidden();
      await link.press('Shift+F10');
      await expect(menu).toBeHidden();
    }

    await urlLink.press('Enter');
    await action('copy_url').click();
    await expect.poll(clipboard).toBe(preview);
    await page.context().route(preview, route => route.fulfill({body:'Preview service'}));
    await urlLink.click();
    const popupEvent = page.waitForEvent('popup');
    await action('open_in_browser').click();
    const popup = await popupEvent;
    await expect(popup).toHaveURL(preview);
    await popup.close();

    await page.evaluate(() => {
      globalThis.CLATDesktop = {openInBrowser: url => { globalThis.replyOpenedUrl = url; }};
    });
    await reply.getByRole('button', {name:localHost, exact:true}).click();
    await action('open_in_browser').click();
    await expect.poll(() => page.evaluate(() => globalThis.replyOpenedUrl)).toBe(localHost);
    await page.evaluate(() => { delete globalThis.CLATDesktop; });

    await reply.getByRole('button', {name:'Outside'}).click({button:'right'});
    await expect(action('open_file')).toBeDisabled();
    await expect(action('copy_path')).toBeDisabled();
    await action('copy_absolute_path').click();
    await expect.poll(clipboard).toBe(path.join(otherRoot, 'other file.pdf'));

    await fileLink.click();
    await action('copy_path').click();
    await expect.poll(clipboard).toBe(relative);
    await fileLink.click();
    await action('copy_absolute_path').click();
    await expect.poll(clipboard).toBe(absolute);
    await fileLink.click();
    await action('open_file').click();
    await expect(page.locator('#file-browser .cm-content')).toHaveText('File made by the reply', LIVE);
  } finally { fs.rmSync(path.dirname(absolute), {recursive:true, force:true}); }
});

test('right-clicking a reply link dismisses the quote and allows a new selection', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  await openWorkbench(page, hostInfo('workbench'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'quote-menu-fixture.md');
  fs.writeFileSync(file, 'Documentation');
  try {
    await page.evaluate(file => {
      handleReplay({
        type: 'assistant_message', seq: 991992,
        text: `[Providers](<${file}>)

A sentence worth quoting.`,
      });
    }, file);
    const reply = page.locator('.msg.assistant[data-seq="991992"]');
    const link = reply.getByRole('button', {name:'Providers'});
    const quote = page.locator('.selection-quote');
    const menu = page.getByRole('menu');
    const selectText = locator => locator.evaluate(node => {
      const range = document.createRange();
      range.selectNodeContents(node);
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
      document.dispatchEvent(new Event('selectionchange'));
    });

    await expect(link).toBeVisible(LIVE);
    await selectText(link);
    await expect(quote).toBeVisible();
    await link.click({button:'right'});
    await expect(menu.locator('[data-action="copy_path"]')).toBeVisible();
    // Reproduce WebKit delivering a word selection after the context menu opens.
    await selectText(link);
    await expect(quote).toBeHidden({timeout:1500});
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();
    await selectText(link);
    await expect(quote).toBeHidden({timeout:1500});

    const paragraph = reply.getByText('A sentence worth quoting.', {exact:true});
    await paragraph.dispatchEvent('pointerdown', {button:0, pointerType:'mouse'});
    await selectText(paragraph);
    await expect(quote).toBeVisible();
    await quote.click();
    await expect(page.locator('#prompt')).toHaveValue(/> A sentence worth quoting\./);
  } finally { fs.rmSync(file, {force:true}); }
});

test('application toolbar history returns between features without losing their work', async ({ page }) => {
  await openWorkbench(page, hostInfo('workbench'));
  await page.fill('#prompt', 'Retain through toolbar history');
  await page.click('#nav-history');
  await page.locator('#history-dialog input[type="search"]').fill('retained query');
  await page.click('#nav-settings');
  await page.click('#application-back');
  await expect(page.locator('#history-dialog input[type="search"]')).toBeVisible();
  await expect(page.locator('#history-dialog input[type="search"]')).toHaveValue('retained query');
  await page.click('#application-forward');
  await expect(page.locator('#settings-dialog')).toBeVisible();
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toHaveValue('Retain through toolbar history');
});

test('feature pages own their content without exposing conversation-only controls', async ({ page }) => {
  await openWorkbench(page, hostInfo('workbench'));
  await page.fill('#prompt', 'Preserve the conversation while using another feature');
  const before = await page.evaluate(() => state.selectionGeneration);
  for (const id of ['nav-history', 'nav-resources', 'nav-tasks', 'nav-extensions', 'nav-settings', 'nav-help']) {
    await page.click('#' + id);
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    await expect(page.locator('#' + id)).toBeFocused();
    await expect(page.locator('.project-group:has(#session-list) [data-action=new-project-session]')).toBeHidden();
    await expect(page.locator('#session-search')).toBeHidden();
    await expect(page.locator('#prompt')).toBeHidden();
  }
  // Keyboard navigation retains its position, and Tab can still enter the page.
  await page.locator('#nav-help').focus(); await page.keyboard.press('Enter');
  await expect(page.locator('#nav-help')).toBeFocused();
  await page.keyboard.press('Tab');
  expect(await page.evaluate(() => document.querySelector('#help-dialog').contains(document.activeElement))).toBe(true);
  await page.click('#nav-workbench');
  await expect(page.locator('.project-group:has(#session-list) [data-action=new-project-session]')).toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('Preserve the conversation while using another feature');
  expect(await page.evaluate(() => state.selectionGeneration)).toBe(before);
});

test('resources quote a checked project snapshot back into the retained conversation draft', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  await openWorkbench(page, hostInfo('workbench'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'resource-architecture-example.md');
  fs.writeFileSync(file, 'Original resource snapshot');
  try {
    await page.fill('#prompt', 'Keep this draft');
    await page.click('#nav-resources');
    const editor = page.locator('#resources-dialog #file-browser');
    const row = editor.getByRole('treeitem',{name:'resource-architecture-example.md'});
    await editor.getByRole('searchbox').fill('resource-architecture-example');
    await row.click();
    await expect(editor.locator('.cm-content')).toHaveText('Original resource snapshot', LIVE);
    await row.click({button:'right'});
    await page.locator('[role=menuitem][data-action=quote]').click();
    await expect(page.locator('#prompt')).toBeVisible();
    await expect(page.locator('#prompt')).toHaveValue(/Keep this draft[\s\S]*Original resource snapshot/);
    fs.writeFileSync(file, 'Later disk edit');
    await expect(page.locator('#prompt')).not.toHaveValue(/Later disk edit/);
  } finally { fs.rmSync(file, { force: true }); }
});

test('workbench navigation and side review preserve the unsent conversation draft', async ({page}, info) => {
  await page.emulateMedia({colorScheme:'dark',reducedMotion:'reduce'});
  await openWorkbench(page, hostInfo('workbench'));
  await page.fill('#prompt', 'Keep this conversation draft');
  await page.click('#project-list-toggle');
  await expect(page.locator('#nav-settings')).toBeVisible();
  await page.click('#nav-settings');
  await expect(page.locator('#settings-dialog')).toBeVisible();
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toBeVisible();
  await page.click('#nav-settings');
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toHaveValue('Keep this conversation draft');
  await page.click('#composer-add');
  await page.click('#add-project-file');
  await expect(page.locator('#file-browser')).toBeVisible();
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toHaveValue('Keep this conversation draft');
  await page.click('#nav-tasks');
  await expect(page.locator('#jobs-dialog')).toBeVisible();
  await page.selectOption('#task-type','schedules');
  await expect(page.locator('#jobs-dialog')).toBeVisible();
  await page.goBack();
  await expect(page.locator('#jobs-dialog')).toBeVisible();
  await page.goForward();
  await expect(page.locator('#jobs-dialog')).toBeVisible();
  await page.click('#nav-workbench');
  await expect(page.locator('#prompt')).toHaveValue('Keep this conversation draft');
});

test('plan mode in the add menu changes core policy without sending or clearing the draft', async ({page}, info) => {
  await page.emulateMedia({colorScheme:'dark'});
  await openWorkbench(page, hostInfo('workbench'));
  await page.fill('#prompt', 'Do not send this');
  const before = await page.evaluate(async () => (await rpc('workflow.details', {})).plan_mode);
  await page.click('#composer-add');
  await page.click('#add-plan');
  await expect.poll(() => page.evaluate(async () => (await rpc('workflow.details', {})).plan_mode), LIVE).toBe(!before);
  await expect(page.locator('#prompt')).toHaveValue('Do not send this');
  await page.getByRole('button', {name:'Workspace', exact:true}).click();
  await expect(page.locator('#context-plan')).toBeVisible();
  await page.click('#composer-add');

  await page.click('#add-plan');
  await expect.poll(() => page.evaluate(async () => (await rpc('workflow.details', {})).plan_mode), LIVE).toBe(before);
});

test('pending requests switch and collapse without losing answers or the regular draft', async ({page},info) => {
  await openWorkbench(page, hostInfo('workbench'));
  await page.fill('#prompt','Regular unsent draft');
  await page.evaluate(() => {
    for (const id of ['first','second']) onQuestionNotice({kind:'question_requested',payload:{rpc_id:id,
      question:{question:id,options:[],allow_custom:true}}});
  });
  const first = page.locator('.question-card[data-rpc-id="first"]');
  const second = page.locator('.question-card[data-rpc-id="second"]');
  await expect(first).toBeVisible();
  await first.locator('textarea').fill('Answer one');
  await page.click('#interaction-next');
  await expect(second).toBeVisible();
  await second.locator('textarea').fill('Answer two');
  await page.click('#interaction-collapse');
  await expect(second).toBeHidden();
  await page.click('#interaction-collapse');
  await expect(second.locator('textarea')).toHaveValue('Answer two');
  await page.click('#interaction-previous');
  await expect(first.locator('textarea')).toHaveValue('Answer one');
  await expect(page.locator('#prompt')).toHaveValue('Regular unsent draft');
  expect(await page.evaluate(() => window.dispatchEvent(new CustomEvent('clat-desktop-action-request',{cancelable:true,detail:{action:'open'}})))).toBe(false);

  await page.evaluate(() => onQuestionNotice({kind:'question_resolved',payload:{rpc_id:'first'}}));
  await expect(second).toBeVisible();
  await expect(page.locator('#transcript .question-card[data-rpc-id="first"]')).toHaveClass(/resolved/);
  await page.evaluate(() => onQuestionNotice({kind:'question_requested',payload:{rpc_id:'automatic',automatic_session_id:'scheduled-session',
    question:{question:'Scheduled request',options:[],allow_custom:true}}}));
  await page.click('#interaction-next');
  await page.click('#interaction-origin');
  await expect(page.locator('#jobs-dialog')).toBeVisible();
  await page.locator('.schedule-interactions .interaction-anchor').click();
  await expect(page.locator('.question-card[data-rpc-id="automatic"]')).toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('Regular unsent draft');
});

test('a late mode-change failure cannot overwrite the newly selected conversation', async ({page}) => {
  await openWorkbench(page, hostInfo('workbench'));
  const selection = await page.evaluate(() => state.selectionGeneration);
  let release, started;
  const held = new Promise(resolve => { release = resolve; });
  const requestStarted = new Promise(resolve => { started = resolve; });
  await page.route('**/api/workflow.action', async route => {
    started(); await held;
    await route.fulfill({status:503,body:'late mode failure'});
  });
  try {
    await page.click('#composer-add');
    await page.click('#add-plan');
    await requestStarted;
    await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
    await expect.poll(() => page.evaluate(() => state.selectionGeneration),LIVE).not.toBe(selection);
    await page.fill('#prompt','New conversation draft');
    await page.click('#composer-add');
    const response = page.waitForResponse(reply => reply.url().endsWith('/api/workflow.action'));
    release();
    await response;
    await expect(page.locator('#add-plan')).toBeEnabled(LIVE);
    await expect(page.locator('#run-state')).toBeEmpty();
    await expect(page.locator('#prompt')).toHaveValue('New conversation draft');
  } finally { release(); }
});

test('composer creates a Goal and controls the same durable objective after pausing and switching', async ({page}) => {
  await openWorkbench(page, hostInfo('long-stream'));
  const selection = await page.evaluate(() => state.selectionGeneration);
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect.poll(() => page.evaluate(() => state.selectionGeneration), LIVE).not.toBe(selection);
  const facts = () => page.evaluate(() => rpc('workflow.details', {}));
  // The timed test Host uses classic permissions. Its Plan mode requires
  // a materialized session, unlike the production workbench's fresh composer.
  await page.fill('#prompt', 'Seed a conversation for Plan mode');
  await page.click('#send');
  await expect.poll(async () => {
    const value = await facts(); return Boolean(value.session) && !value.busy;
  }, LIVE).toBe(true);
  const objective = '保留路径 src/example.rs\n核对第二行的结果';
  const bar = page.locator('#composer-goal-bar');
  const pause = page.locator('#composer-goal-pause');
  const start = page.locator('#composer-goal-start');
  const remove = page.locator('#composer-goal-remove');

  await page.click('#composer-add');
  await page.click('#add-plan');
  await expect.poll(async () => (await facts()).plan_mode, LIVE).toBe(true);
  await page.click('#composer-add');
  await page.click('#add-goal');
  expect((await facts()).plan_mode).toBe(true);
  expect((await facts()).goal.goal).toBeNull();
  await expect(page.locator('#composer-plan-toggle[data-mode="goal"]')).toBeVisible(LIVE);
  await page.fill('#prompt', objective);
  await page.locator('#prompt').press('Enter');
  await expect(bar).toContainText('src/example.rs', LIVE);
  const objectiveMessages = page.locator('.msg.user').filter({hasText:'核对第二行的结果'});
  await expect(objectiveMessages).toHaveCount(1, LIVE);
  await expect(objectiveMessages).not.toContainText('<goal_round>');
  await expect(page.locator('#prompt')).toHaveValue('', LIVE);
  await expect(pause).toBeEnabled(LIVE);
  const created = await facts();
  expect(created.plan_mode).toBe(false);
  expect(created.goal.goal.objective).toBe(objective);
  expect(created.goal.goal.limits.maxTokens).toBe(0);
  expect(created.goal.armed).toBe(true);

  await pause.click();
  await expect(start).toBeEnabled(LIVE);
  await expect.poll(async () => (await facts()).goal.goal.phase, LIVE).toBe('paused');
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect(bar).toBeHidden(LIVE);
  await page.locator(`[data-session-id="${created.session}"] .session-item`).click();
  await expect(bar).toContainText('src/example.rs', LIVE);
  await expect(start).toBeEnabled(LIVE);
  await start.click();
  await expect(pause).toBeEnabled(LIVE);
  await expect.poll(async () => (await facts()).goal.goal.roundsStarted, LIVE).toBe(2);
  expect((await facts()).goal.goal.id).toBe(created.goal.goal.id);
  await expect(objectiveMessages).toHaveCount(1, LIVE);
  await expect(page.locator('.msg.user')).toHaveCount(2, LIVE);

  const clearGoal = page.locator('dialog.clat-dialog');
  await remove.click();
  await expect(clearGoal).toBeVisible(LIVE);
  await clearGoal.locator('[data-role="cancel"]').click();
  expect((await facts()).goal.goal.id).toBe(created.goal.goal.id);
  await remove.click();
  await expect(clearGoal).toBeVisible(LIVE);
  await clearGoal.locator('[data-role="accept"]').click();
  await expect(bar).toBeHidden(LIVE);
  await expect.poll(async () => {
    const value = await facts(); return [value.goal.goal, value.busy];
  }, LIVE).toEqual([null, false]);
  await page.reload();
  await expect(page.locator('#conn-status')).toHaveText('live', LIVE);
  await expect(bar).toBeHidden();
  expect((await facts()).goal.goal).toBeNull();
  await expect(objectiveMessages).toHaveCount(1, LIVE);
});

test('goal result renders Markdown while retaining its text', async ({page}) => {
  await openWorkbench(page, hostInfo('success'));
  await page.evaluate(async () => {
    await rpc('command.run', {command:'/goal create Review the result'});
    const original = rpc;
    rpc = async (method, params) => {
      const value = await original(method, params);
      if (method === 'workflow.details' && value.goal?.goal) {
        value.goal.goal.lastResult = '## Summary\n\n**Done**\n\n| File | Result |\n| --- | --- |\n| src/main.rs | verified |';
      }
      return value;
    };
    document.getElementById('workflow-open').click();
  });
  const result = page.locator('#workflow-details .workflow-result');
  await expect(result.locator('strong')).toHaveText('Done', LIVE);
  await expect(result.locator('table')).toContainText('src/main.rs');
});

test('approving a plan links it to the existing paused goal instead of creating a new one', async ({page}) => {
  await openWorkbench(page, hostInfo('success'));
  await page.fill('#prompt', 'Open a conversation to replan');
  await page.click('#send');
  const facts = () => page.evaluate(() => rpc('workflow.details', {}));
  await expect.poll(async () => {
    const value = await facts(); return Boolean(value.session) && !value.busy;
  }, LIVE).toBe(true);

  const {goal, doc} = await page.evaluate(async () => {
    await rpc('command.run', {command:'/goal create Keep the original objective --tokens 3000000'});
    await rpc('command.run', {command:'/goal pause'});
    await rpc('command.run', {command:'/plan on'});
    const value = await rpc('workflow.details', {});
    const doc = await rpc('workflow.action', {
      operationId:crypto.randomUUID(), session:value.session, expectedVersion:value.version,
      expectedGeneration:value.generation,
      intent:{action:'save_plan',id:null,revision:0,text:'# Revised approach\n\nValidate the same objective.'},
    });
    return {goal:value.goal.goal, doc};
  });
  await page.click('#composer-plan-toggle');
  await page.click('#work-mode-details');
  await page.locator('#workflow-details [data-action="approve_for_goal"]').click();
  await expect.poll(async () => {
    const value = await facts();
    return value.documents.find(item => item.id === doc.id && item.revision === doc.revision)?.goal_id;
  }, LIVE).toBe(goal.id);
  const continued = await facts();
  expect(continued.goal.goal.id).toBe(goal.id);
  expect(continued.goal.goal.objective).toBe(goal.objective);
  expect(continued.goal.goal.limits).toEqual(goal.limits);
  expect(continued.plan_mode).toBe(false);
});

test('Goal composer saves an ordered model handoff queue with threshold and note', async ({page}) => {
  await openWorkbench(page, hostInfo('long-stream'));
  await page.evaluate(async () => {
    for (const name of ['Handoff B', 'Handoff C']) {
      await rpc('model.profile.save', {
        name, model:name, protocol:'open_ai_compatible', endpoint:'https://application-test.invalid',
        request_path:'/chat/completions',
      });
    }
  });
  const selection = await page.evaluate(() => state.selectionGeneration);
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect.poll(() => page.evaluate(() => state.selectionGeneration), LIVE).not.toBe(selection);
  await page.click('#composer-add');
  await page.click('#add-goal');
  await page.click('#composer-plan-toggle');
  await page.click('#work-mode-handoff');
  const dialog = page.locator('dialog[data-kind=edit]');
  const add = dialog.getByRole('button', {name:'Add to queue'});
  for (const name of ['Handoff B', 'Handoff C']) {
    await dialog.locator('select').selectOption(name);
    await add.click();
  }
  await dialog.locator('li').filter({hasText:'Handoff C'}).getByRole('button', {name:'Move up'}).click();
  await dialog.locator('input[type=number]').fill('15');
  await dialog.locator('textarea').fill('Continue with the targeted test.');
  await dialog.locator('[data-role=accept]').click();
  await page.fill('#prompt', 'Finish the handoff regression');
  await page.locator('#prompt').press('Enter');
  await expect.poll(
    () => page.evaluate(async () => (await rpc('workflow.details', {})).goal.goal?.handoff || null),
    LIVE
  ).toMatchObject({threshold:15, note:'Continue with the targeted test.',
    routes:[expect.any(Object), {profile:'Handoff C'}, {profile:'Handoff B'}]});
  await page.locator('#composer-goal-pause').click();
  await expect(page.locator('#composer-goal-start')).toBeEnabled(LIVE);
  await page.locator('#composer-goal-details').click();
  await page.locator('#workflow-details').getByRole('button', {name:'Model handoff', exact:true}).click();
  await expect(dialog.locator('li')).toHaveCount(2);
  const remove = dialog.getByRole('button', {name:'Remove from queue'});
  await remove.first().click();
  await remove.first().click();
  await dialog.locator('[data-role=accept]').click();
  await expect.poll(
    () => page.evaluate(async () => (await rpc('workflow.details', {})).goal.goal?.handoff || null),
    LIVE
  ).toBeNull();
});
