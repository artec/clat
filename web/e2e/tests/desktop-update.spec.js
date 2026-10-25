const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench}=require('../helpers/workbench');

test('desktop entry aligns with sidebar tools and language controls have breathing room',async({page})=>{
  const fs=require('node:fs'),path=require('node:path');
  await page.route('**/style.css',async route=>{
    const response=await route.fetch();
    const css=(await response.text()).split('\n').filter(line=>!line.includes('.desktop-dialog-close')).join('\n');
    await route.fulfill({response,body:css}); // A running Host can predate the desktop close-button styles.
  });
  await openWorkbench(page,hostInfo('success'));
  const catalogs=Object.fromEntries(['en','zh-Hans','zh-Hant'].map(id=>[id,JSON.parse(fs.readFileSync(path.resolve(__dirname,`../../../locales/${id}.json`),'utf8'))]));
  await page.evaluate(catalogs=>{
    window.__TAURI__={event:{listen:async()=>()=>{}},core:{invoke:async(command,args)=>{
      if(command==='desktop_preferences')return {preference:args.language,locale:args.language,catalogs};
      return {desktopVersion:'2.1.4',backendVersion:'2.1.4',hostVersion:'2.1.4',hostConnected:true,autoCheck:true};
    }}};
  },catalogs);
  await page.route('**/desktop-bridge-fixture.js',route=>route.fulfill({contentType:'application/javascript',body:fs.readFileSync(path.resolve(__dirname,'../../../desktop/ui/bridge.js'),'utf8')}));
  await page.addScriptTag({url:hostInfo('success').origin+'/desktop-bridge-fixture.js'});
  const launch=page.locator('.sidebar-footer > .sidebar-footer-button').first();
  await expect(launch.locator('svg')).toBeVisible();
  for(const language of ['en','zh-Hans','zh-Hant']){
    await page.evaluate(language=>CLATI18n.setLanguage(language),language);
    const positions=await page.evaluate(()=>{
      const box=s=>document.querySelector(s).getBoundingClientRect();
      const launch=document.querySelector('.sidebar-footer > .sidebar-footer-button');
      const statusIcon=box('#sidebar-footnote svg'),statusText=box('#sidebar-connection');
      return {icon:launch.querySelector('svg').getBoundingClientRect().x,text:launch.querySelector('span').getBoundingClientRect().x,
        otherIcon:box('#jobs-open svg').x,otherText:box('#jobs-open span').x,
        statusOffset:Math.abs(statusIcon.y+statusIcon.height/2-statusText.y-statusText.height/2)};
    });
    expect(positions.icon).toBeCloseTo(positions.otherIcon,0);
    expect(positions.text).toBeCloseTo(positions.otherText,0);
    expect(positions.statusOffset).toBeLessThanOrEqual(1);
    await launch.click();
    const gap=await page.locator('#clat-desktop-settings').evaluate(dialog=>{
      const select=dialog.querySelector('[data-language-select]');
      return select.getBoundingClientRect().left-select.previousElementSibling.getBoundingClientRect().right;
    });
    expect(gap).toBeGreaterThanOrEqual(10);
    const dialogBox=await page.locator('#clat-desktop-settings').boundingBox();
    const closeBox=await page.locator('#clat-desktop-settings [data-action=close]').boundingBox();
    expect(closeBox.y-dialogBox.y).toBeLessThanOrEqual(16);
    expect(dialogBox.x+dialogBox.width-closeBox.x-closeBox.width).toBeLessThanOrEqual(16);
    await page.locator('#clat-desktop-settings [data-action=close]').click();
  }
  await page.evaluate(()=>{state.theme='dark';applyPresentation();});
  await page.screenshot({path:'../../output/playwright/desktop-sidebar-alignment.png'});
  await launch.click();
  await page.screenshot({path:'../../output/playwright/desktop-language-spacing.png'});
});

test('desktop installation lock protects actual workbench draft state and blocks late image selection',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  const result=await page.evaluate(async()=>{
    window.__TAURI__={}; // Only enables the local lock event; no native commands are called.
    const fire=()=>window.dispatchEvent(new CustomEvent('clat-desktop-install-lock',{cancelable:true}));
    dom.prompt.value='未发送文字';const textAccepted=fire(),text=dom.prompt.value;
    dom.prompt.value='';state.draft.images.push({id:'held-image'});const imageAccepted=fire(),image=state.draft.images[0].id;
    state.draft.images=[];state.draft.sending=true;const sendingAccepted=fire();state.draft.sending=false;
    composerDrafts.set("other-session",{text:"other session draft",generation:1});const otherAccepted=fire();composerDrafts.delete("other-session");
    pendingTextSteering.set("not-claimed",{text:"held text"});const steeringAccepted=fire();pendingTextSteering.delete("not-claimed");
    const accepted=fire();const locked={input:dom.prompt.disabled,send:dom.send.disabled,attachment:dom['attachment-open'].disabled};
    await addDraftFiles([new File(['image'],'late.png',{type:'image/png'})]);
    const lateCount=state.draft.images.length;
    window.dispatchEvent(new Event('clat-desktop-install-unlock'));
    delete window.__TAURI__;
    return {textAccepted,text,imageAccepted,image,sendingAccepted,otherAccepted,steeringAccepted,accepted,locked,lateCount,unlocked:!dom.prompt.disabled&&!dom.send.disabled&&!dom['attachment-open'].disabled};
  });
  expect(result).toEqual({textAccepted:false,text:'未发送文字',imageAccepted:false,image:'held-image',sendingAccepted:false,otherAccepted:false,steeringAccepted:false,accepted:true,locked:{input:true,send:true,attachment:true},lateCount:0,unlocked:true});
});

test('ordinary browser pages cannot accidentally take a desktop update lock',async({page})=>{
  await openWorkbench(page,hostInfo('success'));
  await page.evaluate(()=>window.dispatchEvent(new CustomEvent('clat-desktop-install-lock',{cancelable:true})));
  await expect(page.locator('#prompt')).toBeEnabled();
});
