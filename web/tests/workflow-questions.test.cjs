const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),vm=require('node:vm');
const {fixture,walk,texts}=require('./support/builtin-cards.cjs');
function setup(){
 const f=fixture(),base=f.context.el,answers=[];
 f.context.el=(...args)=>{const node=base(...args);node.appendChild=child=>node.children.push(child);return node;};
 f.context.TextEncoder=TextEncoder;
 f.context.submitQuestionAnswer=(_card,answer)=>answers.push(JSON.parse(JSON.stringify(answer)));
 const source=fs.readFileSync('web/app.js','utf8').replace(/\r\n/g,'\n');
 vm.runInContext(source.slice(source.indexOf('function createQuestionCard('),source.indexOf('async function submitQuestionAnswer(')),f.context);
 return {...f,answers};
}
test('workflow confirmation uses translated presentation but submits the stable answer',()=>{
 const f=setup();
 const payload={rpc_id:'proposal',question:{question:'original diagnostic',options:[{label:'confirm'},{label:'decline'}],allow_custom:true,
  presentation:{question:{key:'workflow.model_request_plan',args:{detail:'原始方案'}},options:[{key:'workflow.model_confirm'},{key:'workflow.model_decline'}]}}};
 const original=JSON.stringify(payload);
 const card=f.context.createQuestionCard(payload);
 for(const language of ['en','zh-Hans','zh-Hant']){
  f.language(language);
  assert.ok(texts(card).includes(language+':workflow.model_request_plan'));
  assert.ok(texts(card).includes(language+':workflow.model_confirm'));
 }
 walk(card).find(node=>node.dataset.answerValue==='confirm').click();
 assert.deepEqual(f.answers,[{kind:'selected',value:'confirm'}]);
 assert.equal(JSON.stringify(payload),original);
});
test('legacy model questions keep original content and free text is a custom answer',()=>{
 const f=setup();
 const card=f.context.createQuestionCard({rpc_id:'clarify',question:{question:'Keep SQLite?',options:[{label:'Yes'}],allow_custom:true}});
 f.language('zh-Hant');
 assert.ok(texts(card).includes('Keep SQLite?'));
 const input=walk(card).find(node=>node.tag==='textarea');input.value='Keep existing schema';
 walk(card).find(node=>node.tag==='form').submit({preventDefault(){}});
 assert.deepEqual(f.answers,[{kind:'custom',value:'Keep existing schema'}]);
});
