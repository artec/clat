const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function fixture(){
 const pending=[],streams=[],controls=new Map([['files-open',{}]]),window=new EventTarget();
 const state={workbench:null,selectionGeneration:0,reconnectDelayMs:1000};
 const scope={state,window,Event,workspacePrefix:'/workspace/one',console,
  rpc:method=>new Promise((resolve,reject)=>pending.push({method,resolve,reject})),
  document:{getElementById:id=>controls.get(id)},CLATI18n:{msg:k=>k,present:r=>r?.key||'',attribute:(node,key,read)=>node[key]=read()},
  setConnStatus(){},openStream:()=>new Promise((resolve,reject)=>streams.push({resolve,reject})),
  closeQuestionCards(){},conversationRounds:{stop(){}},runActivity:{hide(){}},context:{changed(){}},
  updateRunState(){},updateRunDetail(){},setTimeout(){},
  clearRoundFollowups(){},invalidateSuggestion(){},syncSuggestionControl(){},
 };
 vm.createContext(scope);
 vm.runInContext(fs.readFileSync('web/workflow-details.js','utf8').replace(/\r\n/g,'\n'),scope);
 const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
 for(const name of ['refreshBuiltinAvailability','connect']){
  const start=source.indexOf('async function '+name+'(');
  if(start>=0)vm.runInContext(source.slice(start,source.indexOf('\n}',start)+2),scope);
 }
 return{scope,state,pending,streams,controls};
}
const reply=enabled=>({instance_id:'host-1',builtin_availability:{'builtin.development':{enabled,reason:enabled?undefined:{key:'disabled'}}}});
const settle=()=>new Promise(resolve=>setImmediate(resolve));
test('startup capabilities arrive while replay and workbench remain pending',async()=>{
 const f=fixture();let fullRefreshes=0;f.scope.window.addEventListener('clat:workbench-updated',()=>fullRefreshes++);void f.scope.connect();
 assert.equal(f.pending[0]?.method,'host.describe');
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 assert.equal(f.controls.get('files-open').disabled,true);
 f.pending[0].resolve(reply(true));await settle();
 assert.equal(f.state.workbench,null);
 assert.equal(f.scope.builtinReady('builtin.development'),true);
 assert.equal(f.controls.get('files-open').disabled,false);
 assert.equal(fullRefreshes,0);
});
test('reconnect rejects old replies and retains explicit disabled reasons',async()=>{
 const f=fixture();void f.scope.connect();const old=f.pending[0];
 void f.scope.connect();old.resolve(reply(true));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 f.pending[1].resolve(reply(false));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 assert.equal(f.scope.builtinUnavailableReason('builtin.development'),'disabled');
});
test('project changes and failed loads cannot reuse old availability',async()=>{
 const f=fixture();void f.scope.connect();f.scope.workspacePrefix='/workspace/two';
 f.pending[0].resolve(reply(true));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 void f.scope.connect();f.pending[1].reject(new Error('offline'));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
});
test('old stream failures cannot revoke a new connection, but its own failure does',async()=>{
 const f=fixture();void f.scope.connect();void f.scope.connect();
 f.pending[1].resolve(reply(true));await settle();
 f.streams[0].reject(new Error('old stream failed'));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),true);
 f.streams[1].reject(new Error('current stream failed'));await settle();
 f.pending[0].resolve(reply(true));await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 assert.equal(f.controls.get('files-open').disabled,true);
});
test('older hosts without the additive projection retain the workbench fallback',async()=>{
 const f=fixture();void f.scope.connect();
 f.pending[0].resolve({instance_id:'old-host'});await settle();
 assert.equal(f.scope.builtinReady('builtin.development'),false);
 f.state.workbench={builtin_availability:reply(true).builtin_availability};
 assert.equal(f.scope.builtinReady('builtin.development'),true);
});
test('workspace menu updates availability even when page actions are unchanged',async()=>{
 const f=fixture();
 const node=()=>({children:[],dataset:{},append(...items){this.children.push(...items);},replaceChildren(){this.children=[];},setAttribute(){}});
 Object.assign(f.scope,{
  WORKSPACE_TOOLS:[{key:'files',icon:'folder',id:'files'}],
  viewAction:(id,icon,label,run)=>({id,icon,label,run}),viewText:k=>k,experienceText:k=>k,
  shellButton:()=>node(),navigationIcon:()=>node(),el:()=>node(),
  helpText:k=>k,
 });
 const source=fs.readFileSync('web/application-toolbar.js','utf8').replace(/\r\n/g,'\n');
 const start=source.indexOf('function renderApplicationActions(');
 vm.runInContext(source.slice(start,source.indexOf('\n}',start)+2),f.scope);
 const chrome={actions:node(),menu:node(),more:node(),close(){}},view={id:'conversation-page',workspace:true};
 const shell={tools:{current:()=>null}};
 f.scope.renderApplicationActions(chrome,view,shell);
 assert.equal(chrome.menu.children.at(-1).children[0].disabled,true);
 void f.scope.connect();f.pending[0].resolve(reply(true));await settle();
 f.scope.renderApplicationActions(chrome,view,shell);
 assert.equal(chrome.menu.children.at(-1).children[0].disabled,false);
});
