const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench,LIVE}=require('../helpers/workbench');
test('native background job survives a new conversation and frontend reconnect; independent output and stop',async({page})=>{
  await openWorkbench(page,hostInfo('native-jobs'));
  await page.evaluate(()=>CLATI18n.setLanguage('en'));
  await page.fill('#prompt','start a background job'); await page.click('#send');
  const approval=page.locator('.approval-card').last();await expect(approval).toBeVisible(LIVE);
  await approval.getByRole('button',{name:'Allow',exact:true}).click();
  await expect(page.locator('#cancel')).not.toBeVisible(LIVE);
  await expect.poll(()=>page.evaluate(async()=> (await rpc('jobs.list',{})).jobs.length),LIVE).toBe(1);
  const job=await page.evaluate(async()=> (await rpc('jobs.list',{})).jobs[0]);
  await expect.poll(()=>page.evaluate(async id=>(await rpc('jobs.output',{job_id:id,offset:0})).text,job.id),LIVE).toContain('background-e2e-output');
  await page.click('#new-session'); await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt','后台执行期间的草稿');
  await page.click('#jobs-open'); const dialog=page.locator('#jobs-dialog');
  await expect(dialog.locator('.history-hit')).toHaveCount(1,LIVE);
  await dialog.getByRole('button',{name:'Read output',exact:true}).click();
  await expect(dialog.locator(':scope > pre')).toContainText('background-e2e-output',LIVE);
  for(const [locale,title] of [['zh-Hans','后台任务'],['zh-Hant','背景工作'],['en','Background jobs']]){
    await page.evaluate(locale=>CLATI18n.setLanguage(locale),locale);
    await expect(dialog.locator('h2')).toHaveText(title);
    await expect(page.locator('#prompt')).toHaveValue('后台执行期间的草稿');
  }
  await dialog.getByRole('button',{name:'Close',exact:true}).click();
  await page.reload(); await expect(page.locator('#conn-status')).toHaveAttribute('data-state','live',LIVE);
  expect(await page.evaluate(async id=>(await rpc('jobs.list',{})).jobs.find(j=>j.id===id).finished_at,job.id)).toBeNull();
  await page.click('#jobs-open');page.once('dialog',d=>d.accept());
  await dialog.getByRole('button',{name:'Stop job',exact:true}).click();
  await expect.poll(()=>page.evaluate(async id=>(await rpc('jobs.list',{})).jobs.find(j=>j.id===id).state,job.id),LIVE).toBe('killed');
  expect(await page.evaluate(async id=>(await rpc('jobs.output',{job_id:id,offset:0})).job.session,job.id)).toEqual(job.session);
});
