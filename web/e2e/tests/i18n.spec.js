const {test,expect}=require('@playwright/test');
const {LIVE,hostInfo,openWorkbench,createWorkbenchConversation}=require('../helpers/workbench');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path');

test('image upload rejection retains its presentation while a draft changes language',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.route('**/api/drafts/*/images',route=>route.fulfill({status:400,contentType:'application/json',body:JSON.stringify({ok:false,error:{message:'original provider diagnostic',presentation:{key:'webtool.not_configured',args:{}}}})}));
  await page.fill('#prompt','保留未发送的草稿');
  await page.evaluate(async()=>{
    const canvas=document.createElement('canvas');canvas.width=2;canvas.height=2;
    const blob=await new Promise(resolve=>canvas.toBlob(resolve,'image/png'));
    await addDraftFiles([new File([blob],'draft.png',{type:'image/png'})]);
  });
  await expect(page.locator('.attachment-chip.is-failed')).toHaveCount(1);
  for(const locale of ['en','zh-Hans','zh-Hant']){
    const expected=await page.evaluate(async locale=>{await CLATI18n.setLanguage(locale);return CLATI18n.t('webtool.not_configured');},locale);
    await expect(page.locator('.attachment-state')).toContainText(expected);
    await expect(page.locator('#prompt')).toHaveValue('保留未发送的草稿');
  }
});

test('client validation and missing RPC reasons stay localizable without extra requests',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  let creates=0;
  page.on('request',request=>{if(request.url().endsWith('/api/schedule.create'))creates++;});
  await page.route('**/api/i18n.fixture',route=>route.fulfill({contentType:'application/json',body:JSON.stringify({ok:false,error:{}})}));
  await page.evaluate(async()=>{
    const error=await rpc('i18n.fixture',{}).catch(error=>error);
    document.body.append(el('p','i18n-error-fixture',()=>error.message));
  });
  await createWorkbenchConversation(page,'Validation target');
  await page.click('#nav-tasks');await page.locator('#jobs-dialog [data-action=create_task]').first().click();
  const dialog=page.locator('#schedule-editor');
  await dialog.locator('.settings-advanced summary').click();
  await dialog.locator('[name=max_runs]').fill('-1');
  await dialog.locator('[data-action=save]').click();
  for(const locale of ['en','zh-Hans','zh-Hant']){
    const expected=await page.evaluate(async locale=>{await CLATI18n.setLanguage(locale);return [CLATI18n.t('schedule.invalid_budget'),CLATI18n.t('web.request_failed')];},locale);
    await expect(dialog.locator('[role=status]')).toContainText(expected[0]);
    await expect(page.locator('.i18n-error-fixture')).toContainText(expected[1]);
  }
  expect(creates).toBe(0);
});

test('a delayed project list cannot erase a newer trust failure or its translation binding',async({page})=>{
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'clat-language-untrusted-'));
  let delayed;
  try{
    await openWorkbench(page,hostInfo('success'));
    await page.route('**/api/workspace.directory',route=>{delayed=route;});
    await page.click('#nav-settings');
    await page.click('[data-settings-category="projects"]');
    await expect.poll(()=>!!delayed,LIVE).toBe(true);
    await page.fill('#workspace-root',root);await page.click('#workspace-open');
    await expect(page.locator('#workspace-error')).toContainText('not trusted',LIVE);
    const response=page.waitForResponse(r=>r.url().endsWith('/api/workspace.directory'));
    await delayed.fulfill({contentType:'application/json',body:JSON.stringify({ok:true,value:{projects:[]}})});delayed=null;
    await response;
    await page.evaluate(()=>CLATI18n.setLanguage('zh-Hant'));
    await expect(page.locator('#workspace-error')).toContainText('尚未受信任');
  }finally{
    if(delayed)await delayed.abort();
    fs.rmSync(root,{recursive:true,force:true});
  }
});

