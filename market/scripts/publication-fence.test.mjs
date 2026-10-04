import test from 'node:test'
import assert from 'node:assert/strict'
import {mkdtemp,cp,readFile,writeFile,stat,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawnSync} from 'node:child_process'
test('actual v1 publisher rejects mixed generation before writing its index',async()=>{
 const temp=await mkdtemp(path.join(tmpdir(),'clat-index-fence-'))
 try {
  const root=path.join(temp,'market');await cp(path.resolve(import.meta.dirname,'..'),root,{recursive:true,filter:p=>!['dist','packages'].includes(path.basename(p))})
  const file=path.join(root,'index.source.json'),index=JSON.parse(await readFile(file,'utf8'))
  index.packages[0].versions[0].capabilities.network={protocol:'clat:net-task@0.1.0',origins:[]}
  await writeFile(file,JSON.stringify(index))
  const result=spawnSync(process.execPath,[path.join(root,'scripts/release-index.mjs'),'--minisign-key',path.join(temp,'missing.key')],{encoding:'utf8'})
  assert.notEqual(result.status,0);assert.match(result.stderr,/index generation: v2 or unknown/)
  await assert.rejects(stat(path.join(root,'dist/index.json')),{code:'ENOENT'})
 } finally {await rm(temp,{recursive:true,force:true})}
})
