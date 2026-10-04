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
  ['fence', 'scope.rs', 'self.inner.fence.check(origin)?;', '// deleted exact fence', 'exact_origin_rejects'],
  ['config', 'origin.rs', 'Some(config) => upper.intersection(&Self::parse(config)?).cloned().collect(),',
    'Some(_config) => upper.clone(),', 'config_can_only_narrow'],
  ['userinfo', 'origin.rs', String.raw`text.contains(['\\', '@', '?', '#'])`,
    String.raw`text.contains(['\\', '?', '#'])`, 'url_normalizer_rejects'],
  ['full-address-set', 'addresses.rs', 'if !is_public(*address) {', 'if false {', 'mixed_answers_reject'],
  ...[32, 40, 48, 56, 64, 96].map(length => [
    `nat64-${length}`, 'nat64.rs', 'if !is_public(embedded.into()) {', 'if false {', `dns_authority::tests::nat64_${length}`]),
  ['all-prefixes', 'nat64.rs', 'for prefix in prefixes {', 'for prefix in prefixes.iter().take(1) {', 'every_matching_nat64_prefix'],
  ['discovery-member', 'nat64.rs', '_ => return Err(Failure::InvalidDiscovery),', '_ => {},', 'discovery_malformed_member'],
  ['foreign-store', 'credential.rs', 'if !Arc::ptr_eq(&scope.inner, &self.scope.inner) {', 'if false {', 'foreign_store_and_new_run'],
  ['single-consume', 'credential.rs', '.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)',
    '.compare_exchange(self.consumed.load(Ordering::Acquire), true, Ordering::AcqRel, Ordering::Acquire)', 'wrong_origin_does_not_consume'],
  ['run-generation', 'scope.rs', 'if !run.active || run.generation != self.inner.generation {', 'if false {', 'policy_invalidation_closes'],
  ['deadline', 'scope.rs', 'if Instant::now() >= deadline.min(run.deadline).min(self.inner.tool.deadline()) {', 'if false {', 'resolution_expiry_is_not_extended'],
  ['store-budget', 'scope.rs', 'store.jobs.len() + store.http_live.load(std::sync::atomic::Ordering::Acquire) >= 16', 'false', 'live_credentials_keep_store_budget'],
  ['run-budget', 'scope.rs', 'run.dns_used >= 64', 'false', 'run_dns_budget_is_shared'],
  ['late-publication', 'resolver.rs', 'if !matches!(*state, State::Pending) {', 'if false {', 'cancelled_discovery_cannot_publish'],
  ['queue-budget', 'resolver.rs', 'mpsc::sync_channel::<Work>(queue)', 'mpsc::sync_channel::<Work>(queue + 1)', 'fixed_workers_and_queue_reject'],
  ['store-drop', 'scope.rs', 'impl Drop for Scope {\n    fn drop(&mut self) {\n        if self.owner {',
    'impl Drop for Scope {\n    fn drop(&mut self) {\n        if false {', 'store_drop_cancels_pending'],
]
const digest = text => createHash('sha256').update(text).digest('hex')
const readSource = async file => (await readFile(file, 'utf8')).replaceAll('\r\n', '\n')
const originals = new Map()
for (const [, file] of mutations) {
  if (!originals.has(file)) originals.set(file, await readSource(path.join(source, 'src/dns_authority', file)))
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
const temporary = await mkdtemp(path.join(tmpdir(), 'clat-dns-deletion-'))
const evidence = []
try {
  const copy = path.join(temporary, 'host')
  await cp(source, copy, { recursive: true })
  // Cargo can reuse a same-name standalone crate's fingerprint in a shared target.
  // Give the isolated mutant its own package/artifact identity; share dependencies only.
  const packageName = 'clat-plg4-http-consumer-host'
  const mutantName = packageName+'-dns-deletion'+'-'+randomUUID().replaceAll('-', '')
  const manifest = await readFile(path.join(copy, 'Cargo.toml'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.toml'), manifest.replaceAll('path = "../../../crates/core"', `path = ${JSON.stringify(path.join(root, "crates/core"))}`).replace(`name = "${packageName}"`,
    `name = "${mutantName}"`).replace(/^default-run = .*\n/m, ''))
  const lock = await readFile(path.join(copy, 'Cargo.lock'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.lock'), lock.replace(`name = "${packageName}"`, `name = "${mutantName}"`))
  for (const [name, file, before, after, filter] of mutations) {
    const original = originals.get(file)
    assert.equal(original.split(before).length - 1, 1, `unique deletion point: ${name}`)
    const destination = path.join(copy, 'src/dns_authority', file)
    await writeFile(destination, original.replace(before, after))
    const result = await run(path.join(copy, 'Cargo.toml'), filter)
    await writeFile(path.join(output, `${name}.log`), result.log)
    await writeFile(destination, original) // Restore only our isolated copy.
    assert.equal(result.code, 101, `${name}: must be red`)
    assert.match(result.log, /panicked at/, `${name}: behavioral assertion required`)
    assert.match(result.log, /test result: FAILED\./)
    assert.doesNotMatch(result.log, /error\[E\d+\]|could not compile/)
    evidence.push({ deletion: name, filter, exit: result.code, sourceSha256: digest(original) })
    console.log(`DNS_DELETION ${JSON.stringify(evidence.at(-1))}`)
  }
  const final = await run(path.join(source, 'Cargo.toml'), 'dns_authority')
  await writeFile(path.join(output, 'canonical-green.log'), final.log)
  assert.equal(final.code, 0)
  assert.match(final.log, /49 passed; 0 failed/)
  for (const [file, original] of originals) {
    assert.equal(await readSource(path.join(source, 'src/dns_authority', file)), original, 'shared source unchanged')
  }
  await writeFile(path.join(output, 'report.json'), JSON.stringify({ deletions: evidence, canonicalPassed: 49,
    fullD1AttackMatrixComplete: false }, null, 2)+'\n')
} finally {
  await rm(temporary, { recursive: true, force: true })
}
