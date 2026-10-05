// Assemble only a pinned, already built search release. Does not sign or publish.
import {readFile,mkdir,writeFile,copyFile} from 'node:fs/promises'
import {createHash} from 'node:crypto'
import path from 'node:path'
import assert from 'node:assert/strict'
import {notices} from '../dsh-adapter/examples/official-web/notices.mjs'
const component=path.resolve(process.argv[2]),destination=path.resolve(process.argv[3])
const report=JSON.parse(await readFile(path.join(path.dirname(component),'build-report.json'),'utf8'))
assert.equal(report.world,'plugin-client');assert.equal(report.searchOnly,true)
const bytes=await readFile(component),sha256=createHash('sha256').update(bytes).digest('hex')
assert.equal(sha256,report.componentSha256)
await mkdir(destination)
await copyFile(component,path.join(destination,'official.wasm'))
const manifest={manifestVersion:2,id:'io.artec.dsh-official-web-wasm',name:'DSH official web search · WASM',version:'0.1.0',
 description:'Search-only sandbox. Network restricted to the declared DeepSeek API origin; use the MCP flavor for arbitrary URL fetching.',
 runtime:{kind:'wasm-component',entry:'official.wasm',sha256},
 capabilities:{tools:true,network:{protocol:'clat:net-task@0.1.0',origins:[{scheme:'https',host:'api.deepseek.com',port:443,methods:['POST']}]},clock:{protocol:'wasi:clocks@0.2.10'}},
 compatibility:{kind:'dsh-v0.2.0-rc.2',revision:'639ed015397290b3745d163aafe02ffee4aa3f84'},
 configSchema:{type:'object',additionalProperties:false,properties:{apiKey:{type:'string',title:'DeepSeek API key',writeOnly:true,description:'Saved only in private local plugin configuration.'},networkPolicy:{type:'object',title:'Optional narrower network policy'}}}}
await writeFile(path.join(destination,'clat-plugin.json'),JSON.stringify(manifest,null,2)+'\n',{flag:'wx'})
await writeFile(path.join(destination,'BUILD.json'),JSON.stringify(report,null,2)+'\n',{flag:'wx'})
await writeFile(path.join(destination,'LICENSES.txt'),await notices(path.resolve(import.meta.dirname,'../dsh-adapter/examples/official-web')),{flag:'wx'})
console.log(`Unsigned search-only package: ${destination}`)
