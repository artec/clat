const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');

test('the global extension retains an open file and can be controlled from every feature page', async ({ page }) => {
  const fs = require('node:fs'), path = require('node:path');
  await openWorkbench(page, hostInfo('workbench'));
  const root = await page.evaluate(async () => (await rpc('workbench.info', {})).project.root);
  const file = path.join(root, 'global-extension-example.md');
  fs.writeFileSync(file, 'Retained global file review');
  try {
    await page.fill('#prompt', 'Unsent conversation draft');
    await page.click('#composer-add'); await page.click('#add-project-file');
    await page.locator('#file-browser input[type="search"]').fill('global-extension-example');
    await page.locator('#file-browser .review-file').click();
    const preview = page.locator('#file-browser pre');
    await expect(preview).toHaveText('Retained global file review', LIVE);
    for (const id of ['nav-resources', 'nav-tasks', 'nav-history', 'nav-extensions', 'nav-settings', 'nav-help']) {
      await page.click('#tools-open');
      await expect(page.locator('#workbench-tools')).toBeVisible();
      await page.click('#' + id);
      await expect(page.locator('#workbench-tools')).toBeHidden();
      await expect(preview).toBeVisible();
      await page.click('#inspector-toggle'); await expect(preview).toBeHidden();
      await page.click('#inspector-toggle');
      await expect(preview).toBeVisible();
      await expect(preview).toHaveText('Retained global file review');
    }
    await page.click('#nav-tasks'); await page.selectOption('#task-type','schedules');
    await expect(preview).toBeVisible();
    await page.locator('#file-browser [data-action="quote-file-snapshot"]').click();
    await expect(page.locator('#prompt')).toBeVisible();
    await expect(page.locator('#prompt')).toHaveValue(/Unsent conversation draft[\s\S]*Retained global file review/);
    await page.click('#nav-workbench');
    await expect(page.locator('#prompt')).toHaveValue(/Unsent conversation draft/);
  } finally { fs.rmSync(file, { force: true }); }
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
    await page.locator('#resource-files input[type="search"]').fill('resource-architecture-example');
    await page.locator('#resource-files .review-file').click();
    await expect(page.locator('#resource-files pre')).toHaveText('Original resource snapshot', LIVE);
    await page.locator('#resource-files [data-action="quote-file-snapshot"]').click();
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
  await expect(page.locator('#prompt')).toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('Keep this conversation draft');
  await page.click('#inspector-close');
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
  await page.click('#inspector-toggle');
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
  await page.route('**/api/command.run', async route => {
    started(); await held;
    await route.fulfill({status:503,body:'late mode failure'});
  });
  try {
    await page.click('#composer-plan-toggle'); await requestStarted;
    await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
    await expect.poll(() => page.evaluate(() => state.selectionGeneration),LIVE).not.toBe(selection);
    await page.fill('#prompt','New conversation draft');
    release();
    await expect.poll(() => page.evaluate(() => Boolean(workbenchShell.planPending)),LIVE).toBe(false);
    await expect(page.locator('#run-state')).toBeEmpty();
    await expect(page.locator('#prompt')).toHaveValue('New conversation draft');
  } finally { release(); }
});
