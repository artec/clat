import test from 'node:test'
import assert from 'node:assert/strict'
import {mkdtemp,mkdir,writeFile,readFile,rm,access} from 'node:fs/promises'
import {spawnSync} from 'node:child_process'
import {createHash} from 'node:crypto'
import os from 'node:os'
import path from 'node:path'
const script=new URL('./release-network-index.mjs',import.meta.url)
test('v2 proposal preserves v1, validates bytes before writing, and refuses overwrite',async()=>{
 const root=await mkdtemp(path.join(os.tmpdir(),'clat-v2-release-test-'))
 try {
  const data=Buffer.from('signed package fixture'),artifacts=path.join(root,'artifacts'),source=path.join(root,'source.json'),out=path.join(root,'v2')
  await mkdir(path.join(artifacts,'packages'),{recursive:true})
  await writeFile(path.join(artifacts,'packages/net.clatpkg'),data)
  const legacy=path.join(root,'index.json');await writeFile(legacy,'unchanged v1 bytes\n')
  const index={schemaVersion:2,market:{id:'cn.at.pi'},packages:[{id:'io.artec.dsh-official-web-wasm',versions:[{
   manifestVersion:2,runtime:'wasm-component',capabilities:{network:{protocol:'clat:net-task@0.1.0',origins:[]}},
   artifacts:[{url:'packages/net.clatpkg',bytes:data.length,sha256:createHash('sha256').update(data).digest('hex')}],
  }]}]}
  const run=()=>spawnSync(process.execPath,[script.pathname,'--source',source,'--artifacts',artifacts,'--out',out],{encoding:'utf8'})
  await writeFile(source,JSON.stringify(index))
  await writeFile(path.join(artifacts,'packages/net.clatpkg'),'corrupted')
  assert.notEqual(run().status,0);await assert.rejects(access(out))
  await writeFile(path.join(artifacts,'packages/net.clatpkg'),data)
  const success=run();assert.equal(success.status,0,success.stderr)
  assert.equal(await readFile(legacy,'utf8'),'unchanged v1 bytes\n')
  assert.deepEqual(await readFile(path.join(out,'packages/net.clatpkg')),data)
  const before=await readFile(path.join(out,'index.json'),'utf8'),published=JSON.parse(before)
  assert.equal(published.market.expiresAtUnix-published.market.generatedAtUnix,7*86400)
  assert.notEqual(run().status,0)
  assert.equal(await readFile(path.join(out,'index.json'),'utf8'),before)
  await assert.rejects(access(path.join(out,'index.json.minisig')))
 }finally{await rm(root,{recursive:true,force:true})}
})
