import { createHash } from 'node:crypto'
import { execFile } from 'node:child_process'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, join, resolve } from 'node:path'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const here = import.meta.dirname
const repo = resolve(here, '..', '..', '..', '..')
// `npm --prefix` changes the script cwd to sdk/dsh-adapter. Resolve an
// operator-supplied relative checkout from npm's original invocation cwd so
// the documented repo-root command points at the intended sibling checkout.
const dshRoot = process.env.DSH_ROOT
  ? resolve(process.env.INIT_CWD ?? process.cwd(), process.env.DSH_ROOT)
  : join(repo, '..', 'deepseek-harness')
// Bun resolves the pinned monorepo's workspace packages directly to their TS
// sources. The published `lib/index.js` outputs are intentionally absent from
// this source checkout, so plain Node/tsx would accidentally test a half-built
// package graph rather than the pinned runtime.
const bun = process.env.BUN_BIN ?? 'bun'
const out = join(repo, 'tests', 'fixtures', 'dsh-oracle')
const names = ['cordis-events', 'system-prompt', 'session-jsonl', 'workspace-model']
const temp = await mkdtemp(join(tmpdir(), 'clat-dsh-oracle-'))
const sha = value => createHash('sha256').update(value).digest('hex')

try {
  await mkdir(out, { recursive: true })
  const manifest = { pinnedRevision: 'b150a551b8', entries: {} }
  for (const name of names) {
    const generator = join(here, `gen-${name}.mts`)
    const checker = join(here, `check-${name}.mjs`)
    await exec(process.execPath, [checker], { env: { ...process.env, DSH_ROOT: dshRoot } })
    const a = join(temp, `${name}-a.json`)
    const b = join(temp, `${name}-b.json`)
    await exec(bun, [generator, '--output', a], { env: { ...process.env, DSH_ROOT: dshRoot } })
    await exec(bun, [generator, '--output', b], { env: { ...process.env, DSH_ROOT: dshRoot } })
    const [aBytes, bBytes, generatorBytes, checkerBytes] = await Promise.all([
      readFile(a), readFile(b), readFile(generator), readFile(checker),
    ])
    if (!aBytes.equals(bBytes)) throw new Error(`${name}: double-run output differs`)
    await writeFile(join(out, `${name}.json`), aBytes)
    manifest.entries[name] = {
      fixture: `${name}.json`,
      fixtureSha256: sha(aBytes),
      generator: basename(generator),
      generatorSha256: sha(generatorBytes),
      checker: basename(checker),
      checkerSha256: sha(checkerBytes),
    }
  }
  await writeFile(join(out, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`)
} finally {
  await rm(temp, { recursive: true, force: true })
}
