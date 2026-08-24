import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const catalog = JSON.parse(await readFile(resolve(root, 'catalog.json'), 'utf8'))
const html = await readFile(resolve(root, 'index.html'), 'utf8')

const fail = (message) => {
  throw new Error(`market validation: ${message}`)
}

if (catalog.schemaVersion !== 1 || !Array.isArray(catalog.packages)) fail('catalog schema')
if (catalog.market?.homepage !== 'https://pi.at.cn') fail('canonical homepage')

const ids = new Set()
const validRuntime = new Set(['wasm-component', 'mcp-stdio'])
const validStatus = new Set(['preview', 'available', 'withdrawn'])
for (const plugin of catalog.packages) {
  if (!/^[a-z0-9][a-z0-9._-]{0,127}$/.test(plugin.id)) fail(`invalid id ${plugin.id}`)
  if (ids.has(plugin.id)) fail(`duplicate id ${plugin.id}`)
  ids.add(plugin.id)
  if (!validRuntime.has(plugin.runtime)) fail(`invalid runtime for ${plugin.id}`)
  if (!validStatus.has(plugin.status)) fail(`invalid status for ${plugin.id}`)
  if (!plugin.name || !plugin.summary || !plugin.publisher) fail(`incomplete record ${plugin.id}`)
  if (plugin.status !== 'available' && plugin.installCommand) {
    fail(`non-available package ${plugin.id} may not advertise an install command`)
  }
  for (const field of ['sourceUrl', 'docsUrl']) {
    if (plugin[field] && new URL(plugin[field]).protocol !== 'https:') fail(`${field} must use HTTPS`)
  }
}

for (const id of ['catalog', 'search', 'package-grid', 'package-dialog']) {
  if (!html.includes(`id="${id}"`)) fail(`index.html missing #${id}`)
}
if (/<script(?![^>]*\bsrc=)/i.test(html)) fail('inline scripts violate CSP')
if (/on(?:click|load|error)\s*=/i.test(html)) fail('inline event handler violates CSP')

console.log(`market validation: PASS (${catalog.packages.length} catalog records)`)
