import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'
assert([3,4].includes(process.argv.length), 'usage: node build-typed-task.mjs /absolute/new-directory [scheduler]')
assert(process.argv[3] === undefined || process.argv[3] === 'scheduler')
const scheduler = process.argv[3] === 'scheduler'
assert(path.isAbsolute(process.argv[2]))
const out = process.argv[2]
await mkdir(out)
const toolRoot = path.join(import.meta.dirname, '../dsh-wasm-spike/node_modules/@bytecodealliance')
const metadata = JSON.parse(await readFile(path.join(toolRoot, 'componentize-js/package.json'), 'utf8'))
assert.equal(metadata.version, '0.23.0')
const { component } = await componentize({
  sourcePath: path.join(import.meta.dirname, scheduler ? 'typed-task/scheduler.mjs' : 'typed-task/consumer.mjs'),
  witPath: path.join(import.meta.dirname, 'typed-task'), worldName: 'typed-client',
  disableFeatures: ['stdio', 'random', 'http', 'fetch-event', ...scheduler ? [] : ['clocks']], env: {},
})
await writeFile(path.join(out, 'typed-task.wasm'), component)
const jco = JSON.parse(await readFile(path.join(toolRoot, 'jco/package.json'), 'utf8'))
assert.equal(jco.version, '1.35.0')
const world = spawnSync(process.execPath, [path.join(toolRoot, 'jco', jco.bin.jco), 'wit', path.join(out, 'typed-task.wasm')], { encoding: 'utf8' })
assert.equal(world.status, 0, world.stderr)
assert.match(world.stdout, /import clat:net-task\/egress@0\.1\.0/)
assert.doesNotMatch(world.stdout, /import wasi:(http|sockets|filesystem|random)|\/environment@/)
await writeFile(path.join(out, 'world.wit'), world.stdout)
const sha = bytes => createHash('sha256').update(bytes).digest('hex')
await writeFile(path.join(out, 'build-report.json'), JSON.stringify({
  componentizeJs: metadata.version, componentSha256: sha(component), bytes: component.length,
  witSha256: sha(await readFile(path.join(import.meta.dirname, 'typed-task/net.wit'))),
  scope: scheduler ? 'native engine WASI timer and typed task polling; not native typed Promise bridge, AbortSignal or production acceptance' : 'typed resource submission/cancel consumer only; not async completion, AbortSignal or production acceptance',
}, null, 2) + '\n')
console.log(JSON.stringify({ output: out, bytes: component.length }))
