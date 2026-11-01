const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function fixture(){
 let language='en';
 const el=(tag,cls,text)=>({tag,cls,text,children:[],dataset:{},append(...children){this.children.push(...children);}});
 const context=vm.createContext({el,CLATI18n:{msg:key=>language+':'+key,present:(message,fallback='')=>message?language+':'+message.key:fallback}});
 vm.runInContext(fs.readFileSync('web/plugin-market.js','utf8').replace(/\r\n/g,'\n'),context);
 return{context,language:next=>language=next};
}
function texts(node){return [typeof node.text==='function'?node.text():node.text||'',...node.children.flatMap(texts)].join('\n');}
const row=instance=>({id:'test.core',version:'1',name_presentation:{key:'test.name'},summary_presentation:{key:'test.summary'},instances:[{project:'project',phase:'active',...instance}]});
test('builtins render server protection reasons and preserve live locale switching',()=>{
 const f=fixture(),card=f.context.builtinPluginCards([row({configurable:false,required_by:[{service_id:'core.sandbox',reason_presentation:{key:'test.reason'}}]})]).get('test.core');
 assert.ok(texts(card).includes('en:test.reason'));
 f.language('zh');assert.ok(texts(card).includes('zh:test.reason'));
});
test('legacy protected instances retain a fallback and optional instances show no protection',()=>{
 const f=fixture();
 assert.ok(texts(f.context.builtinPluginCards([row({configurable:false})]).get('test.core')).includes('en:extension.required_contract'));
 const card=f.context.builtinPluginCards([row({configurable:true,required_by:[]})]).get('test.core');
 assert.ok(!texts(card).includes('en:extension.required_contract'));
});
