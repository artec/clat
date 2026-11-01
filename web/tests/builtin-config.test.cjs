const {test}=require('node:test'),assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
function fixture(){const c=vm.createContext({});vm.runInContext(fs.readFileSync('web/plugin-market.js','utf8').replace(/\r\n/g,'\n'),c);return c.createBuiltinConfigDraft();}
const view=(overrides={})=>({instance_id:'host-a',storage_root:'/root-a',revision:'r1',disabled:[],...overrides});
test('draft never saves or changes baseline and edits revoke previews',()=>{
 const d=fixture();d.receive(view());d.toggle('skills',false);assert.equal(d.dirty,true);assert.equal(d.base.disabled.length,0);
 assert.equal(d.accept({instance_id:'host-a',revision:'r1',disabled:['skills'],preview_id:'p'}),true);assert.ok(d.preview);
 d.toggle('memory',false);assert.equal(d.preview,null);d.discard();assert.equal(d.dirty,false);
});
test('concurrent saves and Host changes retain draft but require review',()=>{
 const d=fixture();d.receive(view());d.toggle('skills',false);d.receive(view({revision:'r2',disabled:['memory']}));
 assert.equal(d.dirty,true);assert.equal(d.review,true);assert.deepEqual([...d.draft],['skills']);d.acknowledge();assert.equal(d.review,false);
 d.receive(view({instance_id:'host-b',revision:'r2'}));assert.equal(d.review,true);assert.equal(d.preview,null);
});
test('different storage roots isolate drafts and stale preview identities are rejected',()=>{
 const d=fixture();d.receive(view());d.toggle('skills',false);
 assert.equal(d.accept({instance_id:'old-host',disabled:['skills']}),false);
 d.receive(view({storage_root:'/root-b',instance_id:'host-b',disabled:['memory']}));assert.equal(d.dirty,false);assert.deepEqual([...d.draft],['memory']);
});
test('refresh and reconnect invalidate preview without discarding pending choices',()=>{
 const d=fixture();d.receive(view());d.toggle('skills',false);d.accept({instance_id:'host-a',revision:'r1',disabled:['skills'],preview_id:'p'});
 d.invalidate();assert.equal(d.preview,null);assert.equal(d.review,true);d.receive(view());assert.equal(d.dirty,true);assert.deepEqual([...d.draft],['skills']);
});
