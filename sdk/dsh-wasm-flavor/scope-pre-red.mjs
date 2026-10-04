import {build} from '../dsh-wasm-spike/node_modules/esbuild/lib/main.js'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {scopeOracle} from './scope-oracle.mjs'
import path from 'node:path'
import assert from 'node:assert/strict'
const out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const root=path.resolve(import.meta.dirname,'../..')
const original=await readFile(path.join(root,'sdk/dsh-adapter/src/shim.ts'),'utf8')
const point='this.#cleanupScope.run(ownedCleanups, () => callback(this.#context as DshContext))'
assert.equal(original.split(point).length-1,1)
async function load(source,name) {
 const result=await build({stdin:{contents:source,loader:'ts',resolveDir:path.join(root,'sdk/dsh-adapter/src')},bundle:true,format:'esm',platform:'node',write:false})
 const file=path.join(out,name+'.mjs');await writeFile(file,result.outputFiles[0].text)
 return (await import(file)).Shim
}
const canonical=await scopeOracle(await load(original,'canonical'))
let red
try {await scopeOracle(await load(original.replace(point,'callback(this.#context as DshContext)'),'unscoped'))}
catch(error){red={message:error.message,code:error.code}}
assert(red&&red.code==='ERR_ASSERTION','removing async scope must produce a behavior failure')
await writeFile(path.join(out,'report.json'),JSON.stringify({canonical,red,originalBunSourceUnchanged:original===await readFile(path.join(root,'sdk/dsh-adapter/src/shim.ts'),'utf8'),scope:'original Shim actual lifecycle oracle; unscoped preimage red, not WASM or D2 completion'},null,2)+'\n')
console.log(JSON.stringify({canonical,red}))