test('translated model actions keep their stable edit and delete semantics',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    await CLATI18n.setLanguage('zh-Hant');
    const calls=[];
    const originalAction=modelAction,originalLoad=loadModelProfile,originalRpc=rpc;
    modelAction=(_,action)=>action();
    loadModelProfile=name=>calls.push(['edit',name]);
    rpc=async(method,params)=>calls.push([method,params.name]);
    const originalConfirm=window.confirm;window.confirm=()=>true;
    try{
      const container=modelField('profiles');container.replaceChildren();renderModelProfile('test-profile',true);
      const buttons=container.querySelectorAll('button');
      const labels=[...buttons].map(b=>b.textContent);
      buttons[0].click();buttons[1].click();
      await Promise.resolve();
      return {calls,labels};
    }finally{modelAction=originalAction;loadModelProfile=originalLoad;rpc=originalRpc;window.confirm=originalConfirm;}
  });
  expect(result.calls).toEqual([['edit','test-profile'],['model.profile.delete','test-profile']]);
});

test('interface languages stay local and preserve the current draft, composition and selection',async({page,browser})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.locator('#prompt').fill('未发送草稿 {path} <script>');
  const before=await page.evaluate(()=>({generation:state.composerGeneration,session:state.sessionId}));
  let requests=[];page.on('request',r=>{if(r.url().includes('/api/')&&r.method()==='POST')requests.push(r.url());});
  await page.locator('#nav-settings').click();
  const select=page.locator('#settings-dialog [data-language-select]');
  for(const id of ['zh-Hans','zh-Hant','en']){
    await select.selectOption(id);
    await expect(page.locator('html')).toHaveAttribute('lang',id);
    await expect(page.locator('#prompt')).toHaveValue('未发送草稿 {path} <script>');
    expect(await page.evaluate(()=>({generation:state.composerGeneration,session:state.sessionId}))).toEqual(before);
  }
  expect(requests.some(u=>/prompt.send|steer.send|approval.respond|queue.enqueue|session.new/.test(u))).toBe(false);
  const other=await browser.newContext({locale:'zh-TW'});
  const tab=await other.newPage();await openWorkbench(tab,hostInfo('success'));
  await expect(tab.locator('html')).toHaveAttribute('lang','zh-Hant',LIVE);
  await expect(page.locator('html')).toHaveAttribute('lang','en');
  await other.close();
});

test('switching during an active stream preserves IME, images, questions, approvals and queued text',async({page})=>{
  await openWorkbench(page,hostInfo('long-stream'));
  await page.fill('#prompt','language switch streaming probe');await page.click('#send');
  await expect(page.locator('#cancel')).toBeVisible(LIVE);
  await expect(page.locator('.msg.assistant').last()).not.toBeEmpty(LIVE);
  const result=await page.evaluate(async()=>{
    dom.prompt.value='正在输入的中文草稿';dom.prompt.focus();dom.prompt.setSelectionRange(2,6);
    dom.prompt.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));
    state.draft.images.push({id:'language-image',url:'',name:'中文.png',type:'image/png',bytes:5,status:'failed',error:'raw diagnostic'});renderDraft();
    renderNextTurnQueue([{text:'已经排队的原文'}]);
    onApprovalRequested({rpc_id:'language-approval',request:{tool:'write_file',arguments:{path:'中文.txt',content:'original <script>'}}});
    onQuestionNotice({kind:'question_requested',payload:{rpc_id:'language-question',question:{question:'原始问题',options:[],allow_custom:true}}});
    const refs={prompt:dom.prompt,image:document.querySelector('[data-image-id="language-image"]'),approval:document.querySelector('.approval-card[data-rpc-id="language-approval"]'),question:document.querySelector('.question-card'),queue:dom['next-turn-queue'].firstChild};
    const original=rpc,calls=[];rpc=(method,params)=>{calls.push(method);return original(method,params);};
    const generation=state.composerGeneration,stream=state.stream;
    try{
      for(const language of ['zh-Hans','zh-Hant','en'])await CLATI18n.setLanguage(language);
      return {calls,draft:dom.prompt.value,selection:[dom.prompt.selectionStart,dom.prompt.selectionEnd],focused:document.activeElement===dom.prompt,
        sameGeneration:state.composerGeneration===generation,sameStream:state.stream===stream,
        sameNodes:!!refs.approval&&refs.approval===document.querySelector('.approval-card[data-rpc-id="language-approval"]')&&refs.prompt===dom.prompt&&refs.image===document.querySelector('[data-image-id="language-image"]')&&refs.question===document.querySelector('.question-card')&&refs.queue===dom['next-turn-queue'].firstChild,
        approvalButtons:refs.approval?.querySelectorAll(".actions button").length,images:state.draft.images.map(i=>i.id),question:document.querySelector('.question-card').textContent,queue:dom['next-turn-queue'].textContent};
    }finally{rpc=original;dom.prompt.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true}));}
  });
  expect(result.calls.filter(method=>/^(prompt.send|steer.send|queue.enqueue|approval.respond|session.(new|switch)|command.run|permission.set|model.profile.(save|delete)|schedule.(create|update|confirm|pause|resume|delete|run_now)|plugin.(commit|remove))$/.test(method))).toEqual([]);expect(result.draft).toBe('正在输入的中文草稿');expect(result.selection).toEqual([2,6]);
  expect(result.focused&&result.sameGeneration&&result.sameStream&&result.sameNodes).toBe(true);
