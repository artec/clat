// Specific guards must fail compiled behavioral assertions in an isolated copy.
import assert from 'node:assert/strict'
import {cp,mkdir,mkdtemp,readFile,writeFile,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawnSync} from 'node:child_process'
const out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const copy=await mkdtemp(path.join(tmpdir(),'clat-flavor-red-'))
try {
 await cp(import.meta.dirname,copy,{recursive:true})
 const rows=[['native-method-case','network-fetch.mjs',".toUpperCase()",".toLowerCase()",'network-fetch.test.mjs'],['provider-deadline-signal','pinned-adapter.mjs',"if(key==='resolveAddresses')return","if(false)return",'pinned-adapter.test.mjs']]
 const evidence=[]
 for(const [id,file,before,after,test] of rows) {
  const target=path.join(copy,file),source=await readFile(target,'utf8');assert.equal(source.split(before).length-1,1)
  await writeFile(target,source.replace(before,after))
  const result=spawnSync(process.execPath,['--test',path.join(copy,test)],{encoding:'utf8'})
  const log=result.stdout+result.stderr;await writeFile(path.join(out,id+'.log'),log)
  await writeFile(target,source)
  assert.equal(result.status,1);assert.match(log,/AssertionError|assertion/);assert.match(log,/# fail 1/)
  evidence.push({id,behaviorRed:true})
 }
 await writeFile(path.join(out,'report.json'),JSON.stringify({rows:evidence,productionSourceMutated:false},null,2)+'\n')
} finally {await rm(copy,{recursive:true,force:true})}
