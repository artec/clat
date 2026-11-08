const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench}=require('../helpers/workbench');
const methods=['plugin.builtin.get','plugin.builtin.preview','plugin.builtin.set'];
async function save(page,disabled){
 return page.evaluate(async disabled=>{const b=await rpc('plugin.builtin.get',{}),edit={instance_id:b.instance_id,revision:b.revision,disabled};const p=await rpc('plugin.builtin.preview',edit);return rpc('plugin.builtin.set',{...edit,preview_id:p.preview_id});},disabled);
}
async function editor(page){
 await page.locator('#nav-extensions').click();await page.locator('[data-action=builtin-config]').click();
 const panel=page.locator('.builtin-config-editor');await expect(panel.locator('input[data-plugin-id="builtin.web"]')).toBeAttached();
 await panel.locator('.builtin-config-list > summary').click();return panel;
}
test('built-in batch configuration preserves running state and retains conflicting drafts',async({page})=>{
 await openWorkbench(page,hostInfo('success'));
 const baseline=await page.evaluate(()=>rpc('plugin.builtin.get',{}));const panel=await editor(page);
 try{
  await expect(panel.locator('input[data-plugin-id="builtin.sandbox.policy"]')).toBeDisabled();
  const desired=!baseline.disabled.includes('builtin.web');
  await panel.locator('input[data-plugin-id="builtin.web"]').setChecked(!desired);
  await expect(panel.locator('[data-action=builtin-save]')).toBeDisabled();
  await panel.locator('[data-action=builtin-preview]').click();await expect(panel.locator('[data-action=builtin-save]')).toBeEnabled();
  await panel.locator('[data-action=builtin-save]').click();
  await expect.poll(()=>page.evaluate(async()=> (await rpc('plugin.builtin.get',{})).disabled.includes('builtin.web'))).toBe(desired);
  const saved=await page.evaluate(()=>rpc('plugin.builtin.get',{}));expect(saved.current).toEqual(baseline.current);
  await panel.locator('.builtin-config-list > summary').click();await panel.locator('input[data-plugin-id="builtin.web"]').setChecked(desired);
  await save(page,[...new Set([...saved.disabled,'builtin.skills'])]);
  await panel.locator('[data-action=builtin-preview]').click();await expect(panel.locator('[data-action=builtin-recheck]')).toBeVisible();
  await expect(panel.locator('input[data-plugin-id="builtin.web"]')).toBeChecked({checked:desired});
  await panel.locator('[data-action=builtin-recheck]').click();await expect(panel.locator('[data-action=builtin-preview]')).toBeEnabled();
  await panel.locator('[data-action=builtin-preview]').click();await expect(panel.locator('[data-action=builtin-save]')).toBeEnabled();
  await page.locator('#nav-settings').click();await page.locator('.builtin-leave-dialog [data-action=builtin-back]').click();await expect(panel).toBeVisible();
  await page.locator('#nav-settings').click();await page.locator('.builtin-leave-dialog [data-action=builtin-keep]').click();
  await page.locator('#nav-extensions').click();await expect(panel.locator('[data-action=builtin-save]')).toBeDisabled();
  await expect(panel.locator('input[data-plugin-id="builtin.web"]')).toBeChecked({checked:desired});
  await panel.locator('[data-action=builtin-discard]').click();
 }finally{await save(page,baseline.disabled);}
});
test('older Hosts retain read-only inventory without attempting configuration writes',async({page})=>{
 const writes=[];
 await page.route('**/api/host.describe',async route=>{
  const {fetchBrowserRequest}=require('../helpers/workbench');const response=await fetchBrowserRequest(route),body=await response.json();
  body.value.methods=body.value.methods.filter(method=>!methods.includes(method));await route.fulfill({response,json:body});
 });
 await page.route('**/api/plugin.builtin.*',route=>{writes.push(route.request().url());return route.abort();});
 await openWorkbench(page,hostInfo('success'));await page.locator('#nav-extensions').click();
 await page.locator('#application-toolbar [data-action=builtin-config]').click();
 await expect(page.locator('.builtin-config-editor')).toBeVisible();
 await expect(page.locator('#market-dialog .navigation-list [data-plugin-id="builtin.skills"]')).toBeVisible();
 await expect(page.locator('[data-action=builtin-save]')).toHaveCount(0);expect(writes).toEqual([]);
});

