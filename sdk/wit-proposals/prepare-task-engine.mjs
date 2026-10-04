// Only touches isolated pinned author checkouts, never node_modules or CLAT.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { access, mkdir, readFile, writeFile, copyFile } from 'node:fs/promises'
import path from 'node:path'

if (process.argv.length !== 5) throw new Error('usage: node prepare-task-engine.mjs STARLING_SOURCE COMPONENTIZE_SOURCE WIT_BINDGEN')
const [starling, embedding, bindgen] = process.argv.slice(2).map(value => path.resolve(value))
const repo = path.resolve(import.meta.dirname, '../..')
const run = (command, args, cwd) => execFileSync(command, args, { cwd, encoding: 'utf8' }).trim()

function pinned(root, revision, allowed) {
  assert.notEqual(run('git', ['rev-parse', '--show-toplevel'], root), repo)
  assert.equal(run('git', ['rev-parse', 'HEAD'], root), revision)
  const dirty = run('git', ['diff', 'HEAD', '--name-only'], root).split('\n').filter(Boolean)
  assert.ok(dirty.every(file => allowed.includes(file)), `unrecognized edits in ${root}`)
}

async function absent(file) {
  try { await access(file) } catch (error) {
    if (error.code === 'ENOENT') return
    throw error
  }
  throw new Error(`refusing to overwrite ${file}`)
}

// Verify every precondition before mutating either checkout.
pinned(starling, '9dda8ba7fcda2e17c6795d402f0478cf4c1f7f37', [
  'builtins/web/fetch/request-response.cpp', 'host-apis/wasi-0.2.0/handles.h',
  'host-apis/wasi-0.2.0/host_api.cpp', 'include/host_api.h',
])
pinned(embedding, '4d812f5a7b524cea5bcfd565cac55f3fab876a57', [])
assert.match(run(bindgen, ['--version']), /\b0\.52\.0\b/)
assert.match(run('rustup', ['run', '1.88.0', 'rustc', '--version']), /\b1\.88\.0\b/)
assert.match(run('rustup', ['target', 'list', '--installed', '--toolchain', '1.88.0']), /wasm32-wasip1/)
const wit = path.join(starling, 'host-apis/wasi-0.2.10/wit')
await absent(path.join(starling, 'clat-native-task.cpp'))
await absent(path.join(wit, 'deps/clat-net-task-probe/package.wit'))
const worldPath = path.join(wit, 'main.wit')
const world = await readFile(worldPath, 'utf8')
assert.equal(world.split('world bindings {').length, 2)
assert.ok(!world.includes('clat:net-task-probe'))
const cmakePath = path.join(embedding, 'CMakeLists.txt')
const cmake = await readFile(cmakePath, 'utf8')
const anchor = 'add_builtin(componentize::embedding SRC embedding/embedding.cpp)'
assert.equal(cmake.split(anchor).length, 2)
const initPath = path.join(starling, 'cmake/init-corrosion.cmake')
const init = await readFile(initPath, 'utf8')
const install = 'execute_process(COMMAND rustup toolchain install ${Rust_TOOLCHAIN})'
const target = 'execute_process(COMMAND rustup target add --toolchain ${Rust_TOOLCHAIN} wasm32-wasip1)'
assert.ok(init.includes(install) && init.includes(target))

await mkdir(path.join(wit, 'deps/clat-net-task-probe'), { recursive: true })
await copyFile(path.join(import.meta.dirname, 'task-consumer/probe.wit'),
  path.join(wit, 'deps/clat-net-task-probe/package.wit'))
await copyFile(path.join(import.meta.dirname, 'task-consumer/native-task.cpp'),
  path.join(starling, 'clat-native-task.cpp'))
await writeFile(worldPath, world.replace('world bindings {',
  'world bindings {\n  import clat:net-task-probe/tasks@0.1.0;'))
await writeFile(cmakePath, cmake.replace(anchor, anchor +
  '\nadd_builtin(clat::net_task_probe SRC "${STARLINGMONKEY_SRC}/clat-native-task.cpp")'))
await writeFile(initPath, init.replace(install,
  'execute_process(COMMAND rustup run ${Rust_TOOLCHAIN} rustc --version RESULT_VARIABLE CLAT_AUTHOR_TOOLCHAIN_STATUS)\n' +
  'if(NOT CLAT_AUTHOR_TOOLCHAIN_STATUS EQUAL 0)\n  message(FATAL_ERROR "Pre-provision the pinned author toolchain")\nendif()')
  .replace(target, '# CLAT author probe: target must already be provisioned; no global installer'))
run(bindgen, ['c', wit, '--world', 'bindings', '--out-dir',
  path.join(starling, 'host-apis/wasi-0.2.10/bindings')])
console.log(JSON.stringify({ scope: 'isolated native task scheduling only', authorToolchainAutoInstall: false }))
