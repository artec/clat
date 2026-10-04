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
let mutations = [
 ['entry-budget','network_resources.rs','self.entries.len() >= 16','self.entries.len() >= 17','resource_owner_counts_actual'],
 ['child-order','network_resources.rs','for child in children {\n            self.entries\n                .get_mut(&child)\n                .ok_or(Error::InvalidHandle)?\n                .retire_parent = false;\n            self.remove_rep(child)?;\n        }','for _child in children { /* deleted child-first cleanup */ }','resource_owner_counts_actual'],
 ['owner-identity','network_resources.rs','handle.owner != self.identity','false','resource_owner_foreign_and_recycled'],
 ['entry-type','network_resources.rs','entry.kind != TypeId::of::<T>()','false','resource_owner_foreign_and_recycled'],
 ['entry-generation','network_resources.rs','entry.serial != handle.serial','false','resource_owner_foreign_and_recycled'],
 ['close-resources','network_resources.rs','while let Some(rep) = self.entries.keys().next().copied() {\n            self.remove_rep(rep)?;\n        }','// deleted resource cleanup','resource_owner_drop_and_explicit_close'],
 ['owner-liveness','network_resources.rs','self\n            .scope\n            .check_active(Instant::now() + std::time::Duration::from_secs(120))','Ok::<(), Failure>(())','resource_owner_run_clear'],
 ['cancel-task','network_resources/task.rs','pub(crate) fn cancel(&mut self) {\n        self.state = State::Cancelled;','pub(crate) fn cancel(&mut self) {\n        self.state = State::Consumed;','resource_task_get_never_waits'],
 ['get-nonblocking','network_resources/task.rs','if matches!(self.state, State::Pending(_)) {','if false {','resource_task_get_never_waits'],
 ['single-consume','network_resources/task.rs','std::mem::replace(&mut self.state, State::Consumed)','std::mem::replace(&mut self.state, State::Cancelled)','resource_task_terminal_result'],
 ['get-liveness','network_resources/task.rs','self.scope.check_active(self.deadline)','Ok::<(), Failure>(())','resource_task_ready_resolution_revoked'],
 ['wait-liveness','network_resources/task.rs','error=self.scope.closed(self.deadline)=>Err(Error::Authority(error)),','// deleted wait liveness','resource_task_readiness_observes'],
 ['ready-future-drop','network_resources/task.rs','let State::Pending(operation) = std::mem::replace(&mut self.state, State::Cancelled) else {\n            unreachable!()\n        };','let State::Pending(operation) = &mut self.state else { return; };','resource_task_read_ready_drop'],
 ['wasi-wait-drop','network_resources/task.rs','// Keep the operation in Task; dropping this wait is not cancellation.\n        let State::Pending(operation) = &mut self.state else {','// Deleted cancellation-safe WASI wait.\n        let State::Pending(operation) = std::mem::replace(&mut self.state, State::Cancelled) else {','wasi_'],
 ['poll-drop-ledger','network_resources/poll.rs','self.drop_binding(resource).map_err(trap)','let handle = self.binding(&resource).map_err(trap)?;\n        HostPollable::drop(&mut self.table, Resource::new_own(handle.rep))','typed_host_subscription_limit'],
 ['canonical-recycling','network_resources.rs',"    fn binding<T: 'static>(&self, resource: &Resource<T>) -> Result<Handle<T>, Error> {\n        // Canonical reps are monotonic IDs, never recyclable ResourceTable indexes.\n        let (rep, entry) = self\n            .entries\n            .iter()\n            .find(|(_, e)| e.serial == u64::from(resource.rep()))\n            .ok_or(Error::InvalidHandle)?;\n        let handle = Handle {\n            rep: *rep,\n            serial: entry.serial,\n            owner: self.identity,\n            marker: PhantomData,\n        };\n        self.validate(&handle)?;\n        Ok(handle)\n    }\n    fn resource<T>(handle: Handle<T>) -> Resource<T> {\n        Resource::new_own(handle.serial as u32)\n    }\n","    fn binding<T: 'static>(&self, resource: &Resource<T>) -> Result<Handle<T>, Error> {\n        let entry = self.entries.get(&resource.rep()).ok_or(Error::InvalidHandle)?;\n        let handle = Handle { rep: resource.rep(), serial: entry.serial,\n            owner: self.identity, marker: PhantomData };\n        self.validate(&handle)?;\n        Ok(handle)\n    }\n    fn resource<T>(handle: Handle<T>) -> Resource<T> {\n        Resource::new_own(handle.rep)\n    }\n",'typed_host_canonical_ids'],
 ['owner-replacement','network_resources.rs',"        // One namespace survives tool-owner replacement in a live Store.\n        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);\n        NEXT.fetch_update(\n            std::sync::atomic::Ordering::Relaxed,\n            std::sync::atomic::Ordering::Relaxed,\n            |value| {\n                value\n                    .checked_add(1)\n                    .filter(|next| *next <= u64::from(u32::MAX))\n            },\n        )\n        .map_err(|_| Error::Limit)","        Ok(self.serial + 1)",'typed_host_owner_replacement'],
 ['timer-parent-drop','network_resources.rs','if let Some(parent) = retire {\n            self.remove_rep(parent)?;\n        }','let _ = retire;','scheduler_timer_drop_releases'],
 ['timer-child-disarm','network_resources.rs','self.entries\n                .get_mut(&child)\n                .ok_or(Error::InvalidHandle)?\n                .retire_parent = false;','// deleted hidden-parent disarm','scheduler_subscription_failure'],
 ['timer-past-ready','network_resources/clocks.rs','Some(when) if when <= tokio::time::Instant::now() => {','Some(when) if when <= tokio::time::Instant::now() && !self.yielded => {','scheduler_clocks_are_real'],
 ['timer-zero-yield','network_resources/clocks.rs','yielded: !duration.is_zero(),','yielded: true,','scheduler_clocks_are_real'],
 ['timer-expired-ready','network_resources/clocks.rs','yielded: !duration.is_zero(),','yielded: false,','scheduler_clocks_are_real'],
 ['timer-rollback','network_resources/clocks.rs','self.remove(&root)?;','// deleted timer admission rollback','scheduler_subscription_failure'],
 ['timer-revocation','network_resources/clocks.rs','_ = self.scope.closed(limit) => {},','_ = std::future::pending::<()>() => {},','scheduler_revocation_interrupts'],
 ['timer-postwait-clear','network_resources/poll.rs','HostPollable::block(&mut self.table, Resource::new_borrow(handle.rep))?;\n        self.check().map_err(trap)','HostPollable::block(&mut self.table, Resource::new_borrow(handle.rep))','scheduler_revocation_interrupts'],
 ['diagnostic-total-budget','network_resources/diagnostics.rs','Box::new(self.diagnostics.clone())','Box::new(wasmtime_wasi::p2::pipe::MemoryOutputPipe::new(32768))','scheduler_diagnostics_share_one'],
 ['diagnostic-error-ledger','network_resources/diagnostics.rs','StreamError::Closed | StreamError::LastOperationFailed(_) => {\n                Ok(streams::StreamError::Closed)\n            }','StreamError::Closed => Ok(streams::StreamError::Closed),\n            StreamError::LastOperationFailed(error) => { let value = self.table.push(error)?; Ok(streams::StreamError::LastOperationFailed(value)) }','scheduler_error_conversion'],
 ['accepted-socket-blocking','network_resources/component/native_transport_tests.rs','socket.set_nonblocking(false).unwrap();','// deleted accepted socket mode','native_transport_fixture_waits'],
 ['metadata-close','network_resources/component.rs','if result.is_err() {\n                r.cancel();\n            }','// deleted typed metadata failure cleanup','typed_host_unrepresentable'],
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
    console.log(`RESOURCE_OWNER_DELETION ${JSON.stringify(evidence.at(-1))}`)
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