expect(result.images).toEqual(['language-image']);expect(result.question).toContain('原始问题');expect(result.queue).toContain('已经排队的原文');
  await page.click('#cancel');await expect(page.locator('#cancel')).toBeHidden(LIVE);
});

test('English fallback survives unavailable language resources and message parameters remain text',async({page})=>{
  await page.route('**/locales/zh-Hans.json',route=>route.abort());
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    await CLATI18n.setLanguage('zh-Hans');
    const notice=addNoticeLine(CLATI18n.msg('common.error',{detail:'<img src=x onerror="globalThis.pwned=true"> {path}'}));
    return {locale:CLATI18n.locale,text:notice.textContent,html:notice.querySelector('img'),pwned:globalThis.pwned===true};
  });
  expect(result.locale).toBe('en');expect(result.text).toContain('<img');expect(result.text).toContain('{path}');expect(result.html).toBeNull();expect(result.pwned).toBe(false);
  await page.fill('#prompt','fallback still works');await page.click('#send');await expect(page.locator('.msg.assistant').last()).toBeVisible(LIVE);
});

test('retained background notices, nested arguments and attributes switch together',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    const child={key:'command.compacted',args:{count:7},fallback:'compacted: shadowed {count} events'};
    onNotice({kind:'compaction',payload:{status:'finished',succeeded:true,note:child.fallback,presentation:child}});
    const card=document.querySelector('.notice.compaction');
    const message={key:'common.error',args:{detail:'English fallback'},arg_presentations:{detail:child},fallback:'Error: {detail}'};
    const node=addNoticeLine(CLATI18n.present(message));
    const before=state.sessionId,seen=[];
    for(const id of ['zh-Hans','zh-Hant','en']){
      await CLATI18n.setLanguage(id);seen.push({id,status:dom['run-state'].textContent,nested:node.textContent});
    }
    return {seen,sameSession:state.sessionId===before,hasHtml:!!node.querySelector('script')};
  });
  expect(result.sameSession).toBe(true);expect(result.hasHtml).toBe(false);
  expect(result.seen[0].status).toContain('已压缩');expect(result.seen[1].nested).toContain('已壓縮');
  expect(result.seen[2].status).toContain('compacted: shadowed 7 events');
});

