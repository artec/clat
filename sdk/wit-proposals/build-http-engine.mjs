// Author engine variant; never replaces the pinned production/PLG-3 toolchain.
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'

if (process.argv.length !== 4) throw new Error('usage: node build-http-engine.mjs ENGINE /absolute/new-directory')
const out = path.resolve(process.argv[3])
await mkdir(out)
const metadata = JSON.parse(await readFile(new URL(
  '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/package.json', import.meta.url), 'utf8'))
assert.equal(metadata.version, '0.23.0')
const { component } = await componentize({
  engine: path.resolve(process.argv[2]),
  sourcePath: path.join(import.meta.dirname, 'http-consumer/consumer.mjs'),
  witPath: path.join(import.meta.dirname, 'http-consumer'), worldName: 'consumer',
  env: {}, disableFeatures: ['stdio', 'random', 'fetch-event'],
})
await writeFile(path.join(out, 'http-consumer.wasm'), component)
console.log(JSON.stringify({ bytes: component.length, scope: 'isolated engine cancellation only' }))
