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

  page.once('dialog', d => d.dismiss());
  await remove.click();
  expect((await facts()).goal.goal.id).toBe(created.goal.goal.id);
  page.once('dialog', d => d.accept());
  await remove.click();
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
