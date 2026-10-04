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
 ['binding-origin','dns_authority/credential.rs','if target != &self.origin {','if false {','binding_rejects_origin_change'],
 ['binding-store','dns_authority/credential.rs','if !Arc::ptr_eq(&scope.inner, &self.scope.inner) {','if false {','binding_rejects_origin_change'],
 ['single-consume','dns_authority/credential.rs','.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)', '.compare_exchange(self.consumed.load(Ordering::Acquire), true, Ordering::AcqRel, Ordering::Acquire)', 'binding_rejects_origin_change'],
 ['run-notification','dns_authority/scope.rs','state.notify.notify_waiters();','// deleted notification','run_invalidation_wakes'],
 ['store-notification','dns_authority/scope.rs','store.notify.notify_waiters();','// deleted notification','tls_wait_cancellation'],
 ['absolute-deadline','dns_authority/credential.rs','tokio::time::sleep_until(self.deadline.into())','tokio::time::sleep_until((self.deadline + std::time::Duration::from_secs(60)).into())','original_deadline_bounds_tls'],
 ['cancel-race','http_authority/connector.rs','error = pins.closed() => return Err(error.into()),','// deleted cancellation branch','tls_wait_cancellation'],
 ['late-success','http_authority/connector.rs','    pins.snapshot()?;','    // deleted final liveness check','completion_cannot_publish'],
 ['original-hostname','http_authority/connector.rs','ServerName::try_from(origin.host().to_owned())','ServerName::try_from("tls-fixture.invalid".to_owned())','tls_rejects_trusted_chain_with_wrong_hostname'],
 ['numeric-pins','http_authority/connector.rs','for address in addresses {','for address in addresses.take(0) {','numeric_dial_connects'],
 ['wait-registration','dns_authority/credential.rs',`if let Err(error) = self.snapshot() {
            return error;
        }`,'// deleted initial state check','invalidation_before_wait_registration'],
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
const temporary = await mkdtemp(path.join(tmpdir(), 'clat-pinned-connect-deletion-'))
const evidence = []
try {
  const copy = path.join(temporary, 'host')
  await cp(source, copy, { recursive: true })
  // Cargo can reuse a same-name standalone crate's fingerprint in a shared target.
  // Give the isolated mutant its own package/artifact identity; share dependencies only.
  const packageName = 'clat-plg4-http-consumer-host'
  const mutantName = packageName+'-pinned-connect-deletion'+'-'+randomUUID().replaceAll('-', '')
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
    console.log(`PINNED_CONNECT_DELETION ${JSON.stringify(evidence.at(-1))}`)
  }
  const final = await run(path.join(source, 'Cargo.toml'), '')
  await writeFile(path.join(output, 'canonical-green.log'), final.log)
  assert.equal(final.code, 0)
  assert.match(final.log, /88 passed; 0 failed/)
  for (const [file, original] of originals) {
    assert.equal(await readSource(path.join(source, 'src', file)), original, 'shared source unchanged')
  }
  await writeFile(path.join(output, 'report.json'), JSON.stringify({ deletions: evidence, canonicalPassed: 88,
    fullD1AttackMatrixComplete: false }, null, 2)+'\n')
} finally {
  await rm(temporary, { recursive: true, force: true })
}
