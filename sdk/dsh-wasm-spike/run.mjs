import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { engineProbe, strictOriginalAttempt, originalInventory, builtinAttempt, loweredOriginalAttempt } from './build.mjs'

if (process.argv.length !== 4 || process.argv[2] !== '--out') throw new Error('usage: npm run probe -- --out /absolute/new-directory')
const out = path.resolve(process.argv[3])
await mkdir(out, { recursive: false })
const original = await originalInventory()
const engine = await engineProbe(out)
const attempt = await strictOriginalAttempt(out)
const builtin = await builtinAttempt(out)
const loweredAttempt = await loweredOriginalAttempt(out)
const after = await originalInventory()
if (JSON.stringify(original) !== JSON.stringify(after)) throw new Error('original input changed during probe')
const report = { status: 'K2: existing WIT contract insufficient; no official WASM package emitted', engine, attempt, builtin, loweredAttempt, original }
await writeFile(path.join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n')
console.log(JSON.stringify({ status: report.status, engineBytes: engine.bytes, output: out }))