test('plugin consent is reset after translation while the ticket and secret draft stay local',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    const original=rpc,calls=[];
    const plugin={id:'fixture.language',name:'Fixture',version:'1.0.0',runtime:'mcp-stdio',capabilities:{},config_schema:{type:'object',properties:{token:{type:'string',writeOnly:true}}}};
    rpc=async(method,params)=>{
      calls.push([method,params]);
      if(method==='plugin.prepare')return {ticket:'language-ticket',action:'install',review:{packages:[plugin]}};
      if(method==='plugin.cancel')return {};
      throw new Error('unexpected fixture call '+method);
    };
    try{
      workbenchShell.show('market-dialog');
      const card=document.createElement('article');pluginMarket.decorate(card,{id:plugin.id,status:'available'});document.body.append(card);
      card.querySelector('button').click();
      await new Promise(resolve=>setTimeout(resolve,0));
      const review=document.querySelector('.plugin-review'),secret=review.querySelector('input[type=password]'),consent=review.querySelector('.plugin-consent input'),activate=review.querySelector('[data-requires-consent]');
      if(!secret||!activate)throw new Error('missing actual plugin review');
      secret.value='opaque test secret';consent.checked=true;consent.dispatchEvent(new Event('change'));
      const enabledBefore=!activate.disabled;
      await CLATI18n.setLanguage('zh-Hant');
      const retained=secret===review.querySelector('input[type=password]')&&secret.value==='opaque test secret';
      const blocked=activate.disabled&&!consent.checked;
      const callsAfterSwitch=calls.map(([method])=>method);
      review.querySelector('button:not([data-requires-consent])').click();await Promise.resolve();
      return {enabledBefore,retained,blocked,callsAfterSwitch,cancel:calls.find(([method])=>method==='plugin.cancel')?.[1],text:review.textContent};
    }finally{rpc=original;}
  });
  expect(result.enabledBefore&&result.retained&&result.blocked).toBe(true);
  expect(result.callsAfterSwitch.filter(method=>method.startsWith('plugin.'))).toEqual(['plugin.prepare']);expect(result.cancel).toEqual({ticket:'language-ticket'});
});


test('three interface languages keep real Host approvals and model-tool-model completion semantics',async({page})=>{
  await openWorkbench(page,hostInfo('run-command'));
  for(const language of ['en','zh-Hans','zh-Hant']){
    for(const decision of ['allow','deny']){
      await page.click('.project-group:has(#session-list) [data-action=new-project-session]');await expect(page.locator('.msg.user')).toHaveCount(0,LIVE);await expect(page.locator('#send')).toBeEnabled(LIVE);
      await page.evaluate(async language=>{await CLATI18n.setLanguage(language);await rpc('permission.set',{mode:'read-only'});await refreshWorkbench();},language);
      const prompt='原始提示词 '+language+' '+decision;
      await page.fill('#prompt',prompt);await page.click('#send');
      const card=page.locator('.approval-card').last();await expect(card).toBeVisible(LIVE);
      await expect(card.locator('.title')).toContainText('run_command');await expect(card.locator('button.primary')).toBeEnabled();
      await expect(page.locator('#composer-permission')).toHaveAttribute('data-mode','read-only');
      await card.locator(decision==='allow'?'button.primary':'button.ghost').click();
      await expect(page.locator('.verdict.completed')).toBeVisible(LIVE);
      await expect(page.locator('.tool-card')).toHaveCount(1);
      await expect(page.locator('.tool-card')).toHaveAttribute('data-state',decision==='allow'?'done':'denied');
      await expect(page.locator('.msg.user .body')).toHaveText(prompt);
      await expect(page.locator('.approval-card .actions button')).toHaveCount(0,LIVE);
      const terminal=await page.evaluate(()=>({active:state.runActive,pending:document.querySelector('.approval-card .actions button')!==null}));
      expect(terminal).toEqual({active:false,pending:false});
    }
  }
});

test('static defaults never overwrite a later host status or accessibility binding',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    const node=document.createElement('span');node.dataset.i18n='web.project_write';node.setAttribute('data-i18n-title','web.project_write');node.textContent='Project Write';document.body.append(node);
    CLATI18n.refresh();CLATI18n.bind(node,'textContent',()=>CLATI18n.msg('web.read_only'));CLATI18n.attribute(node,'title',()=>CLATI18n.msg('web.full_access'));
    const labels=[];
    for(const language of ['zh-Hans','zh-Hant','en']){await CLATI18n.setLanguage(language);labels.push([node.textContent,node.title]);}
    return labels;
  });
  expect(result).toEqual([['只读','完全访问'],['唯讀','完整存取'],['Read Only','Full Access']]);
});
