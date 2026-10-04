import { build } from 'esbuild'
import { componentize } from '@bytecodealliance/componentize-js'
import { componentWit } from '@bytecodealliance/jco'
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { lowerUnicodeProperties } from './regex-lowering.mjs'

export const root = import.meta.dirname
export const repo = path.resolve(root, '../..')
const witPath = path.join(repo, 'wit/plugin.wit')
const features = ['stdio', 'random', 'clocks', 'http', 'fetch-event']

export async function bundle(entry, external = ['clat:plugin/*']) {
  return build({
    absWorkingDir: root, entryPoints: [entry], bundle: true,
    format: 'esm', platform: 'neutral', target: 'es2022',
    external, write: false, metafile: true, logLevel: 'silent',
    conditions: ['import', 'default'], mainFields: ['module', 'main'],
  })
}

export async function compile(source, out) {
  const sourcePath = path.join(out, 'input.mjs')
  await writeFile(sourcePath, source)
  // Wizer only gets this explicit empty environment; no credentials in snapshot.
  const { component } = await componentize({
    sourcePath, witPath, worldName: 'plugin', disableFeatures: features, env: {},
  })
  return { component, wit: await componentWit(component) }
}

export async function engineProbe(out) {
  const result = await bundle('engine-probe.mjs')
  const lowered = lowerUnicodeProperties(result.outputFiles[0].text)
  await writeFile(path.join(out, 'engine-input.mjs'), lowered.code)
  const { component, wit } = await compile(lowered.code, out)
  await writeFile(path.join(out, 'engine-probe.wasm'), component)
  await writeFile(path.join(out, 'engine-probe.wit'), wit)
  return { bytes: component.length, sha256: createHash('sha256').update(component).digest('hex'), wit,
    regexLowering: { literals: lowered.literals, unicodeVersion: lowered.unicodeVersion } }
}

export async function loweredOriginalAttempt(out) {
  const graph = await bundle('official-entry.mjs', ['clat:plugin/*', 'node:*', 'undici'])
  const lowered = lowerUnicodeProperties(graph.outputFiles[0].text)
  await writeFile(path.join(out, 'official-lowered.mjs'), lowered.code)
  let componentFailure
  try { await compile(lowered.code, out) }
  catch (error) { componentFailure = String(error.message) }
  if (!componentFailure) throw new Error('Unexpected lowered component success; reassess missing imports')
  await writeFile(path.join(out, 'official-lowered-error.txt'), componentFailure + '\n')
  return { literals: lowered.literals, unicodeVersion: lowered.unicodeVersion, componentFailure }
}

export async function strictOriginalAttempt(out) {
  let diagnostics
  try {
    await bundle('official-entry.mjs')
    throw new Error('Unexpected strict bundling success; reassess K2 instead of assuming failure')
  } catch (error) {
    if (!Array.isArray(error.errors)) throw error
    diagnostics = error.errors.map(item => ({ text: item.text, file: item.location?.file, line: item.location?.line }))
  }
  await writeFile(path.join(out, 'official-bundle-errors.json'), JSON.stringify(diagnostics, null, 2) + '\n')
  // A second attempt preserves unresolved imports rather than substituting fake APIs.
  const graph = await bundle('official-entry.mjs', ['clat:plugin/*', 'node:*', 'undici'])
  const external = Object.values(graph.metafile.outputs).flatMap(output => output.imports).map(item => item.path)
  await writeFile(path.join(out, 'official-unresolved.mjs'), graph.outputFiles[0].text)
  await writeFile(path.join(out, 'official-metafile.json'), JSON.stringify(graph.metafile, null, 2) + '\n')
  let componentFailure
  try { await compile(graph.outputFiles[0].text, out) }
  catch (error) { componentFailure = String(error.message) }
  if (!componentFailure) throw new Error('Unexpected original componentization success; reassess K2')
  await writeFile(path.join(out, 'official-component-error.txt'), componentFailure + '\n')
  return { diagnostics, external, componentFailure }
}

export async function builtinAttempt(out) {
  const source = (await bundle('builtin-probe.mjs', ['node:*'])).outputFiles[0].text
  let failure
  try { await compile(source, out) } catch (error) { failure = String(error.message) }
  if (!failure) throw new Error('Unexpected Node built-in support; reassess K2')
  await writeFile(path.join(out, 'async-context-component-error.txt'), failure + '\n')
  return failure
}

export async function originalInventory() {
  const names = ['dsh-web', 'dsh-web-search-deepseek', 'dsh-web-fetch-http', 'dsh-tool-web', 'dsh-tools']
  const inventory = []
  for (const name of names) {
    const directory = path.join(repo, 'sdk/dsh-adapter/examples/official-web/node_modules/@deepseek-ai', name)
    const metadata = JSON.parse(await readFile(path.join(directory, 'package.json'), 'utf8'))
    const bytes = await readFile(path.join(directory, 'lib/index.js'))
    inventory.push({ name: metadata.name, version: metadata.version, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') })
  }
  for (const entry of ['shim.ts', 'events.ts', 'host-services.ts']) {
    const bytes = await readFile(path.join(repo, 'sdk/dsh-adapter/src', entry))
    inventory.push({ name: 'adapter/' + entry, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') })
  }
  return inventory
}
