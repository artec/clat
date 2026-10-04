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
 ['joint-authority', 'mod.rs', '.is_some_and(|set| set.contains(&method))', '.is_some_and(|_set| true)', 'origin_and_method_are_joint_authority'],
 ['config-methods', 'mod.rs', 'methods.retain(|method| narrow.get(origin).is_some_and(|set| set.contains(method)));', 'let _ = (origin, methods, &narrow);', 'configuration_only_narrows_methods'],
 ['raw-url', 'mod.rs', "text.contains(['\\\\', '#'])", "text.contains(['\\\\'])", 'raw_url_cannot_hide_authority_or_controls'],
 ['control-headers', 'mod.rs', 'forbidden_header(name.as_str())', 'false', 'transport_control_headers_are_closed'],
 ['value-controls', 'mod.rs', 'value.bytes().any(|b| b < 32 || b == 127)', 'false', 'controls_in_all_header_values_are_rejected'],
 ['body-budget', 'mod.rs', 'body.len() > 1024 * 1024', 'false', 'budgets_include_raw_url_headers_and_body'],
 ['url-budget', 'mod.rs', 'text.len() > 2048', 'false', 'budgets_include_raw_url_headers_and_body'],
 ['header-count', 'mod.rs', 'headers.len() > 64', 'false', 'budgets_include_raw_url_headers_and_body'],
 ['header-bytes', 'mod.rs', '.is_none_or(|total| total > 32768)', '.is_none_or(|_total| false)', 'budgets_include_raw_url_headers_and_body'],
 ['duplicate-declaration', 'mod.rs', 'if result.insert(origin, methods).is_some() {', 'result.insert(origin, methods); if false {', 'declarations_are_bounded_and_unambiguous'],
 ['declaration-count', 'mod.rs', 'list.len() > 64', 'false', 'declarations_are_bounded_and_unambiguous'],
]

const digest = text => createHash('sha256').update(text).digest('hex')
const readSource = async file => (await readFile(file, 'utf8')).replaceAll('\r\n', '\n')
const originals = new Map()
for (const [, file] of mutations) {
  if (!originals.has(file)) originals.set(file, await readSource(path.join(source, 'src/http_authority', file)))
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
const temporary = await mkdtemp(path.join(tmpdir(), 'clat-http-request-deletion-'))
const evidence = []
try {
  const copy = path.join(temporary, 'host')
  await copyAuthor(source, copy, root)
  await cp(path.join(here, 'typed-task'), path.join(temporary, 'typed-task'), { recursive: true })
  // Cargo can reuse a same-name standalone crate's fingerprint in a shared target.
  // Give the isolated mutant its own package/artifact identity; share dependencies only.
  const packageName = 'clat-plg4-http-consumer-host'
  const mutantName = packageName+'-http-request-deletion'+'-'+randomUUID().replaceAll('-', '')
  const manifest = await readFile(path.join(copy, 'Cargo.toml'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.toml'), manifest.replaceAll('path = "../../../crates/core"', `path = ${JSON.stringify(path.join(root, "crates/core"))}`).replace(`name = "${packageName}"`,
    `name = "${mutantName}"`).replace(/^default-run = .*\n/m, ''))
  const lock = await readFile(path.join(copy, 'Cargo.lock'), 'utf8')
  await writeFile(path.join(copy, 'Cargo.lock'), lock.replace(`name = "${packageName}"`, `name = "${mutantName}"`))
  for (const [name, file, before, after, filter] of mutations) {
    const original = originals.get(file)
    assert.equal(original.split(before).length - 1, 1, `unique deletion point: ${name}`)
    const destination = path.join(copy, 'src/http_authority', file)
    await writeFile(destination, original.replace(before, after))
    const result = await run(path.join(copy, 'Cargo.toml'), filter)
    await writeFile(path.join(output, `${name}.log`), result.log)
    await writeFile(destination, original) // Restore only our isolated copy.
    assert.equal(result.code, 101, `${name}: must be red`)
    assert.match(result.log, /panicked at/, `${name}: behavioral assertion required`)
    assert.match(result.log, /test result: FAILED\./)
    assert.doesNotMatch(result.log, /error\[E\d+\]|could not compile/)
    evidence.push({ deletion: name, filter, exit: result.code, sourceSha256: digest(original) })
    console.log(`HTTP_REQUEST_DELETION ${JSON.stringify(evidence.at(-1))}`)
  }
  const final = await run(path.join(source, 'Cargo.toml'), '')
  await writeFile(path.join(output, 'canonical-green.log'), final.log)
  assert.equal(final.code, 0)
  assert.match(final.log, /147 passed; 0 failed/)
  for (const [file, original] of originals) {
    assert.equal(await readSource(path.join(source, 'src/http_authority', file)), original, 'shared source unchanged')
  }
  await writeFile(path.join(output, 'report.json'), JSON.stringify({ deletions: evidence, canonicalPassed: 147,
    fullD1AttackMatrixComplete: false }, null, 2)+'\n')
} finally {
  await rm(temporary, { recursive: true, force: true })
}
