const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
function fixture(availability){
  const requests=[],notices=[],controls=new Map();
  const state={workbench:availability?{builtin_availability:availability}:null,draft:{}};
  const context=vm.createContext({state,window:new EventTarget(),document:{getElementById:id=>controls.get(id)},
    CLATI18n:{msg:key=>key,present:reason=>reason?.key||'',view:read=>read(),error:text=>new Error(text),attribute:(node,key,read)=>node[key]=read()},
    workspacePrefix:'',authHeaders:()=>({}),fetch:async url=>{requests.push(url);return{json:async()=>({ok:true,value:{available:true}})}},updateRunState:text=>notices.push(text)});
  vm.runInContext(fs.readFileSync('web/workflow-details.js','utf8').replace(/\r\n/g,'\n'),context);
  const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
  for(const name of ['rpc','addDraftFiles']){const start=source.indexOf('async function '+name+'(');vm.runInContext(source.slice(start,source.indexOf('\n}',start)+2),context);}
  return {context,requests,notices,controls,state};
}
test('unknown and disabled resource capabilities cannot start I/O while other RPCs remain available',async()=>{
  for(const availability of [null,{'builtin.development':{enabled:false,reason:{key:'disabled'}},'builtin.draft_images':{enabled:false,reason:{key:'disabled'}}}]){
    const f=fixture(availability);
    for(const method of ['editor.list','git.snapshot','terminal.directory','draft.open'])await assert.rejects(f.context.rpc(method,{}));
    await f.context.addDraftFiles([{get type(){throw new Error('image read before availability');}}]);
    assert.equal(f.notices.length,1);assert.deepEqual(f.requests,[]);
    await f.context.rpc('workbench.info',{});await f.context.rpc('session.history',{});
    assert.deepEqual(f.requests,['/api/workbench.info','/api/session.history']);
  }
});
test('availability controls restore per capability and active capabilities can make requests',async()=>{
  const f=fixture({'builtin.development':{enabled:true},'builtin.draft_images':{enabled:false,reason:{key:'disabled'}}});
  for(const id of ['nav-terminal','files-open','attachment-input','add-image'])f.controls.set(id,{});
  f.context.syncOptionalResourceControls();
  assert.equal(f.controls.get('nav-terminal').disabled,false);assert.equal(f.controls.get('files-open').disabled,false);
  assert.equal(f.controls.get('attachment-input').disabled,true);assert.equal(f.controls.get('add-image').disabled,true);
  await f.context.rpc('git.snapshot',{});assert.deepEqual(f.requests,['/api/git.snapshot']);
  f.state.workbench=null;f.context.syncOptionalResourceControls();assert.equal(f.controls.get('files-open').disabled,true);
});
test('startup description bypasses project routing while ordinary queries retain it',async()=>{
  const f=fixture(null);f.context.workspacePrefix='/workspace/project-one';
  await f.context.rpc('host.describe',{});await f.context.rpc('workbench.info',{});
  assert.deepEqual(f.requests,['/api/host.describe','/workspace/project-one/api/workbench.info']);
});
