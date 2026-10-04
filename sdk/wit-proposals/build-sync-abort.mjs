import assert from 'node:assert/strict'
import { writeFile, mkdir, readFile } from 'node:fs/promises'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'

if (process.argv.length !== 3) throw new Error('usage: node build-sync-abort.mjs /absolute/new-directory')
const metadata = JSON.parse(await readFile(new URL(
  '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/package.json', import.meta.url), 'utf8'))
assert.equal(metadata.version, '0.23.0', 'install the pinned PLG-3 author toolchain first')
const out = path.resolve(process.argv[2])
await mkdir(out)
const { component } = await componentize({
  sourcePath: path.join(import.meta.dirname, 'sync-abort-probe.mjs'),
  witPath: path.join(import.meta.dirname, 'net'), worldName: 'consumer-probe',
  disableFeatures: ['stdio', 'random', 'clocks', 'http', 'fetch-event'], env: {},
})
await writeFile(path.join(out, 'sync-abort.wasm'), component)
console.log(JSON.stringify({ output: out, bytes: component.length, scope: 'sync ABI consumer only' }))
