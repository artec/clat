const {test,expect}=require('@playwright/test');
const fs=require('node:fs'),path=require('node:path');
const {hostInfo,openWorkbench,LIVE,createWorkbenchConversation,addWorkbenchProject}=require('../helpers/workbench');

test('a first-message file snapshot is linked to its committed message and bookmarks survive reopening',async({page})=>{
 await openWorkbench(page,hostInfo('workbench'));await page.evaluate(()=>CLATI18n.setLanguage('en'));
 await page.click('.project-group:has(#session-list) [data-action=new-project-session]');await expect.poll(()=>page.evaluate(()=>state.sessionId),LIVE).toBeNull();await expect(page.locator('#send')).toBeEnabled(LIVE);
 await expect.poll(()=>page.evaluate(()=>!!state.workbench),LIVE).toBe(true);const root=await page.evaluate(()=>state.workbench.project.root),file=path.join(root,'navigation-reference.md');fs.writeFileSync(file,'Exact navigation snapshot\n');
 try{
  await page.click('#nav-resources');await page.locator('#resource-files input[type=search]').fill('navigation-reference');await page.locator('#resource-files .review-file').click();
  await expect(page.locator('#resource-files pre')).toContainText('Exact navigation snapshot',LIVE);await page.locator('#resource-files [data-action=quote-file-snapshot]').click();
  await page.click('#send');await expect(page.locator('#cancel')).toBeHidden(LIVE);await expect.poll(()=>page.evaluate(()=>state.sessionId),LIVE).not.toBeNull();
  await expect.poll(()=>page.evaluate(async()=>(await rpc('library.list',{})).entries.filter(e=>e.kind==='snapshot').length),LIVE).toBe(1);
  const metadata=await page.evaluate(async()=>(await rpc('library.list',{})).entries.find(e=>e.kind==='snapshot'));expect(metadata.path).toBe('navigation-reference.md');expect(metadata.quote).toBeUndefined();expect(metadata.excerpt).toBeUndefined();
  const storage=await page.evaluate(async()=>(await rpc('host.describe',{})).storage_root);const privateMetadata=fs.readFileSync(path.join(storage,'library-v1.json'),'utf8');expect(privateMetadata).not.toContain('Exact navigation snapshot');
  const button=page.locator('.msg.user [data-action=bookmark]').last();await button.click();await expect(button).toHaveAttribute('aria-pressed','true');
  await page.reload();await expect(page.locator('#conn-status')).toHaveAttribute('data-state','live',LIVE);
  const entries=await page.evaluate(async()=>(await rpc('library.list',{})).entries);expect(entries.some(e=>e.kind==='bookmark')).toBe(true);
  await page.click('#nav-resources');await page.locator('#resources-dialog [data-tab=bookmarks]').click();await expect(page.locator('#resources-dialog .page-object').first()).toBeVisible(LIVE);
 }finally{fs.rmSync(file,{force:true});}
});

test('a task retains its conversation and draft across feature and project navigation',async({page})=>{
 await openWorkbench(page,hostInfo('workbench'));const session=await createWorkbenchConversation(page,'Task conversation navigation');
 const rule=await page.evaluate(async session=>(await rpc('schedule.create',{session,title:'Navigation task',instruction:'Inspect this project without modifying files.',trigger:{kind:'time',rule:{kind:'at',at_ms:Date.now()+3600000}},budget:{max_runs:1,max_tokens:100000,max_seconds:300,run_tokens:100000,run_seconds:300,max_paid_web_calls:0,run_paid_web_calls:0},model_profile:null})).rule,session);
 await page.click('#nav-tasks');await page.locator('[data-task-id="'+rule.id+'"]').click();await expect(page.locator('#prompt')).toBeVisible(LIVE);await page.fill('#prompt','Retained task draft');
 await page.click('#nav-history');await page.keyboard.press('Escape');await expect(page.locator('#history-dialog')).toBeVisible();
 await page.click('#nav-tasks');await expect(page.locator('#prompt')).toBeVisible(LIVE);await expect(page.locator('#prompt')).toHaveValue('Retained task draft');
 const origin=page.url();const root=fs.realpathSync(fs.mkdtempSync(path.join(require('node:os').tmpdir(),'clat-navigation-project-')));
 try{await addWorkbenchProject(page,root);await page.click('#nav-tasks');await page.selectOption('#task-project',new URL(origin).searchParams.get('workspace')||'default');await expect(page.locator('#conn-status')).toHaveAttribute('data-state','live',LIVE);await page.click('#nav-tasks');await page.locator('[data-task-id="'+rule.id+'"]').click();await expect(page.locator('#prompt')).toBeVisible(LIVE);await expect(page.locator('#prompt')).toHaveValue('Retained task draft');}
 finally{fs.rmSync(root,{recursive:true,force:true});}
});

