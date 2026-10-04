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
 ['approval-before-dns','http_authority/network.rs','result = approval => result.map_err(Error::Permission)?,','result = async { Ok::<(), PermissionFailure>(()) } => result.map_err(Error::Permission)?,','core_dns_denial_plan_fence'],
 ['credential-core-guard','dns_authority/scope.rs','guard.check(deadline)?;','// deleted core credential guard','core_dns_ready_and_consumed'],
 ['dns-wait-core-guard','dns_authority/resolver.rs','error = self.scope.guard_closed(self.deadline) => { self.slot.cancel(error); },','// deleted DNS wait guard','core_dns_wait_cancels'],
 ['pins-wait-core-guard','dns_authority/credential.rs','error = self.scope.guard_closed(self.deadline) => error,','// deleted pins wait guard','core_dns_consumed_pins_wait'],
 ['dispatch-core-guard','dns_authority/resolver.rs','fn execute(work: Work, lookup: &dyn Lookup) {\n        if !work.current() {','fn execute(work: Work, lookup: &dyn Lookup) {\n        if false {','core_dns_revoked_queue'],
 ['discovery-core-guard','dns_authority/resolver.rs','let addresses = lookup.lookup(&work.host)?;\n        if !work.current() {','let addresses = lookup.lookup(&work.host)?;\n        if false {','core_dns_revocation_after_lookup'],
 ['deadline-rebase','dns_authority/resolver.rs','let job = self.0.take().unwrap();','let mut job = self.0.take().unwrap(); job.deadline += Duration::from_millis(100);','core_dns_absolute_deadline'],
 ['scope-approval-watch','http_authority/network.rs','error = self.scope.closed(deadline) => return Err(Error::Authority(error)),','// deleted Store/run approval cancellation','core_dns_scope_teardown'],
]

const digest = text => createHash('sha256').update(text).digest('hex')
const readSource = async file => (await readFile(file, 'utf8')).replaceAll('\r\n', '\n')
const originals = new Map()
for (const [, file] of mutations) {
  if (!originals.has(file)) originals.set(file, await readSource(path.join(source, 'src', file)))
}
for (const [name, file, before] of mutations) assert.equal(originals.get(file).split(before).length - 1, 1, `unique deletion point: ${name}`)
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
const temporary = await mkdtemp(path.join(tmpdir(), 'clat-core-dns-binding-deletion-'))
const evidence = []
try {
  const copy = path.join(temporary, 'host')
  await copyAuthor(source, copy, root)
  await cp(path.join(here, 'typed-task'), path.join(temporary, 'typed-task'), { recursive: true })
  // Cargo can reuse a same-name standalone crate's fingerprint in a shared target.
  // Give the isolated mutant its own package/artifact identity; share dependencies only.
  const packageName = 'clat-plg4-http-consumer-host'
  const mutantName = packageName+'-core-dns-binding-deletion'+'-'+randomUUID().replaceAll('-', '')
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
    console.log(`CORE_DNS_BINDING_DELETION ${JSON.stringify(evidence.at(-1))}`)
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
