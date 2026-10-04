// Exercise the production linker in an isolated checkout, never mutate the live tree.
import assert from 'node:assert/strict'
import {cp,mkdir,mkdtemp,readFile,writeFile,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawnSync} from 'node:child_process'
const root=path.resolve(import.meta.dirname,'../..'),out=process.argv[2]
assert(out&&path.isAbsolute(out));await mkdir(out)
const copy=await mkdtemp(path.join(tmpdir(),'clat-legacy-linker-red-'))
try {
 const inventory=spawnSync('git',['ls-files','--cached','--others','--exclude-standard','-z'],{cwd:root,encoding:'utf8'})
 assert.equal(inventory.status,0)
 for(const file of new Set(inventory.stdout.split('\0').filter(Boolean))) {
  const target=path.join(copy,file);await mkdir(path.dirname(target),{recursive:true})
  await cp(path.join(root,file),target)
 }
 const file=path.join(copy,'src/plugins/wasm/mod.rs'),original=await readFile(file,'utf8')
 const point='fn legacy_linker(engine: &Engine) -> Result<Linker<PluginState>, PluginError> {\n    let mut linker: Linker<PluginState> = Linker::new(engine);'
 assert.equal(original.split(point).length-1,1)
 const fault='\n    linker.instance("clat:net-task/egress@0.1.0").unwrap().func_wrap("dns-start", |_state: wasmtime::StoreContextMut<PluginState>, (x,): (u32,)| Ok((x,))).unwrap();'
 await writeFile(file,original.replace(point,point+fault))
 const result=spawnSync('cargo',['test','-p','clat-core','--locked','--offline','--target-dir',path.join(root,'target'),'--lib','plg4_legacy_linker_does_not_register_network_candidate','--','--nocapture'],{cwd:copy,encoding:'utf8',maxBuffer:16*1024*1024})
 const log=result.stdout+result.stderr;await writeFile(path.join(out,'22-legacy-linker.log'),log)
 assert.equal(result.status,101);assert.match(log,/panicked at/);assert.match(log,/called `Result::unwrap_err\(\)` on an `Ok` value/);assert.doesNotMatch(log,/could not compile|error\[E\d+\]/)
 await writeFile(path.join(out,'report.json'),JSON.stringify({id:'22-legacy-linker',behaviorRed:true,actualProductionFactory:true},null,2)+'\n')
} finally {await rm(copy,{recursive:true,force:true})}
