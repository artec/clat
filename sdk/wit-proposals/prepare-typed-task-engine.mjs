// Transition only a recognized isolated task-probe checkout; snapshot every replaced input.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { cp, mkdir, readFile, writeFile, copyFile } from 'node:fs/promises'
import path from 'node:path'
assert.equal(process.argv.length, 6, 'usage: node prepare-typed-task-engine.mjs STARLING COMPONENTIZE WIT_BINDGEN /absolute/new-snapshot')
const [starling, embedding, bindgen, snapshot] = process.argv.slice(2).map(p => path.resolve(p))
const repo = path.resolve(import.meta.dirname, '../..')
const run = (command, args, cwd) => execFileSync(command, args, {cwd, encoding:'utf8'}).trim()
for (const [root, revision] of [[starling,'9dda8ba7fcda2e17c6795d402f0478cf4c1f7f37'],[embedding,'4d812f5a7b524cea5bcfd565cac55f3fab876a57']]) {
  assert.notEqual(run('git',['rev-parse','--show-toplevel'],root),repo)
  assert.equal(run('git',['rev-parse','HEAD'],root),revision)
}
assert.match(run(bindgen,['--version']), /\b0\.52\.0\b/)
assert.match(run('rustup',['run','1.88.0','rustc','--version']), /\b1\.88\.0\b/)
const wit = path.join(starling,'host-apis/wasi-0.2.10/wit')
const worldFile = path.join(wit,'main.wit'), cmakeFile = path.join(embedding,'CMakeLists.txt')
const world = await readFile(worldFile,'utf8'), cmake = await readFile(cmakeFile,'utf8')
const importAnchor = 'import clat:net-task-probe/tasks@0.1.0;'
const builtinAnchor = 'add_builtin(clat::net_task_probe SRC "${STARLINGMONKEY_SRC}/clat-native-task.cpp")'
assert.equal(world.split(importAnchor).length,2)
assert.equal(cmake.split(builtinAnchor).length,2)
assert.equal(await readFile(path.join(starling,'clat-native-task.cpp'),'utf8'),await readFile(path.join(import.meta.dirname,'task-consumer/native-task.cpp'),'utf8'))
const allowed = ['builtins/web/fetch/request-response.cpp','cmake/init-corrosion.cmake','host-apis/wasi-0.2.0/handles.h','host-apis/wasi-0.2.0/host_api.cpp','host-apis/wasi-0.2.10/bindings/bindings.c','host-apis/wasi-0.2.10/bindings/bindings.h','host-apis/wasi-0.2.10/bindings/bindings_component_type.o','host-apis/wasi-0.2.10/wit/main.wit','include/host_api.h']
assert(run('git',['diff','HEAD','--name-only'],starling).split('\n').filter(Boolean).every(p=>allowed.includes(p)))
assert.equal(run('git',['diff','HEAD','--name-only'],embedding),'CMakeLists.txt')
assert(!(await readFile(path.join(starling,'cmake/init-corrosion.cmake'),'utf8')).includes('execute_process(COMMAND rustup toolchain install'))
await mkdir(snapshot)
await cp(wit,path.join(snapshot,'wit'),{recursive:true})
await cp(path.join(starling,'host-apis/wasi-0.2.10/bindings'),path.join(snapshot,'bindings'),{recursive:true})
await copyFile(path.join(starling,'clat-native-task.cpp'),path.join(snapshot,'native-task.cpp'))
await copyFile(cmakeFile,path.join(snapshot,'CMakeLists.txt'))
await mkdir(path.join(wit,'deps/clat-net-task'))
await copyFile(path.join(import.meta.dirname,'typed-task/net.wit'),path.join(wit,'deps/clat-net-task/package.wit'))
await copyFile(path.join(import.meta.dirname,'typed-task/native-task.cpp'),path.join(starling,'clat-native-task.cpp'))
await writeFile(worldFile,world.replace(importAnchor,'import clat:net-task/egress@0.1.0;'))
await writeFile(cmakeFile,cmake.replace(builtinAnchor,'add_builtin(clat::net_task SRC "${STARLINGMONKEY_SRC}/clat-native-task.cpp")'))
run(bindgen,['c',wit,'--world','bindings','--out-dir',path.join(starling,'host-apis/wasi-0.2.10/bindings')])
console.log(JSON.stringify({snapshot,scope:'isolated typed AsyncTask engine; production unchanged'}))
