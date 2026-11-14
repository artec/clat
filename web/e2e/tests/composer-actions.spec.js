const { test, expect } = require('@playwright/test');
const { LIVE, hostInfo, openWorkbench } = require('../helpers/workbench');


test('send and stop preserve the draft and invoke only their own action', async ({ page }) => {
  await openWorkbench(page, hostInfo('rewind'));
  const prompt = page.locator('#prompt');
  await prompt.fill('A stopped question');
  const send = page.locator('#send'), stop = page.locator('#cancel');
  await expect(stop).toBeHidden();
  await send.click();
  await expect(stop).toBeVisible();
  await expect(send).toBeHidden();
  for (const locale of ['en', 'zh-Hans', 'zh-Hant']) {
    const label = await page.evaluate(async locale => {
      await CLATI18n.setLanguage(locale); return CLATI18n.t('web.stop');
    }, locale);
    await expect(stop).toHaveAccessibleName(label);
    await expect(stop).toHaveAttribute('title', label);
  }

  await prompt.fill('保留草稿');
  await expect(send).toBeVisible();
  await expect(stop).toBeHidden();
  await prompt.clear();
  await expect(stop).toBeVisible();
  // Type a draft after issuing Stop but before the Host receives it.
  await page.route('**/api/run.cancel', async route => {
    await prompt.fill('保留草稿');
    await route.continue();
  }, { times: 1 });
  const cancelled = page.waitForRequest(request => request.url().endsWith('/api/run.cancel'));
  await stop.click(); await cancelled;
  await expect(stop).toBeHidden(LIVE);
  await expect(send).toBeVisible(LIVE);
  await expect(prompt).toHaveValue('保留草稿');
  await expect.poll(() => page.evaluate(() => state.runActive), LIVE).toBe(false);
});

test('a stopped turn offers a hover edit action and can be rerun', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 1200 });
  await openWorkbench(page, hostInfo('rewind'));
  await page.click('.project-group:has(#session-list) [data-action=new-project-session]');
  await expect(page.locator('#conn-status')).toHaveAttribute('data-state', 'live', LIVE);

  const stop = page.locator('#cancel');
  const user = (text) => page.locator('#transcript .msg.user').filter({ hasText: text });
  const pencil = (message) => message.getByRole('button', { name: 'Edit stopped message' });
  const partial = page.getByRole('article', { name: 'Agent reply' }).filter({ hasText: 'An unfinished answer' });
  async function send(text) {
    await page.fill('#prompt', text);
    await page.click('#send');
    await expect(user(text)).toBeVisible(LIVE);
  }

  await send('A previous completed question');
  await expect(stop).toBeHidden(LIVE);
  const prior = user('A previous completed question');
  await prior.hover();
  await expect(pencil(prior)).toBeHidden();
  const completedGoal = await page.evaluate(async () => {
    await rpc('command.run', { command: '/goal create Verify session history' });
    await rpc('command.run', { command: '/goal complete Verified' });
    return (await rpc('workflow.details', {})).goal;
  });
  expect(completedGoal.goal.phase).toBe('complete');
  expect(completedGoal.armed).toBe(false);

  await send('An original stopped question');
  const original = user('An original stopped question');
  await expect(partial).toBeVisible(LIVE);
  await original.hover();
  await expect(pencil(original)).toBeHidden();
  const id = await page.evaluate(() => state.sessionId);
  // Stop without moving the pointer away from the active bubble.
  await expect(stop).toBeVisible(LIVE);
  await stop.evaluate(button => button.focus({ preventScroll: true }));
  await page.keyboard.press('Enter');
  await expect(stop).toBeHidden(LIVE);
  await expect(pencil(original)).toBeVisible(LIVE);
  await page.locator('#prompt').hover();
  await expect(pencil(original)).toBeHidden();
  // A slow Host recheck must not hide a previously authorized hover action.
  let releaseCheck;
  const checkGate = new Promise(resolve => { releaseCheck = resolve; });
  const slowCheck = async route => {
    await checkGate;
    await route.continue();
  };
  await page.route('**/api/session.rewind.check', slowCheck);
  try {
    await original.hover();
    await expect(pencil(original)).toBeVisible({ timeout: 2000 });
  } finally {
    releaseCheck();
    await page.unrouteAll({ behavior: 'wait' });
  }

  await page.reload();
  await expect(page.locator('#conn-status')).toHaveAttribute('data-state', 'live', LIVE);
  await page.locator('#prompt').hover();
  await expect(pencil(original)).toBeHidden();
  await original.hover();
  await expect(pencil(original)).toBeVisible(LIVE);
  await pencil(original).click();
  const editor = original.getByRole('textbox');
  await expect(editor).toHaveValue('An original stopped question');
  await editor.fill('The revised question');
  await original.getByRole('button', { name: 'Send and rerun', exact: true }).click();
  await expect(original).toHaveCount(0, LIVE);
  await expect(partial).toHaveCount(0, LIVE);
  await expect(user('The revised question')).toHaveCount(1, LIVE);
  await expect(prior).toHaveCount(1);
  await expect.poll(() => page.evaluate(async () => {
    const session = await rpc('session.info', {});
    const history = await rpc('session.history', {});
    return {
      id: session.session_id,
      messages: history.events.filter(event => event.type === 'user_message').map(event => event.text),
      aborted: history.events.some(event => event.type === 'turn_ended' && event.reason?.aborted === 'user'),
    };
  }), LIVE).toEqual({ id, messages: ['A previous completed question', 'The revised question'], aborted: false });
  expect(await page.evaluate(async () => (await rpc('workflow.details', {})).goal)).toEqual(completedGoal);

  await expect(stop).toBeHidden(LIVE);
  await page.locator('#composer-plan-toggle').click();
  const newGoal = page.locator('#work-mode-menu [data-work-mode="goal"]');
  await expect(newGoal).toBeEnabled(LIVE);
  await newGoal.click();
  await page.fill('#prompt', 'Verify new work in the same conversation');
  await page.click('#send');
  await expect.poll(() => page.evaluate(async previousGoalId => {
    const workflow = await rpc('workflow.details', {});
    const current = workflow.goal?.goal;
    return { session: workflow.session, objective: current?.objective,
      freshGoal: Boolean(current?.id) && current.id !== previousGoalId, rounds: current?.roundsStarted };
  }, completedGoal.goal.id), LIVE).toEqual({ session: id, objective: 'Verify new work in the same conversation', freshGoal: true, rounds: 1 });
});
