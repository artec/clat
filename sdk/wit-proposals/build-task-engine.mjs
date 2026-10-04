// Native DNS/HTTP semantic-task scheduling probe; no real network or shim v2.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'

if (process.argv.length !== 4) throw new Error('usage: node build-task-engine.mjs ENGINE /absolute/new-directory')
const out = path.resolve(process.argv[3])
await mkdir(out)
const engine = path.resolve(process.argv[2])
const metadata = JSON.parse(await readFile(new URL(
  '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/package.json', import.meta.url), 'utf8'))
assert.equal(metadata.version, '0.23.0')
const { component } = await componentize({
  engine, sourcePath: path.join(import.meta.dirname, 'task-consumer/consumer.mjs'),
  witPath: path.join(import.meta.dirname, 'task-consumer'), worldName: 'consumer',
  env: {}, disableFeatures: ['stdio', 'random', 'http', 'fetch-event'],
})
await writeFile(path.join(out, 'task-consumer.wasm'), component)
const jcoRoot = path.join(import.meta.dirname, '../dsh-wasm-spike/node_modules/@bytecodealliance/jco')
const jco = JSON.parse(await readFile(path.join(jcoRoot, 'package.json'), 'utf8'))
assert.equal(jco.version, '1.35.0')
const world = spawnSync(process.execPath, [path.join(jcoRoot, jco.bin.jco), 'wit',
  path.join(out, 'task-consumer.wasm')], { encoding: 'utf8' })
assert.equal(world.status, 0, world.stderr)
assert.match(world.stdout, /import clat:net-task-probe\/tasks@0\.1\.0/)
assert.doesNotMatch(world.stdout, /import wasi:(http|sockets|filesystem|random)|\/environment@/)
await writeFile(path.join(out, 'world.wit'), world.stdout)
const sha = bytes => createHash('sha256').update(bytes).digest('hex')
await writeFile(path.join(out, 'build-report.json'), JSON.stringify({
  componentizeJs: metadata.version, engineSha256: sha(await readFile(engine)),
  componentSha256: sha(component), bytes: component.length,
  expectedNativeSourceSha256: sha(await readFile(path.join(import.meta.dirname, 'task-consumer/native-task.cpp'))),
  probeWitSha256: sha(await readFile(path.join(import.meta.dirname, 'task-consumer/probe.wit'))),
  scope: 'native semantic task scheduling only; no DNS/HTTP transport or authority acceptance',
}, null, 2) + '\n')
