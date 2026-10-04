import {build} from '../dsh-wasm-spike/node_modules/esbuild/lib/main.js'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {scopeOracle} from './scope-oracle.mjs'
import WebRuntime from '../dsh-adapter/examples/official-web/node_modules/@deepseek-ai/dsh-web/lib/index.js'
import {explicitScope} from './explicit-scope.mjs'
import path from 'node:path'
import assert from 'node:assert/strict'
const out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const root=path.resolve(import.meta.dirname,'../..')
const file=path.join(root,'sdk/dsh-adapter/src/shim.ts'),original=await readFile(file,'utf8'),source=explicitScope(original)
const result=await build({stdin:{contents:source,loader:'ts',resolveDir:path.dirname(file)},bundle:true,format:'esm',platform:'node',write:false})
await writeFile(path.join(out,'shim.ts'),source)
const bundled=path.join(out,'shim.mjs');await writeFile(bundled,result.outputFiles[0].text)
const {Shim}=await import(bundled)
const oracle=await scopeOracle(Shim)
const shim=new Shim({sampling:async()=>{},elicitation:async()=>{},capabilities:{sampling:false,elicitation:false,hostServices:false},log:()=>{}},'late-scope')
let child,tools,prompt,on
const fiber=await shim.buildContext().inject('tools',ctx=>{child=ctx;tools=ctx.tools;prompt=ctx.systemPrompt;on=ctx.on;assert.equal(ctx.get('tools'),tools)})
await fiber.dispose()
for(const action of [()=>child.tools,()=>tools.register({}),()=>prompt.section({name:'late',order:1,text:'late'}),()=>on('late',()=>{})])assert.throws(action,/disposed/)
assert.equal(shim.listTools().length,0);assert.equal(shim.listPrompts().length,0)
await shim.disposeAll()
assert.equal(await readFile(file,'utf8'),original)
const providedShim=new Shim({sampling:async()=>{},elicitation:async()=>{},capabilities:{sampling:false,elicitation:false,hostServices:false},log:()=>{}},'provided-scope')
const providedRoot=providedShim.buildContext(),web=new WebRuntime(providedRoot,WebRuntime.Config({}))
const provider=id=>({id,available:()=>true,fetch:async()=>({})})
const register=id=>providedRoot.inject('web',async ctx=>{await Promise.resolve();ctx.web.registerFetchProvider(provider(id))})
const first=await register('first'),second=await register('second')
assert.equal(web.fetchProviders.size,2)
await first.dispose()
assert.deepEqual([...web.fetchProviders.keys()],['second'],'provided WebRuntime registration must belong to child scope')
await second.dispose();assert.equal(web.fetchProviders.size,0)
await providedShim.disposeAll()
await writeFile(path.join(out,'report.json'),JSON.stringify({oracle,providedWebRuntimeScopeVerified:true,lateCapturedScopeRejected:true,originalBunUnchanged:true,asyncLocalStorageAbsent:!source.includes('AsyncLocalStorage'),scope:'explicit flavor lifecycle in JS; componentization still required'},null,2)+'\n')
console.log(JSON.stringify(oracle))
