const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function runtime(){
  let tick,cleared=0;const scope={setInterval(fn){tick=fn;return 1;},clearInterval(){cleared++;}};
  vm.runInNewContext(fs.readFileSync('desktop/ui/update-controller.js','utf8').replace(/\r\n/g,'\n'),scope);
  return {api:scope.CLATDesktopUpdates,tick:()=>tick(),cleared:()=>cleared};
}
const available=()=>({updateAvailable:true,updateVersion:'3.0.0',updatePhase:'idle',updateBusy:false,updatePresentation:{key:'update.found'}});
function deferred(){let resolve;const promise=new Promise(r=>resolve=r);return{promise,resolve};}
test('only an available update or verified package exposes an actionable shortcut',()=>{
 const {api}=runtime();assert.equal(api.project({info:{updatePhase:'checking'}}).visible,false);
 assert.equal(api.project({info:{updatePresentation:{key:'update.index_network'}}}).visible,false);
 assert.equal(api.project({info:available()}).action,'download');
 assert.equal(api.project({info:{...available(),downloaded:true,updatePresentation:{key:'update.verified'}}}).action,'install');
});
test('download progress never fabricates a percentage for unknown totals',()=>{
 const {api}=runtime(),info={...available(),updateBusy:true,updatePhase:'downloading',updateReceived:25};
 assert.equal(api.project({info}).ratio,null);assert.equal(api.project({info}).action,null);
 assert.equal(api.project({info:{...info,updateTotal:100}}).ratio,.25);
 const allBytes=api.project({info:{...info,updateTotal:20}});
 assert.equal(allBytes.ratio,1);assert.equal(allBytes.action,null,'receiving all bytes does not verify the package');
});
test('native failures preserve retry target while cancellation is not a failure',()=>{
 const {api}=runtime();
 assert.equal(api.project({info:{...available(),updatePresentation:{key:'update.install_failed'}}}).failed,true);
 assert.equal(api.project({info:{...available(),downloaded:true,updatePresentation:{key:'update.install_failed'}}}).action,'install');
 assert.equal(api.project({info:{...available(),updatePresentation:{key:'update.cancel_download'}}}).failed,false);
 assert.equal(api.project({info:available(),error:{key:'update.cancel_download'},updateError:true}).failed,false);
 assert.equal(api.project({info:{...available(),updateBusy:true,updatePhase:'installing',updateCancellable:false}}).cancellable,false);
});
test('about and rail share one exclusive download and never automatically install',async()=>{
 const {api}=runtime(),download=deferred(),calls=[],about=[],rail=[];let info=available();
 const controller=api.create({invoke:async command=>{calls.push(command);if(command==='desktop_info')return info;await download.promise;info={...info,downloaded:true,updatePresentation:{key:'update.verified'}};}});
 controller.subscribe(value=>about.push(value));controller.subscribe(value=>rail.push(value));await controller.refresh();
 const pending=controller.act('download');await controller.act('download');await controller.act('install');
 assert.equal(calls.filter(c=>c==='desktop_download_update').length,1);
 download.resolve();await pending;
 assert.equal(calls.includes('desktop_install_update'),false);assert.equal(api.project(controller.snapshot()).action,'install');
 assert.deepEqual(about.at(-1),rail.at(-1));assert.equal(about.at(-1).info.downloaded,true);
});
test('draft rejection preserves verified bytes and never invokes installation',async()=>{
 const {api}=runtime(),calls=[],info={...available(),downloaded:true,updatePresentation:{key:'update.verified'}};
 const controller=api.create({invoke:async command=>{calls.push(command);return info;},prepare(){throw{key:'desktop.draft_warning'};}});
 await controller.refresh();await controller.act('install');
 assert.equal(calls.includes('desktop_install_update'),false);assert.equal(controller.snapshot().info.downloaded,true);
 assert.equal(controller.snapshot().error.key,'desktop.draft_warning');assert.equal(api.project(controller.snapshot()).failed,true);
});
test('retry clears the shared error and releases installation guards',async()=>{
 const {api}=runtime();let attempts=0,released=0;const info={...available(),downloaded:true,updatePresentation:{key:'update.verified'}};
 const controller=api.create({prepare:()=>()=>released++,invoke:async command=>{if(command==='desktop_info')return info;if(attempts++===0)throw{key:'update.install_failed'};}});
 await controller.refresh();await controller.act('install');assert.equal(api.project(controller.snapshot()).failed,true);
 await controller.act('install');assert.equal(controller.snapshot().error,null);assert.equal(released,2);
});
test('one scheduler polls busy updates each second even with about closed',async()=>{
 const r=runtime();let reads=0;const info={...available(),updateBusy:true,updatePhase:'downloading'};
 const c=r.api.create({invoke:async()=>{reads++;return info;}});c.start();c.start();await c.refresh();
 r.tick();await c.refresh();assert.equal(reads,2);c.stop();assert.equal(r.cleared(),1);
});
test('install and cancellation use the same operation and retain verified readiness',async()=>{
 const {api}=runtime(),install=deferred(),calls=[];let info={...available(),downloaded:true,updatePresentation:{key:'update.verified'}};
 const c=api.create({invoke:async command=>{calls.push(command);if(command==='desktop_info')return info;if(command==='desktop_install_update'){info={...info,updateBusy:true,updateCancellable:true,updatePhase:'waiting'};await install.promise;}if(command==='desktop_cancel_update'){info={...info,updateBusy:false,updatePhase:'idle',updatePresentation:{key:'update.cancel_install'}};install.resolve();}}});
 await c.refresh();const pending=c.act('install');await c.refresh();assert.equal(api.project(c.snapshot()).cancellable,true);
 await c.act('cancel');await pending;assert.equal(calls.filter(c=>c==='desktop_cancel_update').length,1);assert.equal(api.project(c.snapshot()).action,'install');
});