test('late setting saves retain newer input and leaving clears a delayed pairing secret',async({page})=>{
 await openWorkbench(page,hostInfo('workbench'));await page.click('#nav-settings');await page.click('[data-settings-category=utility]');
 await page.evaluate(()=>{const original=rpc;globalThis.releaseUtility=null;rpc=async(method,params)=>method==='model.utility.set'?new Promise(resolve=>{globalThis.releaseUtility=()=>resolve({});}):original(method,params);});
 const naming=page.locator('#utility-naming-enabled');await expect(naming).toBeChecked(LIVE);await naming.uncheck();await page.click('#utility-settings-save');await expect.poll(()=>page.evaluate(()=>!!globalThis.releaseUtility)).toBe(true);await naming.check();await page.evaluate(()=>globalThis.releaseUtility());await expect(page.locator('#utility-settings-save')).toBeEnabled(LIVE);await expect(naming).toBeChecked();await expect(page.locator('[data-settings-panel=utility]')).toHaveAttribute('data-unsaved','true');
 await page.click('[data-settings-category=wechat]');
 await page.evaluate(()=>{const original=rpc;rpc=async(method,params)=>method==='wechat.pairing.create'?new Promise(resolve=>{globalThis.releasePair=()=>resolve({code:'private-delayed-code'});}):original(method,params);dom['wechat-pair'].disabled=false;});
 await page.click('#wechat-pair');await expect.poll(()=>page.evaluate(()=>!!globalThis.releasePair)).toBe(true);await page.click('#nav-help');await page.evaluate(()=>globalThis.releasePair());await page.click('#nav-settings');await page.click('[data-settings-category=wechat]');await expect(page.locator('#wechat-pairing-code')).toBeEmpty();
});

test('run now executes its authorized conversation without changing the regular schedule',async({page})=>{
 await openWorkbench(page,hostInfo('workbench'));const session=await createWorkbenchConversation(page,'Manual schedule execution');
 const rule=await page.evaluate(async session=>(await rpc('schedule.create',{session,title:'Run once now',instruction:'Report the current project status.',trigger:{kind:'time',rule:{kind:'interval',anchor_ms:Date.now()+3600000,every_ms:3600000}},budget:{max_runs:1,max_tokens:100000,max_seconds:300,run_tokens:100000,run_seconds:300,max_paid_web_calls:0,run_paid_web_calls:0},model_profile:null})).rule,session);
 await page.click('#nav-tasks');await page.locator('[data-task-id="'+rule.id+'"]').click();await page.locator('#jobs-dialog [data-action=review]').click();await page.locator('.schedule-review [data-action=confirm]').click();
 const before=await page.evaluate(async id=>(await rpc('schedule.list',{})).rules.find(r=>r.id===id),rule.id);await page.locator('#jobs-dialog [data-action=run_now]').click();
 await expect.poll(()=>page.evaluate(async id=>(await rpc('schedule.deliveries',{})).deliveries.find(d=>d.rule_id===id)?.phase,rule.id),LIVE).toBe('completed');
 const result=await page.evaluate(async id=>({rule:(await rpc('schedule.list',{})).rules.find(r=>r.id===id),deliveries:(await rpc('schedule.deliveries',{})).deliveries.filter(d=>d.rule_id===id)}),rule.id);
 expect(result.deliveries.length).toBe(1);expect(result.deliveries[0].scope.session).toBe(session);expect(result.rule.spec.trigger).toEqual(before.spec.trigger);expect(result.rule.used.runs).toBe(1);
});

test('a permission draft cannot be applied to a conversation selected after the edit began',async({page})=>{
 await openWorkbench(page,hostInfo('workbench'));await createWorkbenchConversation(page,'Permission owner A');await page.click('#nav-settings');await page.click('[data-settings-category=permissions]');await page.locator('label').filter({has:page.locator('input[name=permission-mode][value=read-only]')}).click();
 await page.click('#nav-workbench');await createWorkbenchConversation(page,'Permission owner B');const before=await page.evaluate(async()=>(await rpc('workbench.info',{})).permission.mode);
 await page.click('#nav-settings');await page.click('[data-settings-category=permissions]');await page.click('#permission-save');expect(await page.evaluate(async()=>(await rpc('workbench.info',{})).permission.mode)).toBe(before);await expect(page.locator('[data-settings-panel=permissions]')).toHaveAttribute('data-unsaved','true');
});
