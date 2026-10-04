// D1 shape checks only: no network implementation and no production registration.
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, writeFile, copyFile, rm } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import os from 'node:os'
import path from 'node:path'

const repo = path.resolve(import.meta.dirname, '../..')
const toolRoot = path.join(repo, 'sdk/dsh-wasm-spike/node_modules/@bytecodealliance/jco')
const metadata = JSON.parse(await readFile(path.join(toolRoot, 'package.json'), 'utf8'))
assert.equal(metadata.version, '1.35.0', 'install the pinned PLG-3 author toolchain first')
const cli = path.join(toolRoot, metadata.bin.jco)
const original = path.join(repo, 'wit/plugin.wit')
const sha = bytes => createHash('sha256').update(bytes).digest('hex')
const before = sha(await readFile(original))

function command(args, accepted = true) {
  const result = spawnSync(process.execPath, [cli, ...args], { encoding: 'utf8' })
  if (result.error) throw result.error
  if (accepted) assert.equal(result.status, 0, result.stderr)
  else assert.notEqual(result.status, 0, 'missing net dependency must fail')
  return result
}

async function world(dir, name, temporary) {
  const core = path.join(temporary, `${name}.core.wasm`)
  const component = path.join(temporary, `${name}.wasm`)
  command(['embed', '--wit', dir, '--world-name', name, '--dummy', '-o', core])
  command(['new', core, '-o', component])
  return command(['wit', component]).stdout
}

const temporary = await mkdtemp(path.join(os.tmpdir(), 'clat-plg4-wit-'))
try {
  const legacy = await world(path.join(repo, 'wit'), 'plugin', temporary)
  assert.doesNotMatch(legacy, /clat:net/)
  const net = await world(path.join(import.meta.dirname, 'net/net.wit'), 'net-client', temporary)
  assert.match(net, /import clat:net\/egress@0\.1\.0/)
  assert.doesNotMatch(net, /wasi:|socket|tcp|udp/)
  const composition = path.join(temporary, 'composition')
  await mkdir(path.join(composition, 'deps/plugin'), { recursive: true })
  await mkdir(path.join(composition, 'deps/net'), { recursive: true })
  await copyFile(original, path.join(composition, 'deps/plugin/plugin.wit'))
  await copyFile(path.join(import.meta.dirname, 'net/net.wit'), path.join(composition, 'deps/net/net.wit'))
  await writeFile(path.join(composition, 'world.wit'),
    'package clat:proposal@0.1.0;\nworld plugin-with-net {\n' +
    'include clat:plugin/plugin@0.1.0;\nimport clat:net/egress@0.1.0;\n}\n')
  const combined = await world(composition, 'plugin-with-net', temporary)
  assert.match(combined, /export clat:plugin\/tools@0\.1\.0/)
  for (const name of ['sampling', 'elicitation', 'config', 'host']) {
    assert.ok(combined.includes(`import clat:plugin/${name}@0.1.0`), name)
  }
  assert.match(combined, /import clat:net\/egress@0\.1\.0/)
  assert.doesNotMatch(combined, /wasi:/)
  await rm(path.join(composition, 'deps/net'), { recursive: true })
  const negative = command(['embed', '--wit', composition, '--world-name', 'plugin-with-net',
    '--dummy', '-o', path.join(temporary, 'missing.wasm')], false)
  assert.match(negative.stderr, /clat:net/)
  assert.equal(sha(await readFile(original)), before)
  console.log(JSON.stringify({ scope: 'D1 WIT shape only; not host safety acceptance',
    checks: 4, oldWorldUnchanged: true, jco: metadata.version, pluginWitSha256: before }))
} finally {
  await rm(temporary, { recursive: true, force: true })
}
