// Consume original packages with an explicit, closed set of semantic adapters.
import assert from 'node:assert/strict'
import {readFile,mkdir,writeFile} from 'node:fs/promises'
import {createHash} from 'node:crypto'
import path from 'node:path'
import {build} from '../dsh-wasm-spike/node_modules/esbuild/lib/main.js'
import {componentize} from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'
import {lowerUnicodeProperties} from '../dsh-wasm-spike/regex-lowering.mjs'
import {explicitScope} from './explicit-scope.mjs'
const root=path.resolve(import.meta.dirname,'../..'),engine=path.resolve(process.argv[2]),out=process.argv[3],formal=process.argv[4]==='tools'
assert(out&&path.isAbsolute(out));
const engineSha=createHash('sha256').update(await readFile(engine)).digest('hex');
assert.equal(engineSha,'004c15137499ee782819af991a2403ada7ebedad6b7126dfd85babe5b611f29f','unapproved native engine');
assert.deepEqual(await readFile(path.join(root,'sdk/dsh-wasm-flavor/plugin-wit/deps/plugin.wit')),await readFile(path.join(root,'wit/plugin.wit')),'tools/config contract drift');
await mkdir(out)
const shim=path.join(root,'sdk/dsh-adapter/src/shim.ts'),original=await readFile(shim,'utf8')
let entry=`import {Shim} from ${JSON.stringify(shim)};
import {officialWeb} from ${JSON.stringify(path.join(root,'sdk/dsh-adapter/examples/official-web/plugin.mjs'))};
import {fetch as networkFetch} from ${JSON.stringify(path.join(import.meta.dirname,'network-fetch.mjs'))};
import {configureOrigins} from ${JSON.stringify(path.join(import.meta.dirname,'pinned-adapter.mjs'))};
import {installNativeSourceFormat} from ${JSON.stringify(path.join(import.meta.dirname,'native-source-format.mjs'))};
import {installAbortAny} from ${JSON.stringify(path.join(import.meta.dirname,'abort-any.mjs'))};
import {installUrlCanParse} from ${JSON.stringify(path.join(import.meta.dirname,'url-can-parse.mjs'))};
installNativeSourceFormat();installAbortAny();installUrlCanParse();
globalThis.fetch=networkFetch;
globalThis.process=Object.freeze({env:Object.freeze({}),platform:'wasm'});
export async function run(scenario) {
 const config=JSON.parse(scenario);configureOrigins(config.origins??[]);
 const controller=new AbortController();let timer;
 const unavailable=async()=>{throw new Error('capability-denied')};
 const shim=new Shim({sampling:unavailable,elicitation:unavailable,capabilities:{sampling:false,elicitation:false,hostServices:false},beginCall:()=>controller.signal,log(){}},'official-wasm');
 try {
  officialWeb.apply(shim.buildContext(),config.options??{});
  if(config.phase==='list')return JSON.stringify(shim.listTools().map(tool=>tool.name));
  if(config.phase==='scope') {
   const ctx=shim.buildContext(),provider=id=>({id,available:()=>true,fetch:async()=>({})});
   const inject=id=>ctx.inject('web',async child=>{await Promise.resolve();child.web.registerFetchProvider(provider(id))});
   const a=await inject('scope-a'),b=await inject('scope-b');
   await a.dispose();
   if(ctx.web.fetchProviders.has('scope-a')||!ctx.web.fetchProviders.has('scope-b'))throw new Error('provided scope ownership');
   await b.dispose();
   if(ctx.web.fetchProviders.has('scope-b')||!ctx.web.fetchProviders.has('http'))throw new Error('provided scope cleanup');
   return JSON.stringify({owned:true,rootProviderRetained:true});
  }

  if(config.abortMs>0)timer=setTimeout(()=>controller.abort(),config.abortMs);
  const result=await shim.callTool(config.name,config.arguments,'wasm-call');return JSON.stringify(result.structuredContent);
 } catch(error) {return JSON.stringify({failure:{name:error.name,message:error.message,stack:error.stack,cause:error.cause===undefined?undefined:String(error.cause?.message??error.cause)}})} finally {if(timer!==undefined)clearTimeout(timer);controller.abort();await shim.disposeAll()}
}`
if(formal) {
 entry+=`
import {get as pluginConfig} from 'clat:plugin/config@0.1.0';
 async function withPlugin(use) {
  const config=JSON.parse(pluginConfig());configureOrigins(config.origins??[]);
  const unavailable=async()=>{throw new Error('capability-denied')};
  const controller=new AbortController();
  const shim=new Shim({sampling:unavailable,elicitation:unavailable,capabilities:{sampling:false,elicitation:false,hostServices:false},beginCall:()=>controller.signal,log(){}},'official-wasm');
  try {officialWeb.apply(shim.buildContext(),config.options??{});return await use(shim)}
  finally {controller.abort();await shim.disposeAll()}
 }
 export const tools={
  async listTools(){return withPlugin(shim=>shim.listTools().map(tool=>({name:tool.name,description:tool.description??'',inputSchema:JSON.stringify(tool.parameters),effect:'network'})))},
  async call(name,args){return withPlugin(async shim=>{
   try {const result=await shim.callTool(name,JSON.parse(args),'wit-call');if(result.isError)throw result.content.map(item=>item.text??'').join('\\n');return JSON.stringify(result.structuredContent)}
   catch(error){throw typeof error==='string'?error:error.message}
  })}
 };
 `
 // The production tools contract is reused verbatim; author run is not exported.
 entry=entry.replace('export async function run(', 'async function run(')
}
const names={'node:dns/promises':'pinned-adapter.mjs','node:net':'ip-family.mjs','undici':'pinned-adapter.mjs','node:fs':'closed-platform.mjs','node:os':'closed-platform.mjs'}
const metadata=JSON.parse(await readFile(path.join(root,'sdk/dsh-adapter/examples/official-web/node_modules/@deepseek-ai/dsh-llm/package.json'),'utf8'))
const graph=await build({stdin:{contents:entry,resolveDir:import.meta.dirname},bundle:true,format:'esm',platform:'neutral',target:'es2022',write:false,metafile:true,external:formal?['clat:plugin/config@0.1.0']:[],define:{'import.meta.url':JSON.stringify('clat:component/official-web')},conditions:['import','default'],mainFields:['module','main'],plugins:[{name:'closed-capabilities',setup(b){
 b.onLoad({filter:/\/dsh-adapter\/src\/shim\.ts$/},()=>({contents:(`import {bindFetchProvider} from ${JSON.stringify(path.join(import.meta.dirname,'pinned-adapter.mjs'))};\n`+explicitScope(original).replace('const result = entry.apply(target, args)',`if (key === 'registerFetchProvider') args[0] = bindFetchProvider(args[0]);\n          const result = entry.apply(target, args)`)).replace('context.cwd : process.cwd()','context.cwd : undefined'),loader:'ts',resolveDir:path.dirname(shim)}))
 b.onResolve({filter:/^\.\/host-services\.js$/},args=>args.importer===shim?{path:path.join(import.meta.dirname,'closed-host-services.mjs')}:undefined)
 b.onResolve({filter:/^(node:|undici$)/},args=>{
  if(args.path==='node:module'){
   assert(args.importer.endsWith('/@deepseek-ai/dsh-llm/lib/index.js'),'unsupported: arbitrary module loading')
   return {path:'package-metadata',namespace:'static-metadata'}
  }
  assert(names[args.path],`unsupported platform capability: ${args.path}`)
  return {path:path.join(import.meta.dirname,names[args.path])}
 })
 b.onLoad({filter:/.*/,namespace:'static-metadata'},()=>({contents:`export function createRequire(){return specifier=>{if(specifier!=='../package.json')throw new Error('unsupported: module loading');return Object.freeze({version:${JSON.stringify(metadata.version)}})}}`,loader:'js'}))
}}]})
const lowered=lowerUnicodeProperties(graph.outputFiles[0].text),sourcePath=path.join(out,'official.mjs')
await writeFile(sourcePath,lowered.code);await writeFile(path.join(out,'metafile.json'),JSON.stringify(graph.metafile,null,2)+'\n')
const inputs=[]
for(const file of Object.keys(graph.metafile.inputs).filter(file=>file.includes('node_modules/@deepseek-ai/')))inputs.push({file,sha256:createHash('sha256').update(await readFile(path.resolve(root,file))).digest('hex')})
const {component}=await componentize({engine,sourcePath,witPath:path.join(root,formal?'sdk/dsh-wasm-flavor/plugin-wit':'sdk/wit-proposals/typed-task'),worldName:formal?'plugin-client':'typed-client',disableFeatures:['stdio','random','http','fetch-event'],env:{}})
await writeFile(path.join(out,'official.wasm'),component)
assert.equal(await readFile(shim,'utf8'),original)
for(const input of inputs)assert.equal(createHash('sha256').update(await readFile(path.resolve(root,input.file))).digest('hex'),input.sha256,'original package changed during build')
await writeFile(path.join(out,'build-report.json'),JSON.stringify({inputs,componentSha256:createHash('sha256').update(component).digest('hex'),engineSha256:engineSha,world:formal?'plugin-client':'typed-client',originalBunUnchanged:true,originalPackagesUnmodified:true,scope:'four original packages componentized; runtime acceptance requires real author invocation tests'},null,2)+'\n')
console.log(JSON.stringify({bytes:component.length}))
