import { createHash } from 'node:crypto'
import { cp, mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import { resolve } from 'node:path'
import './validate.mjs'

const root = resolve(import.meta.dirname, '..')
const dist = resolve(root, 'dist')
const keyFlag = process.argv.indexOf('--minisign-key')
if (keyFlag < 0 || !process.argv[keyFlag + 1]) {
  throw new Error('usage: npm run release-index -- --minisign-key /secure/path/minisign.key')
}
const secretKey = resolve(process.argv[keyFlag + 1])
const now = Math.floor(Date.now() / 1000)
const source = JSON.parse(await readFile(resolve(root, 'index.source.json'), 'utf8'))
const index = {
  ...source,
  market: {
    ...source.market,
    generatedAtUnix: now,
    expiresAtUnix: now + 7 * 24 * 60 * 60
  }
}

for (const plugin of index.packages) {
  for (const version of plugin.versions) {
    for (const artifact of version.artifacts) {
      if (/^[a-z]+:/i.test(artifact.url)) continue
      if (!/^packages\/[A-Za-z0-9._-]+\.clatpkg$/.test(artifact.url)) {
        throw new Error(`unsafe relative artifact URL: ${artifact.url}`)
      }
      const artifactPath = resolve(dist, artifact.url)
      const metadata = await stat(artifactPath)
      const bytes = await readFile(artifactPath)
      const digest = createHash('sha256').update(bytes).digest('hex')
      if (metadata.size !== artifact.bytes || digest.toLowerCase() !== artifact.sha256.toLowerCase()) {
        throw new Error(`artifact metadata mismatch: ${plugin.id}@${version.version} ${artifact.url}`)
      }
    }
  }
}

await mkdir(dist, { recursive: true })
for (const file of ['index.html', 'style.css', 'app.js', 'catalog.json', '_headers']) {
  await cp(resolve(root, file), resolve(dist, file))
}
const indexPath = resolve(dist, 'index.json')
const signaturePath = resolve(dist, 'index.json.minisig')
await writeFile(indexPath, `${JSON.stringify(index)}\n`, { flag: 'wx' }).catch(async (error) => {
  if (error.code !== 'EEXIST') throw error
  await writeFile(indexPath, `${JSON.stringify(index)}\n`)
})
const comment = `CLAT plugin index ${index.market.id} generated ${now}`
const signed = spawnSync(
  'minisign',
  ['-S', '-s', secretKey, '-m', indexPath, '-x', signaturePath, '-t', comment],
  { stdio: 'inherit' }
)
if (signed.error) throw signed.error
if (signed.status !== 0) throw new Error(`minisign exited ${signed.status}`)

console.log(`signed market index: ${indexPath}`)
console.log(`trusted comment: ${comment}`)
