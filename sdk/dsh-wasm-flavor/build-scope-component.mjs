// Real component proof of the explicit-scope lifecycle, before network mapping.
import assert from 'node:assert/strict'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {createHash} from 'node:crypto'
import path from 'node:path'
import {build} from '../dsh-wasm-spike/node_modules/esbuild/lib/main.js'
import {componentize} from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'
import {explicitScope} from './explicit-scope.mjs'
const root=path.resolve(import.meta.dirname,'../..'),engine=path.resolve(process.argv[2]),out=process.argv[3]
assert(out&&path.isAbsolute(out));await mkdir(out)
const shim=path.join(root,'sdk/dsh-adapter/src/shim.ts'),original=await readFile(shim,'utf8')
const oracle=(await readFile(path.join(import.meta.dirname,'scope-oracle.mjs'),'utf8')).replace("import assert from 'node:assert/strict'",`const assert = {
 equal(a,b,message) { if(a!==b) throw new Error(message || 'equality failed') },
 deepEqual(a,b,message) { if(JSON.stringify(a)!==JSON.stringify(b)) throw new Error(message || 'trace differs') },
 async rejects(promise,pattern) { try { await promise } catch(error) { if(pattern.test(error.message)) return; throw error } throw new Error('expected cleanup rejection') }
}`)
const input=`import {Shim} from ${JSON.stringify(shim)};\n${oracle}\nexport async function run() { return JSON.stringify(await scopeOracle(Shim)) }`
const bundled=await build({stdin:{contents:input,resolveDir:import.meta.dirname},bundle:true,format:'esm',platform:'neutral',target:'es2022',write:false,plugins:[{name:'closed-scope',setup(b){
 b.onLoad({filter:/\/dsh-adapter\/src\/shim\.ts$/},()=>({contents:explicitScope(original).replace('context.cwd : process.cwd()','context.cwd : undefined'),loader:'ts',resolveDir:path.dirname(shim)}))
 b.onResolve({filter:/^\.\/host-services\.js$/},args=>args.importer===shim?{path:path.join(import.meta.dirname,'closed-host-services.mjs')}:undefined)
}}]})
const sourcePath=path.join(out,'scope.mjs');await writeFile(sourcePath,bundled.outputFiles[0].text)
const {component}=await componentize({engine,sourcePath,witPath:path.join(root,'sdk/wit-proposals/typed-task'),worldName:'typed-client',disableFeatures:['stdio','random','http','fetch-event'],env:{}})
await writeFile(path.join(out,'scope.wasm'),component)
assert.equal(await readFile(shim,'utf8'),original)
const sha=bytes=>createHash('sha256').update(bytes).digest('hex')
await writeFile(path.join(out,'build-report.json'),JSON.stringify({engineSha256:sha(await readFile(engine)),componentSha256:sha(component),sourceSha256:sha(bundled.outputFiles[0].text),originalBunUnchanged:true,scope:'actual component lifecycle oracle, not quartet acceptance'},null,2)+'\n')
console.log(JSON.stringify({bytes:component.length}))
