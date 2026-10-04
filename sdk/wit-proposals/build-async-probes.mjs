import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import path from 'node:path'
import { componentize } from '../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/src/componentize.js'

if (process.argv.length !== 3) throw new Error('usage: node build-async-probes.mjs /absolute/new-directory')
const out = path.resolve(process.argv[2])
await mkdir(out)
const tool = new URL('../dsh-wasm-spike/node_modules/@bytecodealliance/componentize-js/package.json', import.meta.url)
assert.equal(JSON.parse(await readFile(tool, 'utf8')).version, '0.23.0')
const reports = []
const control = path.join(out, 'native-sync-control')
await mkdir(control)
await writeFile(path.join(control, 'net.wit'),
  (await readFile(path.join(import.meta.dirname, 'native-async/net.wit'), 'utf8')).replaceAll('async func', 'func'))
await writeFile(path.join(control, 'consumer.mjs'),
  await readFile(path.join(import.meta.dirname, 'native-async/consumer.mjs')))

for (const probe of ['http-consumer', 'native-sync-control', 'native-async']) {
  const dir = probe === 'native-sync-control' ? control : path.join(import.meta.dirname, probe)
  try {
    const { component } = await componentize({ sourcePath: path.join(dir, 'consumer.mjs'),
      witPath: dir, worldName: 'consumer', env: {},
      disableFeatures: probe === 'http-consumer'
        ? ['stdio', 'random', 'fetch-event']
        : ['stdio', 'random', 'clocks', 'http', 'fetch-event'],
    })
    await writeFile(path.join(out, `${probe}.wasm`), component)
    reports.push({ probe, built: true, bytes: component.length,
      sha256: createHash('sha256').update(component).digest('hex') })
  } catch (error) {
    const diagnostic = String(error.stack ?? error)
    await writeFile(path.join(out, `${probe}.error.log`), diagnostic)
    reports.push({ probe, built: false, diagnostic })
  }
}
await writeFile(path.join(out, 'build-report.json'), JSON.stringify({
  scope: 'isolated author probes; not production networking or quartet compatibility',
  componentizeJs: '0.23.0', reports,
}, null, 2) + '\n')
console.log(JSON.stringify(reports))
if (!reports.find(r => r.probe === 'http-consumer').built ||
    !reports.find(r => r.probe === 'native-sync-control').built) process.exitCode = 1
