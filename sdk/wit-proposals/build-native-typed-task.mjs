// Author-only native typed net bridge, using a pinned isolated engine.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'
assert.equal(process.argv.length,4,'usage: node build-native-typed-task.mjs ENGINE /absolute/new-directory')
assert(path.isAbsolute(process.argv[3]))
const engine = path.resolve(process.argv[2]), out=process.argv[3]
await mkdir(out)
for (const file of ['native-task.cpp','native-consumer.mjs','net.wit']) {
  await writeFile(path.join(out,file),await readFile(path.join(import.meta.dirname,'typed-task',file)))
}
const root = path.join(import.meta.dirname,'../dsh-wasm-spike/node_modules/@bytecodealliance')
assert.equal(JSON.parse(await readFile(path.join(root,'componentize-js/package.json'),'utf8')).version,'0.23.0')
const {component} = await componentize({
  engine, sourcePath:path.join(import.meta.dirname,'typed-task/native-consumer.mjs'),
  witPath:path.join(import.meta.dirname,'typed-task'),worldName:'typed-client',
  disableFeatures:['stdio','random','http','fetch-event'],env:{},
})
await writeFile(path.join(out,'typed-task.wasm'),component)
const jco=JSON.parse(await readFile(path.join(root,'jco/package.json'),'utf8'))
assert.equal(jco.version,'1.35.0')
const world=spawnSync(process.execPath,[path.join(root,'jco',jco.bin.jco),'wit',path.join(out,'typed-task.wasm')],{encoding:'utf8'})
assert.equal(world.status,0,world.stderr)
assert.match(world.stdout,/import clat:net-task\/egress@0\.1\.0/)
assert.doesNotMatch(world.stdout,/clat:net-task-probe|import wasi:(http|sockets|filesystem|random)|\/environment@/)
await writeFile(path.join(out,'world.wit'),world.stdout)
const sha=bytes=>createHash('sha256').update(bytes).digest('hex')
await writeFile(path.join(out,'build-report.json'),JSON.stringify({engineSha256:sha(await readFile(engine)),componentSha256:sha(component),nativeSourceSha256:sha(await readFile(path.join(import.meta.dirname,'typed-task/native-task.cpp'))),witSha256:sha(await readFile(path.join(import.meta.dirname,'typed-task/net.wit'))),consumerSourceSha256:sha(await readFile(path.join(import.meta.dirname,'typed-task/native-consumer.mjs'))),componentizeJs:'0.23.0',jco:'1.35.0',scope:'native typed network Promise/AbortSignal author consumer; local transport uses private fixture only, not DNS policy, production or quartet acceptance'},null,2)+'\n')
console.log(JSON.stringify({output:out,bytes:component.length}))
