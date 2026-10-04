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
const caps='capabilities.rs', clock='network_resources/clocks.rs'
const mutations=[
 ['version',caps,'declared.manifest_version != 2','false','capability_descriptor_is_versioned'],
 ['dangerous-combinations',caps,'if !caps.host_tools.is_empty()\n            || !caps.preopens.is_empty()\n            || !config.host_tools.is_empty()\n            || !config.preopens.is_empty()','if false','capability_network_rejects_all_process'],
 ['network-protocol',caps,'network.protocol != "clat:net-task@0.1.0"','false','capability_network_requires_declaration'],
 ['unknown-capabilities',caps,'#[serde(default, rename_all = "camelCase", deny_unknown_fields)]\nstruct Capabilities','#[serde(default, rename_all = "camelCase")]\nstruct Capabilities','capability_descriptor_is_versioned'],
 ['dns-config',caps,'config_origins.as_deref()','None','capability_config_only_intersects'],
 ['http-config',caps,'config_declarations.as_deref()','None','capability_config_only_intersects'],
 ['clock-escalation',caps,'config == Some(true) && declared.is_none()','false','capability_clock_is_independent'],
 ['clock-config-disable',caps,'declared.is_some() && config != Some(false)','declared.is_some()','capability_clock_is_independent'],
 ['clock-protocol',caps,'c.protocol != "wasi:clocks@0.2.10"','false','capability_clock_is_independent'],
 ['sampling-label',caps,'if self.sampling {','if false {','capability_sampling_label'],
 ['clock-grant',clock,'if self.clock_grant.is_none() {','if false {','capability_clock_'],
 ['clock-now',clock,'self.require_clock().map_err(trap)?;\n        monotonic_clock::Host::now','// removed capability check\n        monotonic_clock::Host::now','capability_clock_'],
 ['clock-resolution',clock,'self.require_clock().map_err(trap)?;\n        monotonic_clock::Host::resolution','// removed capability check\n        monotonic_clock::Host::resolution','capability_clock_reads'],
 ['wall-now',clock,'self.require_clock().map_err(trap)?;\n        wall_clock::Host::now','// removed capability check\n        wall_clock::Host::now','capability_clock_reads'],
 ['wall-resolution',clock,'self.require_clock().map_err(trap)?;\n        wall_clock::Host::resolution','// removed capability check\n        wall_clock::Host::resolution','capability_clock_reads'],
 ['clock-subscription',clock,'self.require_clock()?;\n        let root = self.insert(Timer {','let root = self.insert(Timer {','capability_clock_subscription'],
]
const originals=new Map(await Promise.all([...new Set(mutations.map(m=>m[1]))].map(async f=>[f,(await readFile(path.join(source,'src',f),'utf8')).replaceAll('\r\n','\n')])))
const sha=s=>createHash('sha256').update(s).digest('hex')
const run=(manifest,filter)=>new Promise((resolve,reject)=>{
 const child=spawn('cargo',['test','--manifest-path',manifest,'--locked','--offline','--target-dir',path.join(root,'target'),'--lib',filter,'--','--nocapture'],{cwd:root})
 let log=''; child.stdout.on('data',b=>{log+=b}); child.stderr.on('data',b=>{log+=b});child.on('error',reject);child.on('close',code=>resolve({code,log}))
})
const temp=await mkdtemp(path.join(tmpdir(),'clat-capability-deletions-')), copy=path.join(temp,'host'), evidence=[]
try {
 await copyAuthor(source,copy,root)
 await cp(path.join(here,'typed-task'),path.join(temp,'typed-task'),{recursive:true})
 const manifestFile=path.join(copy,'Cargo.toml'), lockFile=path.join(copy,'Cargo.lock')
 const name='clat-plg4-http-consumer-host', mutant=name+'-cap-'+randomUUID().replaceAll('-','')
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
  console.log('CAPABILITY_DELETION '+JSON.stringify(evidence.at(-1)))
 }
 const green=await run(path.join(source,'Cargo.toml'),'')
 await writeFile(path.join(output,'canonical-green.log'),green.log)
 assert.equal(green.code,0);assert.match(green.log,/147 passed; 0 failed/)
 for(const [file,original] of originals) assert.equal((await readFile(path.join(source,'src',file),'utf8')).replaceAll('\r\n','\n'),original)
 await writeFile(path.join(output,'report.json'),JSON.stringify({deletions:evidence,canonicalPassed:147,fullD1AttackMatrixComplete:false,productionManifestOrSignatureVerified:false},null,2)+'\n')
} finally { await rm(temp,{recursive:true,force:true}) }
