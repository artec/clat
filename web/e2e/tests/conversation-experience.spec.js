const {test,expect}=require('@playwright/test');
const path=require('path');
const {hostInfo,openWorkbench,LIVE}=require('../helpers/workbench');
test('round followups preserve the draft and cannot generate twice for a completed run',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await expect(page.locator('#prompt')).toBeEnabled(LIVE);
  await page.locator('#prompt').fill('Explain this example');await page.locator('#send').click();
  await expect(page.locator('.followup-choice').first()).toBeVisible(LIVE);
  const reference=await page.evaluate(async()=>({seq:(await rpc('workbench.info',{})).session.committed_seq,
    followups:await rpc('prompt.followups',{action:'read',run_id:'',expected_selection_generation:state.selectionGeneration})}));
  expect(reference.followups.items.length).toBeLessThanOrEqual(3);
  await page.locator('#prompt').fill('Keep my draft');
  await page.setInputFiles('#attachment-input',path.resolve(__dirname,'../../icons/icon-192.png'));
  const images=await page.evaluate(()=>state.draft.images.map(image=>image.id));
  expect(images.length).toBeGreaterThan(0);
  const chosen=await page.locator('.followup-choice').first().textContent();await page.locator('.followup-choice').first().click();
  await expect(page.locator('#prompt')).toHaveValue('Keep my draft\n\n'+chosen);
  expect(await page.evaluate(()=>state.draft.images.map(image=>image.id))).toEqual(images);
  const after=await page.evaluate(async(run)=>({seq:(await rpc('workbench.info',{})).session.committed_seq,
    followups:await rpc('prompt.followups',{action:'generate',run_id:run,expected_selection_generation:state.selectionGeneration})}),reference.followups.run_id);
  expect(after.seq).toBe(reference.seq);expect(after.followups.items).toEqual(reference.followups.items);
  const times=await page.evaluate(()=>[...dom.transcript.querySelectorAll('.msg[data-seq]')].map(node=>({seq:Number(node.dataset.seq),time:Number(node.dataset.time),
    expected:state.history.outline.find(item=>String(item.seq)===node.dataset.seq)?.time_ms})));
  expect(times.length).toBeGreaterThan(0);for(const time of times){expect(time.time).toBe(time.expected);expect(time.time).toBeGreaterThan(0);}
});
test('usage reads do not change the selected conversation or its draft and side modules retain that draft',async({page})=>{
  await openWorkbench(page,hostInfo('workbench'));await page.locator('#prompt').fill('draft for review');
  const before=await page.evaluate(async()=>{const info=await rpc('workbench.info',{});return info.session;});
  await page.locator('#context-usage').click();await expect(page.locator('.usage-popover')).toBeVisible();
  const stats=await page.evaluate(()=>rpc('conversation.stats',{}));
  expect(stats.context.used).toBeGreaterThanOrEqual(0);expect(stats.allowance).toHaveProperty('status');
  await page.keyboard.press('Escape');
  await expect(page.locator('.usage-popover')).toBeHidden();
  await page.locator('#inspector-toggle').click();
  await page.locator('#context-files-open').click();await expect(page.locator('#file-browser')).toBeVisible(LIVE);
  await page.locator('#context-overview-open').click();await expect(page.locator('#context-files-open')).toBeVisible();
  await expect(page.locator('#prompt')).toHaveValue('draft for review');
  const after=await page.evaluate(async()=>{const info=await rpc('workbench.info',{});return info.session;});
  expect(after.id).toBe(before.id);expect(after.committed_seq).toBe(before.committed_seq);
});
