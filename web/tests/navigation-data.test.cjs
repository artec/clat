const {test}=require('node:test');const assert=require('node:assert/strict');const fs=require('node:fs'),vm=require('node:vm'),path=require('node:path');
const source=name=>fs.readFileSync(path.join(__dirname,'..',name),'utf8');
test('a committed retry can prove a reference without treating the first unknown receipt as success',async()=>{
 const changes=[];let messages=[];const scope={composerScope:'project:new',composerOwnerAliases:new Map(),state:{sessionId:'session',workbench:{project:{root:'/project'}}},newOpaqueClientId:()=> 'first',rpc:async(method,p)=>method==='history.conversation'?{messages,next_seq:null}:changes.push(p)};
 vm.createContext(scope);vm.runInContext(source('resource-library.js'),scope);
 scope.snapshot={path:'a.md',version:'v',reference:'fixed quote\n'};vm.runInContext('queueFileReference(snapshot)',scope);
 scope.params={};vm.runInContext("bindFileReferenceSubmission(composerScope,'fixed quote',params)",scope);
 assert.equal(scope.params.clientMessageId,'first');await vm.runInContext('reconcileFileReferences()',scope);assert.equal(changes.length,0);
 scope.params={clientMessageId:'retry'};vm.runInContext("bindFileReferenceSubmission(composerScope,'fixed quote',params)",scope);
 messages=[{role:'user',client_message_id:'retry',text:'fixed quote',reference:{seq:4}}];await vm.runInContext('reconcileFileReferences()',scope);assert.equal(changes.length,1);assert.equal(changes[0].reference.seq,4);
});
test('a recalled share ignores a stale preview failure after the selection changes',async()=>{
 let rejectPreview;const notices=[];
 const scope={composerScope:'project:one',composerOwnerAliases:new Map(),state:{selectionGeneration:1},
  composerOwns(owner){return owner===scope.composerScope},renderSessionShareDraft(){},syncInputSubmitButtons(){},
  updateRunState(message){notices.push(message)},rpc(){return new Promise((_,reject)=>{rejectPreview=reject})}};
 vm.createContext(scope);vm.runInContext(source('resource-library.js'),scope);
 scope.item={draft:{sessionShares:[{reference:{session:'source'},recent_turns:null,version:'v'}]}};
 vm.runInContext('renderSessionShareDraft = () => {}',scope);
 vm.runInContext("restoreSessionShares(item, 'project:one')",scope);
 scope.state.selectionGeneration=2;
 rejectPreview(new Error('old error'));
 await new Promise(resolve=>setImmediate(resolve));
 assert.deepEqual(notices,[]);
 assert.equal(vm.runInContext("sessionSharesFor('project:one').length",scope),1);
});
test('a share recalled after switching away refreshes when its draft is reopened',async()=>{
 const calls=[];
 const scope={composerScope:'project:two',composerOwnerAliases:new Map(),state:{selectionGeneration:2},
  composerOwns(owner){return owner===scope.composerScope},renderSessionShareDraft(){},syncInputSubmitButtons(){},
  updateRunState(){},rpc(method,params){calls.push(params);return Promise.resolve({...params,version:'fresh',title:'Source',turns:2,characters:12})}};
 vm.createContext(scope);vm.runInContext(source('resource-library.js'),scope);
 scope.item={draft:{sessionShares:[{reference:{session:'source'},recent_turns:null,version:'old'}]}};
 vm.runInContext('renderSessionShareDraft = () => {}',scope);
 vm.runInContext("restoreSessionShares(item, 'project:one')",scope);
 assert.equal(calls.length,0);
 scope.composerScope='project:one'; scope.state.selectionGeneration=3;
 vm.runInContext("refreshRecalledSessionShares('project:one')",scope);
 await new Promise(resolve=>setImmediate(resolve));
 assert.equal(calls.length,1);
 assert.equal(vm.runInContext("sessionSharesFor('project:one')[0].version",scope),'fresh');
});
test('an older recalled-share preview cannot overwrite a newer preview',async()=>{
 const replies=[]; const scope={composerScope:'project:one',composerOwnerAliases:new Map(),state:{selectionGeneration:1},
  composerOwns(owner){return owner===scope.composerScope},renderSessionShareDraft(){},syncInputSubmitButtons(){},
  updateRunState(){},rpc(){return new Promise(resolve=>replies.push(resolve))}};
 vm.createContext(scope);vm.runInContext(source('resource-library.js'),scope);
 scope.item={draft:{sessionShares:[{reference:{session:'source'},recent_turns:null,version:'old'}]}};
 vm.runInContext('renderSessionShareDraft = () => {}',scope);
 vm.runInContext("restoreSessionShares(item, 'project:one'); refreshRecalledSessionShares('project:one')",scope);
 replies[1]({version:'new'}); await new Promise(resolve=>setImmediate(resolve));
 replies[0]({version:'stale'}); await new Promise(resolve=>setImmediate(resolve));
 assert.equal(vm.runInContext("sessionSharesFor('project:one')[0].version",scope),'new');
});
test('reopening a draft previews against the incoming selection generation',async()=>{
 const calls=[];const scope={composerScope:'project:one',composerOwnerAliases:new Map(),state:{selectionGeneration:1},
  composerOwns(owner){return owner===scope.composerScope},renderSessionShareDraft(){},syncInputSubmitButtons(){},
  updateRunState(){},rpc(method,params){calls.push(params);return Promise.resolve({version:'new'})}};
 vm.createContext(scope);vm.runInContext(source('resource-library.js'),scope);
 scope.item={draft:{sessionShares:[{reference:{session:'source'},recent_turns:null,version:'old'}]}};
 vm.runInContext('renderSessionShareDraft = () => {}',scope);
 vm.runInContext("restoreSessionShares(item, 'project:two')",scope);
 scope.composerScope='project:two';
 vm.runInContext("refreshRecalledSessionShares('project:two', 2)",scope);
 scope.state.selectionGeneration=2;
 await new Promise(resolve=>setImmediate(resolve));
 assert.equal(calls[0].expected_selection_generation,2);
 assert.equal(vm.runInContext("sessionSharesFor('project:two')[0].version",scope),'new');
});
test('editing a continuation preserves its job trigger and a time edit preserves the interval anchor',()=>{
 const scope={CLATI18n:{msg:k=>k,error:m=>Error(m)}};vm.createContext(scope);vm.runInContext(source('scheduled-work.js'),scope);
 const fields=Object.fromEntries(Object.entries({trigger:'interval',interval:'60',title:'Renamed',notification:'attention',session:'session',instruction:'Keep instructions',model_profile:'',max_runs:'2',max_tokens:'100',max_seconds:'60',run_tokens:'50',run_seconds:'30',max_paid_web_calls:'0',run_paid_web_calls:'0'}).map(([key,value])=>[key,{value}]));scope.fields=fields;scope.rule={scope:{session:'session'},spec:{trigger:{kind:'job',job_id:'job-1'}}};
 const job=vm.runInContext("scheduleDraftFrom(fields,rule,{id:rule.spec.trigger.job_id})",scope);assert.equal(job.trigger.kind,'job');assert.equal(job.trigger.job_id,'job-1');
 scope.rule.spec.trigger={kind:'time',rule:{kind:'interval',anchor_ms:12345,every_ms:3600000}};const time=vm.runInContext('scheduleDraftFrom(fields,rule,null)',scope);assert.equal(time.trigger.rule.anchor_ms,12345);
});
