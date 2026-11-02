const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');

function presentation(){
  const source=fs.readFileSync('web/conversation-rounds.js','utf8');
  const scope={state:{sessionId:'session'}};
  vm.createContext(scope);
  vm.runInContext(source.slice(source.indexOf('function modelRequestDetail('),source.indexOf('const modelRequestPresentation=')),scope);
  return scope;
}

test('vendor presentation keeps custom and mismatched requests honest',()=>{
  const {modelRequestDetail:detail}=presentation();
  const event={provider:'OpenAI Compatible',model:'glm-5.3'};
  assert.equal(detail(event,{protocol:event.provider,model:event.model,vendor:'GLM'}),'GLM · glm-5.3');
  for(const route of [null,{protocol:event.provider,model:event.model,vendor:null},
    {protocol:event.provider,model:'other',vendor:'GLM'},
    {protocol:'other',model:event.model,vendor:'GLM'}]){
    assert.equal(detail(event,route),'OpenAI Compatible · glm-5.3');
  }
  assert.equal(event.provider,'OpenAI Compatible');
});

test('late run metadata labels pending notices and ignores the next-run model selection',()=>{
  const {createModelRequestPresentation}=presentation();
  const view=createModelRequestPresentation(),values=[];
  const event={provider:'OpenAI Compatible',model:'glm-5.3'};
  view.add(event,value=>values.push(value));
  assert.equal(values.at(-1),'OpenAI Compatible · glm-5.3');
  const info={session:{id:'session'},model:{vendor:'DeepSeek'},active_run:{
    prompt_rpc_id:'run-1',model:{protocol:event.provider,model:event.model,vendor:'GLM'},
  }};
  view.observe(info);
  assert.equal(values.at(-1),'GLM · glm-5.3');
  view.observe({...info,active_run:null});
  view.observe({...info,active_run:{...info.active_run,prompt_rpc_id:'different-run',model:{...info.active_run.model,vendor:'wrong'}}});
  view.observe({...info,session:{id:'different-session'}});
  assert.equal(values.at(-1),'GLM · glm-5.3');
  view.begin();
  view.add(event,value=>values.push(value));
  assert.equal(values.at(-1),'OpenAI Compatible · glm-5.3');
});