test('narrow-screen configuration remains editable across live language changes',async({page})=>{
 await page.setViewportSize({width:390,height:844});await openWorkbench(page,hostInfo('success'));
 const panel=await editor(page),check=panel.locator('input[data-plugin-id="builtin.web"]');
 const selected=await check.isChecked();await check.setChecked(!selected);
 for(const language of ['zh-Hans','zh-Hant','en']){
  await page.evaluate(language=>CLATI18n.setLanguage(language),language);
  await expect(check).toBeVisible();await expect(check).toBeChecked({checked:!selected});
  await expect(panel.locator('[data-action=builtin-save]')).toBeDisabled();
 }
 await panel.locator('[data-action=builtin-discard]').click();
});
test('lost save acknowledgement retains draft and requires readback without retry',async({page})=>{
 await openWorkbench(page,hostInfo('success'));const baseline=await page.evaluate(()=>rpc('plugin.builtin.get',{})),panel=await editor(page);
 const check=panel.locator('input[data-plugin-id="builtin.web"]'),checked=await check.isChecked();let commits=0;
 const intercept=async route=>{commits++;const {fetchBrowserRequest}=require('../helpers/workbench');await fetchBrowserRequest(route);await route.abort();};
 await page.route('**/api/plugin.builtin.set',intercept);
 try{
  await check.setChecked(!checked);await panel.locator('[data-action=builtin-preview]').click();await expect(panel.locator('[data-action=builtin-save]')).toBeEnabled();await panel.locator('[data-action=builtin-save]').click();
  await expect(panel.locator('[data-action=builtin-recheck]')).toBeVisible();await expect(check).toBeChecked({checked:!checked});await expect(panel.locator('[data-action=builtin-save]')).toBeDisabled();
  expect(commits).toBe(1);const saved=await page.evaluate(()=>rpc('plugin.builtin.get',{}));expect(saved.disabled.includes('builtin.web')).toBe(checked);expect(saved.current).toEqual(baseline.current);
 }finally{await page.unroute('**/api/plugin.builtin.set',intercept);await save(page,baseline.disabled);}
});
test('startup configuration is a separate destination and plugin browsing retains its draft',async({page})=>{
 await openWorkbench(page,hostInfo('success'));await page.locator('#nav-extensions').click();
 const panel=page.locator('.builtin-config-editor');
 const plugin=page.locator('#market-dialog .navigation-list [data-plugin-id="builtin.skills"]');
 await plugin.click();await expect(page.locator('.market-item[data-plugin-id="builtin.skills"]')).toBeVisible();
 await expect(panel).toBeHidden();
 await page.locator('[data-action=builtin-config]').click();await expect(panel).toBeVisible();
 await panel.locator('.builtin-config-list > summary').click();
 const check=panel.locator('input[data-plugin-id="builtin.web"]'),selected=await check.isChecked();await check.setChecked(!selected);
 await plugin.click();await expect(page.locator('.builtin-leave-dialog')).toBeVisible();
 await page.locator('.builtin-leave-dialog [data-action=builtin-back]').click();await expect(panel).toBeVisible();await expect(check).toBeChecked({checked:!selected});
 await plugin.click();await page.locator('.builtin-leave-dialog [data-action=builtin-keep]').click();
 await expect(panel).toBeHidden();await expect(page.locator('.market-item[data-plugin-id="builtin.skills"]')).toBeVisible();
 await page.locator('[data-action=builtin-config]').click();await expect(check).toBeChecked({checked:!selected});
 await panel.locator('[data-action=builtin-discard]').click();
});
