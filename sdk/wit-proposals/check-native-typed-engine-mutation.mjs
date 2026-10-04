// Mutate only the isolated author engine checkout; preserve canonical SDK and artifacts.
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import { mkdir, readFile, writeFile, copyFile } from 'node:fs/promises'
import path from 'node:path'
assert.equal(process.argv.length,4,'usage: node check-native-typed-engine-mutation.mjs /absolute/new-directory cancel|preabort|transfer')
assert(path.isAbsolute(process.argv[2]))
const out=process.argv[2], name=process.argv[3], root=path.resolve(import.meta.dirname,'../..')
assert(['cancel','preabort','transfer'].includes(name))
await mkdir(out)
const file=path.join(root,'output/plg4/starlingmonkey-source/clat-native-task.cpp')
const original=await readFile(file,'utf8')
assert.equal(original,await readFile(path.join(import.meta.dirname,'typed-task/native-task.cpp'),'utf8'))
const before=name==='transfer' ? 'if (!handle(cx, args[0], true, &rep)) return RejectPromiseWithPendingError(cx, result);' : name==='cancel' ? 'return ENGINE->cancel_async_task(this);' : 'if (AbortSignal::is_aborted(signal)) return preabort(cx, signal, result);'
const after=name==='transfer' ? '// deleted final ownership validation' : name==='cancel' ? 'return true; // deleted physical cancel' : 'if (false) return preabort(cx, signal, result);'
assert.equal(original.split(before).length-1,name==='preabort'?4:1)
await writeFile(path.join(out,'native-original.cpp'),original)
const enginePath=path.join(root,'output/plg4/componentize-js-source/lib/starlingmonkey_embedding.wasm')
const env={...process.env,RUSTUP_TOOLCHAIN:'1.88.0',STARLINGMONKEY_SRC:path.join(root,'output/plg4/starlingmonkey-source')}
const build=()=>spawnSync(path.join(root,'output/plg4/cmake-tool/cmake/data/bin/cmake'),['--build',path.join(root,'output/plg4/engine-build'),'--target','starlingmonkey_embedding','-j4'],{env,encoding:'utf8',maxBuffer:16*1024*1024})
try {
  await writeFile(file,original.replace(before,after))
  const result=build()
  await writeFile(path.join(out,'engine-build.log'),result.stdout+result.stderr)
  assert.equal(result.status,0)
  await copyFile(enginePath,path.join(out,'engine.wasm'))
  execFileSync(process.execPath,[path.join(import.meta.dirname,'build-native-typed-task.mjs'),path.join(out,'engine.wasm'),path.join(out,'component')],{encoding:'utf8'})
} finally {
  await writeFile(file,original) // only the isolated author file, never git restore
}
console.log(JSON.stringify({mutation:name,component:path.join(out,'component/typed-task.wasm'),engineSourceRestored:true,buildCacheIsMutant:true}))
