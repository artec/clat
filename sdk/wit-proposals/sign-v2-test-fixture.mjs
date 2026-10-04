// Test keys are generated locally, used once, then erased. Never reads release private keys.
import {mkdir,readFile,writeFile,rm} from 'node:fs/promises'
import {spawnSync} from 'node:child_process'
import {createHash} from 'node:crypto'
import path from 'node:path'
import assert from 'node:assert/strict'
const out=process.argv[2];assert(out&&path.isAbsolute(out));await mkdir(out)
const run=args=>{const r=spawnSync('minisign',args,{encoding:'utf8'});assert.equal(r.status,0,r.stderr)}
const secret=path.join(out,'ephemeral-test.key')
try {
 run(['-G','-W','-s',secret,'-p',path.join(out,'test.pub')])
 const manifest={manifestVersion:2,id:'io.artec.network-wasm',version:'0.1.0',runtime:{kind:'wasm-component',entry:'plugin.wasm',sha256:createHash('sha256').update('component').digest('hex')},capabilities:{network:{protocol:'clat:net-task@0.1.0',origins:[{scheme:'https',host:'example.com',port:443,methods:['GET']}]},clock:{protocol:'wasi:clocks@0.2.10'},sampling:false,hostTools:[],preopens:[]}}
 await writeFile(path.join(out,'manifest.json'),JSON.stringify(manifest)+'\n')
 run(['-S','-s',secret,'-m',path.join(out,'manifest.json'),'-x',path.join(out,'manifest.minisig'),'-t','PLG4 isolated test manifest; not production'])
 assert((await readFile(path.join(out,'manifest.minisig'))).length>0)
} finally {await rm(secret,{force:true})}
console.log(JSON.stringify({output:out,testPrivateKeyErased:true,productionSigning:false}))
