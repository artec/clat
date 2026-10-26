const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench,LIVE,createWorkbenchConversation}=require('../helpers/workbench');
test('task editing requires authorization and runs in its fixed conversation while another draft survives',async({page})=>{
 await openWorkbench(page,hostInfo('success'));await page.evaluate(()=>CLATI18n.setLanguage('en'));
 const origin=await createWorkbenchConversation(page,'schedule origin seed');
 await page.fill('#prompt','未发送的中文草稿');await page.click('#nav-tasks');
 await page.locator('#jobs-dialog [data-action=create_task]').first().click();const editor=page.locator('#schedule-editor');
 await editor.locator('[name=title]').fill('Scheduled behavior fixture');await editor.locator('[name=instruction]').fill('scheduled-e2e-original-instruction');await editor.locator('[name=delay]').fill('0');
 await editor.locator('[data-action=save]').click();const review=page.locator('.schedule-review');await expect(review).toBeVisible(LIVE);
 const draft=await page.evaluate(async()=> (await rpc('schedule.list',{})).rules.find(r=>r.spec.instruction==='scheduled-e2e-original-instruction'));
 expect(draft.phase).toBe('draft');expect((await page.evaluate(async()=> (await rpc('schedule.deliveries',{})).deliveries)).length).toBe(0);
 await review.locator('[data-action=cancel]').click();await page.click('#nav-workbench');await expect(page.locator('#prompt')).toHaveValue('未发送的中文草稿');
 await page.evaluate(async()=>{await rpc('session.new',{});resubscribe();});await expect.poll(()=>page.evaluate(()=>state.sessionId),LIVE).not.toBe(origin);
 await page.fill('#prompt','另一个会话的草稿');
 const png=await page.evaluate(()=>{const c=document.createElement('canvas');c.width=2;c.height=2;c.getContext('2d').fillRect(0,0,2,2);return c.toDataURL('image/png').split(',')[1];});
 await page.locator('#attachment-input').setInputFiles({name:'draft.png',mimeType:'image/png',buffer:Buffer.from(png,'base64')});
 await expect.poll(()=>page.evaluate(()=>state.draft.images[0]?.status),LIVE).toBe('uploaded');
 await page.click('#nav-tasks');await page.locator('[data-task-id="'+draft.id+'"]').click();
 // Merely reading a task must not select its conversation or touch another draft.
 expect(await page.evaluate(()=>state.sessionId)).not.toBe(origin);
 await page.locator('#jobs-dialog [data-action=review]').click();await expect(review).toBeVisible(LIVE);await review.locator('[data-action=confirm]').click();
 await expect.poll(()=>page.evaluate(async id=>(await rpc('schedule.deliveries',{})).deliveries.find(d=>d.rule_id===id)?.phase,draft.id),LIVE).toBe('completed');
 const delivery=await page.evaluate(async id=>(await rpc('schedule.deliveries',{})).deliveries.find(d=>d.rule_id===id),draft.id);
 expect(delivery.scope.session).toBe(origin);expect(delivery.receipt.state).toBe('committed');expect(delivery.actual.tokens).toBeGreaterThan(0);
 await page.click('#nav-workbench');await expect(page.locator('#prompt')).toHaveValue('另一个会话的草稿');expect(await page.evaluate(()=>state.draft.images[0]?.status)).toBe('uploaded');
 await expect(page.locator('#transcript')).not.toContainText('scheduled-e2e-original-instruction');
 await page.reload();await expect(page.locator('#conn-status')).toHaveAttribute('data-state','live',LIVE);
 expect(await page.evaluate(async id=>(await rpc('schedule.deliveries',{})).deliveries.filter(d=>d.rule_id===id).length,draft.id)).toBe(1);
});
