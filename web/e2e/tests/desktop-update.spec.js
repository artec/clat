const {test,expect}=require('@playwright/test');
const {hostInfo,openWorkbench}=require('../helpers/workbench');

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
