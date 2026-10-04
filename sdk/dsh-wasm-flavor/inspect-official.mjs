import {build} from '../dsh-wasm-spike/node_modules/esbuild/lib/main.js'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {explicitScope} from './explicit-scope.mjs'
import path from 'node:path'
import assert from 'node:assert/strict'
const root=path.resolve(import.meta.dirname,'../..'),out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const shim=path.join(root,'sdk/dsh-adapter/src/shim.ts'),before=await readFile(shim,'utf8')
const result=await build({entryPoints:[path.join(root,'sdk/dsh-wasm-spike/official-entry.mjs')],bundle:true,format:'esm',platform:'neutral',target:'es2022',external:['clat:plugin/*','node:*','undici'],write:false,metafile:true,conditions:['import','default'],mainFields:['module','main'],plugins:[{name:'second-flavor',setup(b){
 b.onLoad({filter:/\/dsh-adapter\/src\/shim\.ts$/},async()=>({contents:explicitScope(before).replace("context.cwd : process.cwd()","context.cwd : undefined"),loader:'ts',resolveDir:path.dirname(shim)}))
 b.onResolve({filter:/^\.\/host-services\.js$/},args=>args.importer===shim?{path:path.join(import.meta.dirname,'closed-host-services.mjs')}:undefined)
}}]})
await writeFile(path.join(out,'official.mjs'),result.outputFiles[0].text)
await writeFile(path.join(out,'metafile.json'),JSON.stringify(result.metafile,null,2)+'\n')
const imports=Object.values(result.metafile.outputs).flatMap(o=>o.imports).map(i=>i.path)
assert(!imports.includes('node:async_hooks'))
assert.equal(await readFile(shim,'utf8'),before)
await writeFile(path.join(out,'report.json'),JSON.stringify({imports,asyncLocalStorageAbsent:true,originalBunUnchanged:true,hostToolsPreopensDenied:true,componentizationComplete:false},null,2)+'\n')
console.log(JSON.stringify(imports))
