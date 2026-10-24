const { test, expect } = require('@playwright/test');
const { hostInfo, openWorkbench, LIVE } = require('../helpers/workbench');
test('schedule draft requires review, runs in original session and preserves another draft across languages', async ({ page }, testInfo) => {
  await openWorkbench(page, hostInfo('success')); await page.evaluate(() => CLATI18n.setLanguage('en'));
  await page.fill('#prompt', 'schedule origin seed'); await page.click('#send'); await expect(page.locator('#cancel')).not.toBeVisible(LIVE);
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).toEqual(expect.any(String));
  const origin = await page.evaluate(() => state.sessionId);
  await page.fill('#prompt', '未发送的中文草稿'); await page.click('#schedules-open');
  const dialog = page.locator('#schedules-dialog'); await dialog.locator('summary').filter({ hasText: /^Create draft$/ }).click();
  await dialog.locator('[name=instruction]').fill('scheduled-e2e-original-instruction');
  await dialog.locator('[name=delay]').fill('0');
  await dialog.getByRole('button', { name: 'Create draft', exact: true }).click();
  await expect(dialog.locator('.schedule-review')).toBeVisible(LIVE);
  const draft = await page.evaluate(async () => (await rpc('schedule.list', {})).rules.find(r => r.spec.instruction === 'scheduled-e2e-original-instruction'));
  expect(draft.phase).toBe('draft'); expect((await page.evaluate(async () => (await rpc('schedule.deliveries', {})).deliveries)).length).toBe(0);
  for (const [locale, title] of [['zh-Hans', '定时与续跑'], ['zh-Hant', '定時與續跑'], ['en', 'Scheduled work']]) {
    await page.evaluate(locale => CLATI18n.setLanguage(locale), locale); await expect(dialog.locator('h2')).toHaveText(title);
    await expect(page.locator('#prompt')).toHaveValue('未发送的中文草稿');
    await page.screenshot({path:testInfo.outputPath(`schedule-review-${locale}.png`)});
  }
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();
  // A separate selected session must never receive this execution's transcript.
  await page.evaluate(async () => { await rpc('session.new', {}); resubscribe(); });
  await expect.poll(() => page.evaluate(() => state.sessionId), LIVE).not.toBe(origin);
  await page.fill('#prompt', '另一个会话的草稿');
  const png=await page.evaluate(()=>{const c=document.createElement("canvas");c.width=2;c.height=2;c.getContext("2d").fillRect(0,0,2,2);return c.toDataURL("image/png").split(",")[1];});
  await page.locator("#attachment-input").setInputFiles({name:"draft.png",mimeType:"image/png",buffer:Buffer.from(png,"base64")});
  await expect.poll(()=>page.evaluate(()=>state.draft.images[0]?.status),LIVE).toBe('uploaded');
  await page.click('#schedules-open');
  await dialog.locator('.schedule-review').getByRole('button', { name: 'Authorize this exact task and budget', exact: true }).click();
  await expect.poll(async () => page.evaluate(async id => (await rpc('schedule.deliveries', {})).deliveries.find(d => d.rule_id === id)?.phase, draft.id), LIVE).toBe('completed');
  const delivery = await page.evaluate(async id => (await rpc('schedule.deliveries', {})).deliveries.find(d => d.rule_id === id), draft.id);
  expect(delivery.scope.session).toBe(origin); expect(delivery.receipt.state).toBe('committed'); expect(delivery.actual.tokens).toBeGreaterThan(0);
  await expect(page.locator('#prompt')).toHaveValue('另一个会话的草稿');
  expect(await page.evaluate(()=>state.draft.images.map(i=>({name:i.name,status:i.status})))).toEqual([{name:'draft.png',status:'uploaded'}]);
  await expect(page.locator('#transcript')).not.toContainText('scheduled-e2e-original-instruction');
  await page.reload(); await expect(page.locator('#conn-status')).toHaveAttribute('data-state', 'live', LIVE);
  const deliveries = await page.evaluate(async id => (await rpc('schedule.deliveries', {})).deliveries.filter(d => d.rule_id === id), draft.id);
  expect(deliveries).toHaveLength(1);
});
