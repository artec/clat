// Owner-run release into a new, isolated v2 directory. Never writes the v1 site.
import {readFile,mkdir,writeFile,copyFile,stat} from 'node:fs/promises'
import {spawnSync} from 'node:child_process'
import {createHash} from 'node:crypto'
import path from 'node:path'
import {validateIndexGeneration} from './index-generation.mjs'
function option(name) {
 const at=process.argv.indexOf(name)
 if(at<0||!process.argv[at+1]) throw new Error(`missing ${name}`)
 return path.resolve(process.argv[at+1])
}
const source=option('--source'),artifacts=option('--artifacts'),destination=option('--out')
if(path.basename(destination)!=='v2') throw new Error('destination must be an isolated v2 directory')
const index=JSON.parse(await readFile(source,'utf8'))
validateIndexGeneration(index,2)
const now=Math.floor(Date.now()/1000)
index.market={...index.market,generatedAtUnix:now,expiresAtUnix:now+7*86400}
const copies=[]
for(const plugin of index.packages) for(const version of plugin.versions) for(const artifact of version.artifacts) {
 const from=path.join(artifacts,artifact.url),bytes=await readFile(from)
 if(bytes.length!==artifact.bytes||createHash('sha256').update(bytes).digest('hex')!==artifact.sha256)
  throw new Error(`artifact mismatch for ${plugin.id}`)
 if(!(await stat(from)).isFile()) throw new Error('artifact is not a file')
 copies.push([from,path.join(destination,artifact.url)])
}
// A new directory makes overwrite of an existing signed release impossible.
await mkdir(destination)
await mkdir(path.join(destination,'packages'))
for(const [from,to] of copies) await copyFile(from,to)
const file=path.join(destination,'index.json')
await writeFile(file,JSON.stringify(index)+'\n',{flag:'wx'})
const keyAt=process.argv.indexOf('--minisign-key')
if(keyAt>=0) {
 const key=option('--minisign-key')
 const result=spawnSync('minisign',['-S','-s',key,'-m',file,'-x',file+'.minisig','-t',`CLAT plugin index cn.at.pi generated ${now}`],{stdio:'inherit'})
 if(result.error) throw result.error
 if(result.status!==0) throw new Error('index signing failed')
} else console.log('Unsigned v2 proposal prepared. The owner must sign index.json before upload.')
