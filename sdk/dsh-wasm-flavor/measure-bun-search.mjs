// Owner-armed real service comparison. Reads a private config file, never an env key.
import {readFile,writeFile,stat} from 'node:fs/promises'
import {performance} from 'node:perf_hooks'
import {Shim} from '../dsh-adapter/dist/src/index.js'
import {officialWeb} from '../dsh-adapter/examples/official-web/plugin.mjs'
const config=JSON.parse(await readFile(process.argv[2],'utf8'))
if(typeof config.apiKey!=='string'||!config.apiKey)throw new Error('private API key config required')
const samples=[]
for(let sample=0;sample<5;sample++){
 const controller=new AbortController(),unavailable=async()=>{throw new Error('unavailable')}
 const shim=new Shim({sampling:unavailable,elicitation:unavailable,capabilities:{sampling:false,elicitation:false,hostServices:false},beginCall:()=>controller.signal,log(){}},'original-bun-measurement')
 const start=performance.now()
 try {
  officialWeb.apply(shim.buildContext(),config)
  const result=await shim.callTool('web_search',{queries:['Rust programming language official website']},'measurement')
  if(result.isError)throw new Error('real provider search failed; diagnostics suppressed')
  const serialized=JSON.stringify(result.structuredContent)
  if(serialized.includes(config.apiKey))throw new Error('secret appeared in result')
  if(!serialized.includes('https://'))throw new Error('real search returned no source URLs')
  samples.push({sample,fuel:null,wallUs:Math.round((performance.now()-start)*1000),resultBytes:Buffer.byteLength(serialized)})
 }finally{controller.abort();await shim.disposeAll()}
}
const report={scope:'original unmodified official packages and Bun adapter; real DeepSeek search',runtime:Bun.version,samples}
if(process.argv[4])report.executableBytes=(await stat(process.argv[4])).size
await writeFile(process.argv[3],JSON.stringify(report)+'\n',{flag:'wx'})
console.log('Five real Bun searches completed; report contains measurements only.')
