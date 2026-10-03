import { notices } from './notices.mjs'
import { compileWithBun, smoke } from '../../dist/src/dsh-cli.js'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rename, rm, writeFile, lstat } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.dirname(fileURLToPath(import.meta.url))
const args = process.argv.slice(2)
if (args.length !== 2 || args[0] !== '--out') {
  throw new Error('usage: npm run package -- --out /absolute/new-package-directory')
}
const destination = path.resolve(args[1])
const exists = await lstat(destination).then(() => true, error => {
  if (error.code === 'ENOENT') return false
  throw error
})
if (exists) throw new Error('output already exists; choose a new directory')
await mkdir(path.dirname(destination), { recursive: true })
const staging = await mkdtemp(path.join(path.dirname(destination), '.official-web-'))
try {
  const entry = process.platform === 'win32' ? 'clat-dsh-official-web.exe' : 'clat-dsh-official-web'
  await compileWithBun('bun', path.join(root, 'bin.mjs'), path.join(staging, entry), root, true)
  await smoke(path.join(staging, entry), [], root)
  const sha256 = createHash('sha256').update(await readFile(path.join(staging, entry))).digest('hex')
  const manifest = {
    manifestVersion: 1,
    id: 'io.artec.dsh-official-web',
    name: 'DSH official web search and fetch',
    version: '0.1.0',
    description: 'Official DSH web search and HTTP fetch in one executable; no Node or Bun needed after installation.',
    runtime: { kind: 'mcp-stdio', entry, sha256 },
    capabilities: { tools: true, prompts: true },
    compatibility: { kind: 'dsh-v0.2.0-rc.2', revision: '639ed015397290b3745d163aafe02ffee4aa3f84' },
    configSchema: {
      type: 'object', additionalProperties: false,
      properties: { apiKey: { type: 'string', title: 'DeepSeek API key', writeOnly: true, description: 'Used for web search. Saved only in your local CLAT private configuration.' }, apiKeyEnv: { type: 'string', title: 'API key environment variable', description: 'Optional alternative to entering a key. Defaults to DEEPSEEK_API_KEY on the local host.' } },
    },
  }
  await writeFile(path.join(staging, 'clat-plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`)
  await writeFile(path.join(staging, 'LICENSES.txt'), await notices(root))
  await rename(staging, destination)
  console.log(`Package: ${destination}`)
} finally {
  await rm(staging, { recursive: true, force: true })
}
