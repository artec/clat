const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench}=require('../helpers/workbench');

test('native Web settings save through Host without losing Chinese draft or exposing a stored credential',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.evaluate(()=>{dom.prompt.value='未发送的中文草稿';openSettings();});
  await page.locator('[data-settings-category="web"]').click();
  const panel=page.locator('[data-settings-panel="web"]');
  await expect(panel).toBeVisible();
  await panel.locator('input[type="password"]').fill('native-web-test-key-only');
  await panel.locator('button.primary').click();
  await expect(panel.locator('input[type="password"]')).toHaveValue('');
  const view=await page.evaluate(()=>rpc('web.settings.get',{}));
  expect(view.search_available).toBe(true);
  expect(JSON.stringify(view)).not.toContain('native-web-test-key-only');
  for(const [locale,title] of [['zh-Hans','联网'],['zh-Hant','網路'],['en','Web']]){
    await page.evaluate(async(locale)=>{await CLATI18n.setLanguage(locale);},locale);
    await expect(panel.locator('h3')).toHaveText(title);
    await expect(page.locator('#prompt')).toHaveValue('未发送的中文草稿');
  }
  await panel.locator('input[type="checkbox"]').last().check();
  await panel.locator('button.primary').click();
  await expect.poll(()=>page.evaluate(async()=> (await rpc('web.settings.get',{})).search_available)).toBe(false);
});
