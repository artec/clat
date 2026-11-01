const {test}=require('node:test'),assert=require('node:assert/strict');
const {fixture,texts,row}=require('./support/builtin-cards.cjs');
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
