import {copyAuthor} from './author-copy.mjs'
// Author-only deletion checks. Never mutate the shared working tree.
import assert from 'node:assert/strict'
import { createHash, randomUUID } from 'node:crypto'
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const root = path.resolve(here, '../..')
const source = path.join(here, 'http-consumer-host')
const output = path.resolve(process.argv[2] ?? '')
assert(process.argv[2] && path.isAbsolute(process.argv[2]), 'pass an absolute new evidence directory')
await mkdir(output) // Refuses overwrite.
const mutations = [
 ['authority-lifetime','http_authority/permission.rs','self.source.check()','Ok(())','core_gate_revocation_closes_http'],
 ['deny-terminal','http_authority/permission/core_adapter.rs','self.get().map(|result| result.map_err(Into::into))','self.get().map(|_| Ok(()))','modes_use_actual_core_network'],
 ['approval-origin','http_authority/permission.rs','request.origin(),','&Origin::parse("https://other.invalid").unwrap(),','http_wire_preserves'],
 ['approval-action','http_authority/permission/core_adapter.rs','            method,','            None,','core_gate_dns_http_share'],
 ['encoded-entity','http_authority/transport/stream.rs','if value.as_bytes() != b"identity" {','if false {','encoded_body_is_explicitly'],
 ['advertised-entity','http_authority/transport/stream.rs','if length > max as u64 {','if false {','advertised_oversize_response'],
 ['cumulative-entity','http_authority/transport/stream.rs','if data.len() > self.max - body.received {','if false {','chunked_cumulative_body'],
 ['process-budget','http_authority/transport.rs','Semaphore::new(8)','Semaphore::new(9)','response_holding_keeps'],
 ['header-budget','http_authority/transport.rs','const HEADER_COUNT: usize = 64;','const HEADER_COUNT: usize = 100;','response_header_count'],
 ['body-cancel','http_authority/transport/stream.rs','_ = cancelled(&self.gate,&self.cancel,self.deadline) => return Err(Failure::Permission(self.gate.check(&self.cancel,self.deadline).unwrap_err())),','// deleted body cancellation','waiting_body_cancel'],
]

const digest = text => createHash('sha256').update(text).digest('hex')
const readSource = async file => (await readFile(file, 'utf8')).replaceAll('\r\n', '\n')
const originals = new Map()
for (const [, file] of mutations) {
  if (!originals.has(file)) originals.set(file, await readSource(path.join(source, 'src', file)))
}
async function run(manifest, filter) {
  return await new Promise((resolve, reject) => {
    const child = spawn('cargo', ['test', '--manifest-path', manifest, '--locked', '--offline',
      '--target-dir', path.join(root, 'target'), '--lib', filter, '--', '--nocapture'], { cwd: root })
    let log = ''
    child.stdout.on('data', chunk => { log += chunk })
    child.stderr.on('data', chunk => { log += chunk })
    child.on('error', reject)
    child.on('close', code => resolve({ code, log }))
  })
}
const temporary = await mkdtemp(path.join(tmpdir(), 'clat-network-http-deletion-'))
const evidence = []
try {
  const copy = path.join(temporary, 'host')
  await copyAuthor(source, copy, root)
  await cp(path.join(here, 'typed-task'), path.join(temporary, 'typed-task'), { recursive: true })
  // Cargo can reuse a same-name standalone crate's fingerprint in a shared target.
  // Give the isolated mutant its own package/artifact identity; share dependencies only.
  const packageName = 'clat-plg4-http-consumer-host'
  const mutantName = packageName+'-network-http-deletion'+'-'+randomUUID().replaceAll('-', '')
  const manifest = await readFile(path.join(copy, 'Cargo.toml'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.toml'), manifest.replaceAll('path = "../../../crates/core"', `path = ${JSON.stringify(path.join(root, "crates/core"))}`).replace(`name = "${packageName}"`,
    `name = "${mutantName}"`).replace(/^default-run = .*\n/m, ''))
  const lock = await readFile(path.join(copy, 'Cargo.lock'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.lock'), lock.replace(`name = "${packageName}"`, `name = "${mutantName}"`))
  for (const [name, file, before, after, filter] of mutations) {
    const original = originals.get(file)
    assert.equal(original.split(before).length - 1, 1, `unique deletion point: ${name}`)
    const destination = path.join(copy, 'src', file)
    await writeFile(destination, original.replace(before, after))
    const result = await run(path.join(copy, 'Cargo.toml'), filter)
    await writeFile(path.join(output, `${name}.log`), result.log)
    await writeFile(destination, original) // Restore only our isolated copy.
    assert.equal(result.code, 101, `${name}: must be red`)
    assert.match(result.log, /panicked at/, `${name}: behavioral assertion required`)
    assert.match(result.log, /test result: FAILED\./)
    assert.doesNotMatch(result.log, /error\[E\d+\]|could not compile/)
    evidence.push({ deletion: name, filter, exit: result.code, sourceSha256: digest(original) })
    console.log(`NETWORK_HTTP_DELETION ${JSON.stringify(evidence.at(-1))}`)
  }
  const final = await run(path.join(source, 'Cargo.toml'), '')
  await writeFile(path.join(output, 'canonical-green.log'), final.log)
  assert.equal(final.code, 0)
  assert.match(final.log, /147 passed; 0 failed/)
  for (const [file, original] of originals) {
    assert.equal(await readSource(path.join(source, 'src', file)), original, 'shared source unchanged')
  }
  await writeFile(path.join(output, 'report.json'), JSON.stringify({ deletions: evidence, canonicalPassed: 147,
    fullD1AttackMatrixComplete: false }, null, 2)+'\n')
} finally {
  await rm(temporary, { recursive: true, force: true })
}
