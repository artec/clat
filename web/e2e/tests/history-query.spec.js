const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench,LIVE}=require('../helpers/workbench');
test('history search and inspector use committed facts without changing the composer or selection',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.click('#new-session');await expect(page.locator('#send')).toBeEnabled(LIVE);
  await page.fill('#prompt','历史检索 regression-needle <img src=x onerror=alert(1)>');await page.click('#send');
  await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);await expect(page.locator('#cancel')).not.toBeVisible(LIVE);
  await page.evaluate(()=>refreshWorkbench());
  await expect.poll(()=>page.evaluate(()=>state.sessionId),LIVE).not.toBeNull();
  await page.fill('#prompt','未发送的中文草稿');
  const before=await page.evaluate(()=>state.sessionId);
  const mutations=[];page.on('request',r=>{if(/prompt.send|steer.send|session.switch|session.new|approval.respond/.test(r.url()))mutations.push(r.url());});
  await page.click('#history-open');
  const dialog=page.locator('#history-dialog');await expect(dialog).toBeVisible();
  await dialog.locator('input[type="search"]').fill('regression-needle');await dialog.locator('button.primary').click();
  await expect(dialog.locator('.history-hit')).toHaveCount(1,LIVE);
  await expect(dialog.locator('.history-hit img')).toHaveCount(0);
  await dialog.locator('.history-hit-open').click();
  await expect(dialog.locator('.history-event')).not.toHaveCount(0,LIVE);
  await dialog.locator('.history-event').filter({hasText:'user/message'}).click();
  await expect(dialog.locator('.history-json')).toContainText('regression-needle',LIVE);
  for(const [lang,title] of [['zh-Hans','会话历史'],['zh-Hant','工作階段歷史'],['en','Session history']]){
    await page.evaluate(lang=>CLATI18n.setLanguage(lang),lang);await expect(dialog.locator('h2')).toHaveText(title);
    await expect(page.locator('#prompt')).toHaveValue('未发送的中文草稿');
  }
  await dialog.getByRole('button',{name:'Close',exact:true}).click();
  expect(await page.evaluate(()=>state.sessionId)).toEqual(before);expect(mutations).toEqual([]);
});
