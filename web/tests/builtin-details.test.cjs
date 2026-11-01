const {test}=require('node:test'),assert=require('node:assert/strict');
const {fixture,texts,walk,row}=require('./support/builtin-cards.cjs');
const sample=()=>row({configurable:true,required_by:[],disabled_by:[],provides:[],requires:['core.tools'],optional:[],tools:['apply_patch'],commands:[],extra:{literal:'原始<JSON>\n"value"'}});
test('readable table resolves service and tool names without rewriting raw copied metadata',async()=>{
 const f=fixture(),plugin=sample(),provider={...row({provides:['core.tools']}),id:'registry',name_presentation:{key:'registry.name'}};
 const card=f.context.builtinPluginCards([plugin,provider]).get(plugin.id);
 const table=walk(card).find(node=>node.tag==='table');assert.ok(table,'technical details should offer readable information');
 assert.ok(texts(table).includes('en:registry.name'));assert.ok(texts(table).includes('en:tool:apply_patch'));
 const copy=walk(card).find(node=>node.dataset.action==='copy-builtin-json');assert.ok(copy);
 await copy.click();await new Promise(setImmediate);
 assert.deepEqual(JSON.parse(f.copies[0]),{id:plugin.id,version:plugin.version,runtime:plugin.runtime,scope:plugin.scope,...plugin.instances[0]});
 f.language('zh');assert.ok(texts(table).includes('zh:registry.name'));
});
test('clipboard failure preserves the source and allows retry',async()=>{
 const f=fixture(),plugin=sample(),before=JSON.stringify(plugin);f.failCopy();
 const card=f.context.builtinPluginCards([plugin]).get(plugin.id),copy=walk(card).find(node=>node.dataset.action==='copy-builtin-json');
 assert.ok(copy);await copy.click();await new Promise(setImmediate);assert.equal(f.copies.length,0);assert.equal(JSON.stringify(plugin),before);
 assert.equal(copy.disabled,true);assert.ok(copy.textContent.includes('web.copy_failed'));
 f.language('zh');f.timers.forEach(fn=>fn());assert.equal(copy.disabled,false);assert.ok(copy.textContent.startsWith('zh:'));
});
test('unknown dependencies stay readable while original identifiers remain available',async()=>{
 const f=fixture(),plugin=sample();plugin.instances[0].requires=['unknown.service'];plugin.instances[0].disabled_by=['unknown.plugin'];
 const card=f.context.builtinPluginCards([plugin]).get(plugin.id),table=walk(card).find(node=>node.tag==='table');
 assert.ok(texts(table).includes('extension.detail_other_service'));assert.ok(texts(table).includes('extension.detail_other_plugin'));
 assert.equal(texts(table).includes('unknown.service'),false);assert.equal(texts(table).includes('unknown.plugin'),false);
 await walk(card).find(node=>node.dataset.action==='copy-builtin-json').click();await new Promise(setImmediate);
 assert.deepEqual(JSON.parse(f.copies[0]).requires,['unknown.service']);assert.deepEqual(JSON.parse(f.copies[0]).disabled_by,['unknown.plugin']);
});
test('existing copy buttons still accept literal labels and source text',async()=>{
 const f=fixture(),label='user supplied label',source='exact text\nwith punctuation < >';
 const button=f.context.copyTextButton(label,()=>source);await button.click();await new Promise(setImmediate);
 assert.equal(f.copies[0],source);f.timers.forEach(fn=>fn());assert.equal(button.textContent,label);assert.equal(button.disabled,false);
});
