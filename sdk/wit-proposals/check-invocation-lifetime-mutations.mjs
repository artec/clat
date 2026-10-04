import {copyAuthor} from './author-copy.mjs'
// Independent author capability deletions; never write to the shared SDK.
import assert from 'node:assert/strict'
import {createHash, randomUUID} from 'node:crypto'
import {cp, mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawn} from 'node:child_process'
const here=import.meta.dirname, root=path.resolve(here,'../..')
const source=path.join(here,'http-consumer-host'), output=process.argv[2]
assert(output && path.isAbsolute(output),'pass an absolute fresh evidence directory')
await mkdir(output)
const file='network_resources/component/invocation.rs'
const mutations=[
 ['boundary-close',file,'let result = host.owner.close().map_err(super::trap);','let result = Ok(());','invocation_boundary_closes'],
 ['scope-revoke',file,'host.network.scope().invalidate();','// removed scope revoke','invocation_boundary_closes'],
 ['unwind-close',file,'let _ = self.close();','// removed unwind close','invocation_unwind_clears'],
 ['epoch-authority',file,'context.data_mut().owner.check().map_err(super::trap)?;','// removed epoch authority','invocation_run_revocation'],
 ['entry-authority',file,'boundary.0.data_mut().owner.check().map_err(super::trap)?;','// removed entry authority','invocation_rejects_revoked_entry'],
 ['return-authority',file,'let active = boundary.0.data_mut().owner.check().map_err(super::trap);','let active: wasmtime::Result<()> = Ok(());','invocation_return_cannot'],
 ['memory-count',file,'.memories(1)','','invocation_second_memory'],
 ['memory-limit',file,'.memory_size(256 * 1024 * 1024)','','invocation_memory_limit'],
]
const originals=new Map(await Promise.all([...new Set(mutations.map(m=>m[1]))].map(async f=>[f,(await readFile(path.join(source,'src',f),'utf8')).replaceAll('\r\n','\n')])))
const sha=s=>createHash('sha256').update(s).digest('hex')
const run=(manifest,filter)=>new Promise((resolve,reject)=>{
 const child=spawn('cargo',['test','--manifest-path',manifest,'--locked','--offline','--target-dir',path.join(root,'target'),'--lib',filter,'--','--nocapture'],{cwd:root})
 let log=''; child.stdout.on('data',b=>{log+=b}); child.stderr.on('data',b=>{log+=b});child.on('error',reject);child.on('close',code=>resolve({code,log}))
})
const temp=await mkdtemp(path.join(tmpdir(),'clat-invocation-deletions-')), copy=path.join(temp,'host'), evidence=[]
try {
 await copyAuthor(source,copy,root)
 await cp(path.join(here,'typed-task'),path.join(temp,'typed-task'),{recursive:true})
 const manifestFile=path.join(copy,'Cargo.toml'), lockFile=path.join(copy,'Cargo.lock')
 const name='clat-plg4-http-consumer-host', mutant=name+'-invocation-'+randomUUID().replaceAll('-','')
 await writeFile(manifestFile,(await readFile(manifestFile,'utf8')).replaceAll('path = "../../../crates/core"',`path = ${JSON.stringify(path.join(root,'crates/core'))}`).replace(`name = "${name}"`,`name = "${mutant}"`).replace(/^default-run = .*\n/m,''))
 await writeFile(lockFile,(await readFile(lockFile,'utf8')).replace(`name = "${name}"`,`name = "${mutant}"`))
 for(const [name,file,before,after,filter] of mutations) {
  const original=originals.get(file), destination=path.join(copy,'src',file)
  assert.equal(original.split(before).length-1,1,`unique guard ${name}`)
  await writeFile(destination,original.replace(before,after))
  const result=await run(manifestFile,filter)
  await writeFile(path.join(output,name+'.log'),result.log)
  await writeFile(destination,original)
  assert.equal(result.code,101,`${name}: deletion must be red`)
  assert.match(result.log,/panicked at/);assert.match(result.log,/test result: FAILED\./)
  assert.doesNotMatch(result.log,/error\[E\d+\]|could not compile/)
  evidence.push({deletion:name,filter,exit:result.code,sourceSha256:sha(original)})
  console.log('INVOCATION_DELETION '+JSON.stringify(evidence.at(-1)))
 }
 const green=await run(path.join(source,'Cargo.toml'),'')
 await writeFile(path.join(output,'canonical-green.log'),green.log)
 assert.equal(green.code,0);assert.match(green.log,/147 passed; 0 failed/)
 for(const [file,original] of originals) assert.equal((await readFile(path.join(source,'src',file),'utf8')).replaceAll('\r\n','\n'),original)
 await writeFile(path.join(output,'report.json'),JSON.stringify({deletions:evidence,canonicalPassed:147,fullD1AttackMatrixComplete:false,productionInvocationWired:false,systemDnsTested:false},null,2)+'\n')
} finally { await rm(temp,{recursive:true,force:true}) }
