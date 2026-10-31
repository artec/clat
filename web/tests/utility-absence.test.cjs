const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function fixture(enabled){
 const calls=[],draft={text:'keep my draft'},controls=new Map();
 const node=()=>({disabled:false,dataset:{},setAttribute(){}});
 controls.set('add-suggestion',node());
 const scope={window:new EventTarget(),state:{workbench:enabled===null?null:{builtin_availability:{'builtin.utility_model':{enabled,reason:{key:'disabled'}}}},utilitySettings:{suggestions_enabled:true},sessionId:'session',selectionGeneration:1,composerGeneration:2},
  dom:{suggest:node()},document:{getElementById:id=>controls.get(id)},CLATI18n:{bind:(node,key,read)=>node[key]=read(),attribute:(node,key,read)=>node[key]=read(),present:v=>v?.key||'',msg:key=>key},
  composerScope:{},composerOwns:()=>true,rpc:async(method)=>{calls.push(method);return{items:[]};},renderRoundFollowups(){},clearRoundFollowups(){},draft,
 };
 vm.createContext(scope);vm.runInContext(fs.readFileSync('web/workflow-details.js','utf8').replace(/\r\n/g,'\n'),scope);
 const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
 for(const name of ['syncSuggestionControl','requestSuggestion']){const prefix=name==='requestSuggestion'?'async ':'';const start=source.indexOf(prefix+'function '+name+'(');vm.runInContext(source.slice(start,source.indexOf('\n}',start)+2),scope);}
 const experience=fs.readFileSync('web/conversation-experience.js','utf8').replace(/\r\n/g,'\n');
 const start=experience.indexOf('async function loadRoundFollowups(');vm.runInContext('let followupEpoch=0;\n'+experience.slice(start,experience.indexOf('\n}',start)+2),scope);
 return{scope,calls,draft,controls};
}
test('unknown or absent auxiliary capability prevents suggestion and automatic followup I/O without changing drafts',async()=>{
 for(const enabled of [null,false]){
  const f=fixture(enabled);f.scope.syncSuggestionControl();
  assert.equal(f.scope.dom.suggest.disabled,true);assert.equal(f.controls.get('add-suggestion').disabled,true);
  await f.scope.requestSuggestion();await f.scope.loadRoundFollowups('completed-run');
  assert.deepEqual(f.calls,[]);assert.deepEqual(f.draft,{text:'keep my draft'});
 }
});
test('effective auxiliary availability restores execution independently of retained settings',()=>{
 const f=fixture(false);f.scope.syncSuggestionControl();
 assert.equal(f.scope.state.utilitySettings.suggestions_enabled,true);
 f.scope.state.workbench.builtin_availability['builtin.utility_model'].enabled=true;
 f.scope.syncSuggestionControl();assert.equal(f.scope.dom.suggest.disabled,false);assert.equal(f.controls.get('add-suggestion').disabled,false);
});
test('availability refresh clears pending auxiliary presentation while retaining settings and draft',()=>{
 const f=fixture(false);let invalidations=0;
 f.scope.invalidateSuggestion=()=>{invalidations++;f.scope.state.suggestion=null;};
 f.scope.state.suggestion={text:'old preview'};
 f.scope.syncUtilityAvailability();
 assert.equal(invalidations,1);assert.equal(f.scope.state.suggestion,null);
 assert.equal(f.scope.state.utilitySettings.suggestions_enabled,true);
 assert.deepEqual(f.draft,{text:'keep my draft'});
});
