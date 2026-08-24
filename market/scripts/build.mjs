import { cp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import './validate.mjs'

const root = resolve(import.meta.dirname, '..')
const dist = resolve(root, 'dist')
await rm(dist, { recursive: true, force: true })
await mkdir(dist, { recursive: true })

for (const file of ['index.html', 'style.css', 'app.js', 'catalog.json', '_headers']) {
  await cp(resolve(root, file), resolve(dist, file))
}

const packageSource = resolve(root, 'packages')
const packageFiles = await readdir(packageSource).catch((error) => {
  if (error.code === 'ENOENT') return []
  throw error
})
if (packageFiles.some((file) => file.endsWith('.clatpkg'))) {
  await mkdir(resolve(dist, 'packages'), { recursive: true })
  for (const file of packageFiles.filter((file) => file.endsWith('.clatpkg')).sort()) {
    await cp(resolve(packageSource, file), resolve(dist, 'packages', file))
  }
}

const headers = await readFile(resolve(dist, '_headers'), 'utf8')
await writeFile(resolve(dist, '_headers'), headers.replaceAll('\r\n', '\n'))
console.log(`market build: ${dist}`)
