// Build metadata from a publisher-signed package, never from hand-entered hashes.
// This produces a proposal for owner review; it does not establish market trust.
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile, stat } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import path from 'node:path'

const args = process.argv.slice(2)
function option(name) {
  const at = args.indexOf(name)
  if (at < 0 || !args[at + 1]) throw new Error(`missing ${name}`)
  return args[at + 1]
}
const directory = path.resolve(option('--package'))
const destination = path.resolve(option('--out'))
const target = option('--target')
const keyId = option('--publisher-key-id')
const reviewUrl = option('--review-url')
const sourceUrl = option('--source-url')
const executable = path.resolve(option('--clat'))
for (const value of [target, keyId]) if (!/^[a-zA-Z0-9._-]+$/.test(value)) throw new Error('invalid target/key id')
for (const value of [reviewUrl, sourceUrl]) if (new URL(value).protocol !== 'https:') throw new Error('review/source URLs must use HTTPS')
const manifest = JSON.parse(await readFile(path.join(directory, 'clat-plugin.json'), 'utf8'))
const publisher = JSON.parse(await readFile(path.join(directory, 'clat-plugin.publisher.json'), 'utf8'))
function clat(...words) {
  const result = spawnSync(executable, ['plugin', ...words], { stdio: 'inherit' })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error('CLAT package validation failed')
}
clat('inspect', directory)
await mkdir(path.dirname(destination), { recursive: true })
await mkdir(destination)
await mkdir(path.join(destination, 'packages'), { recursive: true })
const name = `${manifest.id}-${manifest.version}-${target}.clatpkg`
const bundle = path.join(destination, 'packages', name)
clat('pack', directory, '--output', bundle)
const bytes = await readFile(bundle)
const now = Math.floor(Date.now() / 1000)
const version = {
  version: manifest.version, runtime: manifest.runtime.kind, publisher: publisher.publisher,
  publisherKey: keyId, publishedAtUnix: now, capabilities: manifest.capabilities,
  dependencies: {}, compatibility: manifest.compatibility?.kind?.startsWith('dsh-')
    ? { dshRevision: manifest.compatibility.revision } : {}, yanked: false,
  artifacts: [{ target, url: `packages/${name}`, sha256: createHash('sha256').update(bytes).digest('hex'), bytes: (await stat(bundle)).size }],
}
const proposal = {
  schemaVersion: 1, market: { id: 'cn.at.pi', name: 'CLAT Plugin Index', homepage: 'https://pi.at.cn' },
  publishers: [{ id: publisher.publisher, name: publisher.publisher, status: 'trusted', reviewUrl,
    keys: [{ id: keyId, publicKey: publisher.publicKey, status: 'active', notBeforeUnix: now - 60, notAfterUnix: now + 365 * 86400 }] }],
  packages: [{ id: manifest.id, name: manifest.name, summary: manifest.description || manifest.name,
    homepage: sourceUrl, tags: [manifest.runtime.kind === 'mcp-stdio' ? 'MCP' : 'WASM'], versions: [version] }], revocations: [], vulnerabilities: [],
}
await writeFile(path.join(destination, 'index.source.proposed.json'), JSON.stringify(proposal, null, 2) + '\n', { flag: 'wx' })
await writeFile(path.join(destination, 'catalog.proposed.json'), JSON.stringify({ schemaVersion: 1,
  market: { name: 'CLAT Plugin Index', homepage: 'https://pi.at.cn' }, packages: [{
    id: manifest.id, name: manifest.name, runtime: manifest.runtime.kind, status: 'available',
    summary: manifest.description || manifest.name, publisher: publisher.publisher,
    tags: [manifest.runtime.kind === 'mcp-stdio' ? 'MCP' : 'WASM'], sourceUrl, docsUrl: sourceUrl,
    installCommand: `clat plugin market install ${manifest.id} --accept-capabilities`,
  }] }, null, 2) + '\n', { flag: 'wx' })
console.log(`Publication proposal: ${destination}. Review publisher identity before owner-signing an index.`)
