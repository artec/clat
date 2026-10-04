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
let mutations = [
 ['expired-construction','dns_authority/scope.rs','let tool = super::Tool::from_run(state.deadline);','let deadline = state.deadline; drop(state); let tool = super::Tool::new(deadline).expect("expired tool"); let state = run.inner.lock().unwrap();','expired_run_scope_construction'],
 ['tool-attempts','dns_authority/budget.rs','tool.http_used >= 10','false','tool_http_budget_cannot_reset'],
 ['run-attempts','dns_authority/budget.rs','run.http_used >= 64','false','run_http_budget_cannot_reset'],
 ['http-store-resources','dns_authority/budget.rs','store.jobs.len() + store.http_live.load(Ordering::Acquire) >= 16','false','shared_store_budget_counts'],
 ['dns-store-resources','dns_authority/scope.rs','store.jobs.len() + store.http_live.load(std::sync::atomic::Ordering::Acquire) >= 16','false','shared_store_budget_counts'],
 ['resource-drop','dns_authority/budget.rs','self.0.fetch_sub(1, Ordering::AcqRel);','// deleted HTTP reservation release','http_store_resources_release'],
 ['tool-drop','dns_authority/budget.rs','self.inner.lock().unwrap().active = false;','// deleted tool termination','tool_deadline_and_drop_revoke'],
 ['dns-tool-deadline','dns_authority/scope.rs','let deadline = (entered + timeout)\n            .min(run.deadline)\n            .min(self.inner.tool.deadline());','let deadline = (entered + timeout).min(run.deadline);','tool_entry_deadline_includes_dns'],
 ['request-deadline','http_authority/transport/stream.rs','connection.pins.restrict_deadline(deadline);','// deleted request ceiling','stream_request_ceiling'],
 ['headers-first','http_authority/transport/stream.rs','    validate_head(&parts, max)?;','    validate_head(&parts, max)?; let mut drain = Body { incoming, driver, pending: Bytes::new(), received: 0 }; while drain.next().await?.is_some() {} let Body { incoming, driver, .. } = drain;','stream_headers_precede_body'],
 ['read-argument','http_authority/transport/stream.rs','if !(1..=65536).contains(&max_bytes) {','if false {','stream_headers_precede_body'],
 ['read-chunk','http_authority/transport/stream.rs','max_bytes.min(body.pending.len())','body.pending.len()','stream_headers_precede_body'],
 ['guest-limit','http_authority/transport/stream.rs','if data.len() > self.max - body.received {','if false {','stream_guest_limit_rejects'],
 ['metadata-guard','http_authority/transport/stream.rs','if let Err(error) = self.check() {','if let Err(error) = Ok::<(), Failure>(()) {','stream_metadata_and_read_reject'],
 ['cancel-socket','http_authority/transport/stream.rs','self.body = None;\n        self.process = None;','// deleted socket cancellation\n        self.process = None;','stream_cancel_and_drop_close'],
 ['cancel-budget','http_authority/transport/stream.rs','self.process = None;','// deleted process release','stream_cancel_and_drop_close'],
 ['read-future-drop','http_authority/transport/stream.rs','self.response.cancel();','// deleted dropped future cancellation','stream_dropped_read_future'],
 ['driver-completion','http_authority/transport/stream.rs','(fetch.await.map_err(|_| Failure::Transport)?, None)','return Err(Failure::Transport)','http_wire_preserves'],
]

if (process.argv[3]) {
  mutations = mutations.filter(item => item[0] === process.argv[3])
  assert(mutations.length, "unknown deletion name")
}
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
  await cp(source, copy, { recursive: true })
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
    console.log(`STREAM_BUDGET_DELETION ${JSON.stringify(evidence.at(-1))}`)
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
