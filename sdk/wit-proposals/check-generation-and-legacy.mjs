// Behavior red for the actual owner publisher and the actual legacy linker.
import assert from 'node:assert/strict'
import {cp,mkdir,mkdtemp,readFile,writeFile,rm} from 'node:fs/promises'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {spawnSync} from 'node:child_process'
const root=path.resolve(import.meta.dirname,'../..'),out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const temporary=await mkdtemp(path.join(tmpdir(),'clat-market-generation-red-'))
try{
 const market=path.join(temporary,'market')
 await cp(path.join(root,'market'),market,{recursive:true,filter:p=>!['dist','packages'].includes(path.basename(p))})
 const file=path.join(market,'scripts/index-generation.mjs'),original=await readFile(file,'utf8')
 const guard="assert(Object.keys(caps).every(key => legacyCapabilities.has(key)), 'v2 or unknown capabilities cannot enter v1')"
 assert.equal(original.split(guard).length-1,1);await writeFile(file,original.replace(guard,'// removed generation capability fence'))
 const result=spawnSync(process.execPath,['--test',path.join(market,'scripts/publication-fence.test.mjs')],{cwd:root,encoding:'utf8'})
 await writeFile(path.join(out,'23-publication-red.log'),result.stdout+result.stderr)
 assert.equal(result.status,1);assert.match(result.stdout,/AssertionError|assertion/);assert.match(result.stdout,/# fail 1/)
 await writeFile(path.join(out,'report.json'),JSON.stringify({id:'23-publication-fence',behaviorRed:true,productionIndexUnchanged:true},null,2)+'\n')
}finally{await rm(temporary,{recursive:true,force:true})}
