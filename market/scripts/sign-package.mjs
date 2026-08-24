import { createHash } from 'node:crypto'
import { mkdtemp, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { basename, join, relative, resolve, sep } from 'node:path'

function option(name) {
  const index = process.argv.indexOf(name)
  if (index < 0 || !process.argv[index + 1]) throw new Error(`${name} is required`)
  return process.argv[index + 1]
}

const packageRoot = resolve(option('--package'))
const publisher = option('--publisher')
const publicKeyFile = resolve(option('--public-key'))
const secretKeyFile = resolve(option('--minisign-key'))
if (!/^[a-z0-9][a-z0-9._-]{0,127}$/.test(publisher)) throw new Error('invalid publisher id')

const publicKey = (await readFile(publicKeyFile, 'utf8'))
  .split('\n')
  .map((line) => line.trim())
  .find((line) => line && !line.startsWith('untrusted comment:'))
if (!publicKey) throw new Error('public key file has no key line')

const manifest = JSON.parse(await readFile(join(packageRoot, 'clat-plugin.json'), 'utf8'))
if (!manifest.id || !manifest.version) throw new Error('package manifest has no id/version')
const publisherPath = join(packageRoot, 'clat-plugin.publisher.json')
const publisherTemp = join(packageRoot, `.publisher-${process.pid}.tmp`)
await writeFile(publisherTemp, `${JSON.stringify({ schemaVersion: 1, publisher, publicKey })}\n`, { flag: 'wx', mode: 0o600 })
await rename(publisherTemp, publisherPath)

const files = []
async function walk(directory, depth = 0) {
  if (depth > 32) throw new Error('package depth exceeds 32')
  const entries = await readdir(directory, { withFileTypes: true })
  entries.sort((left, right) => left.name.localeCompare(right.name, 'en'))
  for (const entry of entries) {
    const absolute = join(directory, entry.name)
    const name = relative(packageRoot, absolute).split(sep).join('/')
    if (name === 'clat-plugin.minisig') continue
    if (entry.isSymbolicLink()) throw new Error(`symbolic link is forbidden: ${name}`)
    if (entry.isDirectory()) {
      await walk(absolute, depth + 1)
      continue
    }
    if (!entry.isFile()) throw new Error(`special file is forbidden: ${name}`)
    if (files.length >= 4096) throw new Error('package exceeds 4096 files')
    const metadata = await stat(absolute)
    if (metadata.size > 256 * 1024 * 1024) throw new Error(`file is too large: ${name}`)
    const body = await readFile(absolute)
    files.push({ name, bytes: body.length, sha256: createHash('sha256').update(body).digest('hex') })
  }
}
await walk(packageRoot)
files.sort((left, right) => Buffer.from(left.name).compare(Buffer.from(right.name)))

const tree = createHash('sha256')
tree.update(Buffer.from('clat-package-tree-v1\0'))
for (const file of files) {
  const pathBytes = Buffer.from(file.name)
  const pathLength = Buffer.alloc(8)
  const size = Buffer.alloc(8)
  pathLength.writeBigUInt64BE(BigInt(pathBytes.length))
  size.writeBigUInt64BE(BigInt(file.bytes))
  tree.update(pathLength)
  tree.update(pathBytes)
  tree.update(size)
  tree.update(file.sha256)
}
const message = `clat-plugin-signature-v1\npublisher:${publisher}\npublicKey:${publicKey}\n` +
  `id:${manifest.id}\nversion:${manifest.version}\ncontentSha256:${tree.digest('hex')}\n`

const scratch = await mkdtemp(join(tmpdir(), 'clat-market-sign-'))
try {
  const messagePath = join(scratch, 'message')
  const signaturePath = join(packageRoot, 'clat-plugin.minisig')
  await writeFile(messagePath, message, { mode: 0o600 })
  const signed = spawnSync(
    'minisign',
    ['-S', '-s', secretKeyFile, '-m', messagePath, '-x', signaturePath, '-t', `CLAT plugin ${manifest.id} ${manifest.version}`],
    { cwd: packageRoot, stdio: 'inherit' }
  )
  if (signed.error) throw signed.error
  if (signed.status !== 0) throw new Error(`minisign exited ${signed.status}`)
  console.log(`signed ${basename(packageRoot)} as ${publisher}/${manifest.id}@${manifest.version}`)
} finally {
  await rm(scratch, { recursive: true, force: true })
}
