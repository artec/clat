#!/usr/bin/env node
import { createHash, randomUUID } from 'node:crypto'
import { spawn } from 'node:child_process'
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  inspectDshPackageEntry,
  scanDshCompatibility,
  writeCompatibilityMatrix,
  type PackageCompatibility,
} from './scanner.js'

interface PortPlan {
  schemaVersion: 1
  id: string
  name: string
  version: string
  sourceRoot: string
  sourceEntry: string
  wrapper: string
  compatibility: PackageCompatibility
  dshRevision?: string
}

interface CliResult { code: number; stdout?: string; stderr?: string }

const MAX_CONFIG_BYTES = 64 * 1024
const SMOKE_TIMEOUT_MS = 15_000

function usage(): string {
  return `Usage: clat-dsh <COMMAND>\n\n` +
    `Commands:\n` +
    `  scan <checkout> [--output matrix.json]            Semantic checkout matrix\n` +
    `  inspect <package>                                 Inspect one DSH package\n` +
    `  port <package> --out <dir> [--id <id>] [--force] Generate adapter scaffold\n` +
    `  test <port-dir> [--bun <path>]                    MCP smoke-test scaffold\n` +
    `  package <port-dir> --out <dir> [OPTIONS]          Build executable CLAT package\n\n` +
    `Package options:\n` +
    `  --bun <path>                                      Bun executable (default: bun)\n` +
    `  --allow-partial                                   Package despite unsupported seams\n` +
    `  --publisher <id>                                  Self-asserted publisher id\n` +
    `  --publisher-key <path>                            Minisign public key file\n` +
    `  --minisign-key <path>                             Minisign secret key file\n` +
    `  --force                                           Replace a non-empty output directory\n`
}

function sanitizeId(name: string): string {
  const normalized = name.toLowerCase()
    .replace(/^@/, '')
    .replace(/[^a-z0-9._-]+/g, '.')
    .replace(/^[._-]+|[._-]+$/g, '')
  return `dsh.${normalized || 'plugin'}`.slice(0, 128)
}

function parseOptions(args: string[]): { positional: string[]; options: Map<string, string | true> } {
  const positional: string[] = []
  const options = new Map<string, string | true>()
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]
    if (argument === undefined) continue
    if (!argument.startsWith('--')) {
      positional.push(argument)
      continue
    }
    if (argument === '--force' || argument === '--allow-partial') {
      options.set(argument, true)
      continue
    }
    if (!['--out', '--id', '--bun', '--output', '--publisher', '--publisher-key', '--minisign-key'].includes(argument)) {
      throw new Error(`unknown option ${argument}`)
    }
    const value = args[index + 1]
    if (value === undefined) throw new Error(`${argument} requires a value`)
    options.set(argument, value)
    index += 1
  }
  return { positional, options }
}

function optionString(options: Map<string, string | true>, name: string): string | undefined {
  const value = options.get(name)
  return typeof value === 'string' ? value : undefined
}

async function packageMetadata(root: string): Promise<{ name: string; version: string; entry: string }> {
  const packageJson = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8')) as {
    name?: unknown
    version?: unknown
    module?: unknown
    main?: unknown
    exports?: unknown
  }
  const name = typeof packageJson.name === 'string' ? packageJson.name : path.basename(root)
  const version = typeof packageJson.version === 'string' && packageJson.version !== ''
    ? packageJson.version
    : '0.0.0'
  const exportRoot = packageJson.exports !== null && typeof packageJson.exports === 'object'
    ? (packageJson.exports as Record<string, unknown>)['.']
    : packageJson.exports
  const canonicalRoot = await real(root)
  const candidates = [
    firstString(exportRoot, packageJson.module, packageJson.main),
    'src/index.ts', 'src/index.tsx', 'src/index.js', 'index.ts', 'index.js',
  ].filter((candidate): candidate is string => candidate !== undefined)
  for (const entry of [...new Set(candidates)]) {
    try {
      const canonicalEntry = await real(path.resolve(root, entry))
      if (isWithin(canonicalRoot, canonicalEntry) && (await stat(canonicalEntry)).isFile()) {
        return { name, version, entry: canonicalEntry }
      }
    } catch { /* try source fallback */ }
  }
  throw new Error(`cannot determine an existing package entry (tried: ${candidates.join(', ')})`)
}

function firstString(...values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value === 'string') return value
    if (value !== null && typeof value === 'object') {
      const record = value as Record<string, unknown>
      for (const key of ['import', 'default', 'node']) {
        if (typeof record[key] === 'string') return record[key]
      }
    }
  }
  return undefined
}

async function real(target: string): Promise<string> {
  const { realpath } = await import('node:fs/promises')
  return await realpath(target)
}

function isWithin(parent: string, child: string): boolean {
  const relative = path.relative(parent, child)
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative))
}

async function prepareOutput(output: string, force: boolean): Promise<void> {
  try {
    const entries = await readdir(output)
    if (entries.length > 0) {
      if (!force) throw new Error(`output directory is not empty: ${output} (use --force)`)
      await rm(output, { recursive: true, force: true })
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
  }
  await mkdir(output, { recursive: true })
}

async function preparePackageTarget(output: string, force: boolean): Promise<boolean> {
  await mkdir(path.dirname(output), { recursive: true })
  try {
    const entries = await readdir(output)
    if (entries.length > 0 && !force) {
      throw new Error(`output directory is not empty: ${output} (use --force)`)
    }
    return true
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false
    throw error
  }
}

async function publishPackageOutput(staging: string, output: string, existing: boolean): Promise<void> {
  if (!existing) {
    await rename(staging, output)
    return
  }
  const backup = path.join(path.dirname(output), `.clat-package-backup-${randomUUID()}`)
  await rename(output, backup)
  try {
    await rename(staging, output)
  } catch (error) {
    await rename(backup, output)
    throw error
  }
  await rm(backup, { recursive: true, force: false })
}

async function inspectPackage(rootInput: string): Promise<{
  root: string
  metadata: Awaited<ReturnType<typeof packageMetadata>>
  compatibility: PackageCompatibility
  revision?: string
}> {
  const root = await real(path.resolve(rootInput))
  const metadata = await packageMetadata(root)
  const compatibility = await inspectDshPackageEntry(root, metadata.entry, metadata.name)
  if (compatibility.status === 'not-plugin') {
    throw new Error(`${metadata.name} has no source-grounded DSH plugin entry`)
  }
  return { root, metadata, compatibility, revision: undefined }
}

function wrapperSource(plan: PortPlan): string {
  let sourceImport = path.relative(path.dirname(plan.wrapper), plan.sourceEntry).split(path.sep).join('/')
  if (!sourceImport.startsWith('.')) sourceImport = `./${sourceImport}`
  return `#!/usr/bin/env node\n` +
    `import { serveClat } from '@artec/clat-dsh-adapter'\n` +
    `import * as source from ${JSON.stringify(sourceImport)}\n\n` +
    `const plugin = source.default ?? source\n` +
    `const rawConfig = process.env.CLAT_PLUGIN_CONFIG\n` +
    `if (rawConfig !== undefined && Buffer.byteLength(rawConfig) > ${MAX_CONFIG_BYTES}) {\n` +
    `  throw new Error('CLAT_PLUGIN_CONFIG exceeds ${MAX_CONFIG_BYTES} bytes')\n` +
    `}\n` +
    `const config = rawConfig === undefined || rawConfig === '' ? {} : JSON.parse(rawConfig)\n` +
    `await serveClat(plugin, { name: ${JSON.stringify(plan.id)}, version: ${JSON.stringify(plan.version)}, config })\n`
}

function inferredCapabilities(compatibility: PackageCompatibility): Record<string, unknown> {
  const seams = new Set(compatibility.seams)
  const hostTools = new Set<string>()
  if (seams.has('fs')) {
    for (const tool of ['list_files', 'read_file', 'search', 'write_file', 'edit_file']) hostTools.add(tool)
  }
  if (seams.has('shell')) hostTools.add('run_command')
  return {
    tools: seams.has('tools') || seams.has('web'),
    prompts: seams.has('systemPrompt'),
    sampling: seams.has('llm'),
    elicitation: seams.has('userQuestions'),
    hostContext: seams.has('clat') || seams.has('sessions') || seams.has('agents'),
    hostTools: [...hostTools].sort(),
  }
}

async function sha256(file: string): Promise<string> {
  const bytes = await readFile(file)
  return createHash('sha256').update(bytes).digest('hex')
}

async function packageTreeDigest(root: string, excluded?: string): Promise<string> {
  const files: Array<{ relative: string; bytes: number; sha256: string }> = []
  const walk = async (directory: string): Promise<void> => {
    const entries = await readdir(directory, { withFileTypes: true })
    entries.sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : 0)
    for (const entry of entries) {
      const absolute = path.join(directory, entry.name)
      const metadata = await lstat(absolute)
      if (metadata.isSymbolicLink()) throw new Error(`package signing refuses symlink ${absolute}`)
      if (metadata.isDirectory()) { await walk(absolute); continue }
      if (!metadata.isFile()) throw new Error(`package signing refuses special file ${absolute}`)
      const relative = path.relative(root, absolute).split(path.sep).join('/')
      if (relative === excluded) continue
      files.push({ relative, bytes: metadata.size, sha256: await sha256(absolute) })
    }
  }
  await walk(root)
  files.sort((left, right) => left.relative < right.relative ? -1 : left.relative > right.relative ? 1 : 0)
  const digest = createHash('sha256')
  digest.update(Buffer.from('clat-package-tree-v1\0'))
  for (const file of files) {
    const relative = Buffer.from(file.relative)
    const pathLength = Buffer.alloc(8)
    pathLength.writeBigUInt64BE(BigInt(relative.byteLength))
    const size = Buffer.alloc(8)
    size.writeBigUInt64BE(BigInt(file.bytes))
    digest.update(pathLength)
    digest.update(relative)
    digest.update(size)
    digest.update(file.sha256)
  }
  return digest.digest('hex')
}

async function signPackage(
  output: string,
  manifest: { id: string; version: string },
  publisher: string,
  publicKeyFile: string,
  secretKeyFile: string,
): Promise<void> {
  if (!/^[a-z0-9][a-z0-9._-]{0,127}$/.test(publisher)) {
    throw new Error('publisher id must be lowercase ASCII [a-z0-9._-]')
  }
  const publicKey = (await readFile(publicKeyFile, 'utf8'))
    .split('\n')
    .map(line => line.trim())
    .find(line => line !== '' && !line.startsWith('untrusted comment:'))
  if (publicKey === undefined) throw new Error('publisher public key file has no key line')
  await writeFile(path.join(output, 'clat-plugin.publisher.json'), `${JSON.stringify({
    schemaVersion: 1,
    publisher,
    publicKey,
  }, null, 2)}\n`, 'utf8')
  const contentSha256 = await packageTreeDigest(output, 'clat-plugin.minisig')
  const message = `clat-plugin-signature-v1\npublisher:${publisher}\npublicKey:${publicKey}\n` +
    `id:${manifest.id}\nversion:${manifest.version}\ncontentSha256:${contentSha256}\n`
  const scratch = await mkdtemp(path.join(os.tmpdir(), 'clat-plugin-sign-'))
  try {
    const messageFile = path.join(scratch, 'message')
    await writeFile(messageFile, message, 'utf8')
    await runProcess('minisign', [
      '-S', '-s', path.resolve(secretKeyFile), '-m', messageFile,
      '-x', path.join(output, 'clat-plugin.minisig'),
      '-t', `CLAT plugin ${manifest.id} ${manifest.version}`,
    ], output, 30_000)
  } finally {
    await rm(scratch, { recursive: true, force: true })
  }
}

async function runProcess(command: string, args: string[], cwd: string, timeoutMs: number): Promise<{ stdout: string; stderr: string }> {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    const append = (current: string, chunk: Buffer) => (current + chunk.toString('utf8')).slice(-64 * 1024)
    child.stdout.on('data', chunk => { stdout = append(stdout, chunk as Buffer) })
    child.stderr.on('data', chunk => { stderr = append(stderr, chunk as Buffer) })
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      reject(new Error(`${command} timed out after ${timeoutMs}ms`))
    }, timeoutMs)
    child.once('error', error => { clearTimeout(timer); reject(error) })
    child.once('exit', code => {
      clearTimeout(timer)
      if (code === 0) resolve({ stdout, stderr })
      else reject(new Error(`${command} exited ${String(code)}\n${stderr}`))
    })
  })
}

const BUN_BUILD_DRIVER = `
import path from 'node:path'
import { existsSync, readFileSync } from 'node:fs'
const [wrapper, outfile] = process.argv.slice(2)
if (!wrapper || !outfile) throw new Error('build driver requires wrapper and outfile')
const packageJsonPlugin = {
  name: 'clat-static-package-json',
  setup(build) {
    build.onLoad({ filter: /\\.[cm]?[jt]sx?$/ }, async (args) => {
      let contents = await Bun.file(args.path).text()
      const pattern = /createRequire\\(import\\.meta\\.url\\)\\((["'])([^"']*package\\.json)\\1\\)/g
      for (const match of [...contents.matchAll(pattern)]) {
        const request = match[2]
        if (!request) continue
        const packagePath = path.resolve(path.dirname(args.path), request)
        if (!existsSync(packagePath)) continue
        const value = JSON.parse(readFileSync(packagePath, 'utf8'))
        contents = contents.replace(match[0], '(' + JSON.stringify(value) + ')')
      }
      const extension = path.extname(args.path)
      const loader = extension === '.ts' || extension === '.mts' || extension === '.cts'
        ? 'ts' : extension === '.tsx' ? 'tsx' : extension === '.jsx' ? 'jsx' : 'js'
      return { contents, loader }
    })
  },
}
const result = await Bun.build({
  entrypoints: [wrapper],
  target: 'bun',
  plugins: [packageJsonPlugin],
  compile: { outfile },
})
if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}
`

async function compileWithBun(bun: string, wrapper: string, entry: string, cwd: string): Promise<void> {
  const scratch = await mkdtemp(path.join(os.tmpdir(), 'clat-dsh-bun-build-'))
  try {
    const driver = path.join(scratch, 'build.mjs')
    await writeFile(driver, BUN_BUILD_DRIVER, 'utf8')
    await runProcess(bun, [driver, wrapper, entry], cwd, 120_000)
  } finally {
    await rm(scratch, { recursive: true, force: true })
  }
}

async function smoke(command: string, args: string[], cwd: string): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ['pipe', 'pipe', 'pipe'] })
    let stdout = ''
    let stderr = ''
    let initialized = false
    const timer = setTimeout(() => {
      child.kill('SIGKILL')
      reject(new Error(`MCP smoke test timed out\n${stderr}`))
    }, SMOKE_TIMEOUT_MS)
    const finish = (error?: Error) => {
      clearTimeout(timer)
      child.kill('SIGTERM')
      if (error === undefined) resolve()
      else reject(error)
    }
    child.stderr.on('data', chunk => { stderr = (stderr + String(chunk)).slice(-64 * 1024) })
    child.stdout.on('data', chunk => {
      stdout += String(chunk)
      for (;;) {
        const newline = stdout.indexOf('\n')
        if (newline < 0) break
        const line = stdout.slice(0, newline).trim()
        stdout = stdout.slice(newline + 1)
        if (line === '') continue
        let frame: { id?: number; result?: unknown; error?: unknown }
        try { frame = JSON.parse(line) as typeof frame } catch { finish(new Error(`invalid MCP frame: ${line}`)); return }
        if (frame.error !== undefined) { finish(new Error(`MCP smoke error: ${JSON.stringify(frame.error)}`)); return }
        if (frame.id === 1) {
          initialized = true
          child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} })}\n`)
        } else if (frame.id === 2 && initialized) {
          child.stdin.end()
          finish()
        }
      }
    })
    child.once('error', error => finish(error))
    child.once('exit', code => {
      if (!initialized) finish(new Error(`MCP process exited ${String(code)} before initialize\n${stderr}`))
    })
    child.stdin.write(`${JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'clat-dsh-test', version: '1' } },
    })}\n`)
  })
}

async function commandScan(positional: string[], options: Map<string, string | true>): Promise<CliResult> {
  const root = positional[0]
  if (root === undefined || positional.length !== 1) throw new Error('scan requires one checkout path')
  const matrix = await scanDshCompatibility(root)
  const output = optionString(options, '--output')
  if (output !== undefined) await writeCompatibilityMatrix(matrix, output)
  return { code: 0, stdout: output === undefined ? `${JSON.stringify(matrix, null, 2)}\n` : `${output}\n` }
}

async function commandInspect(positional: string[]): Promise<CliResult> {
  const root = positional[0]
  if (root === undefined || positional.length !== 1) throw new Error('inspect requires one package path')
  const inspected = await inspectPackage(root)
  return { code: 0, stdout: `${JSON.stringify(inspected, null, 2)}\n` }
}

async function commandPort(positional: string[], options: Map<string, string | true>): Promise<CliResult> {
  const source = positional[0]
  const outOption = optionString(options, '--out')
  if (source === undefined || positional.length !== 1 || outOption === undefined) {
    throw new Error('port requires one package path and --out <dir>')
  }
  const inspected = await inspectPackage(source)
  const output = path.resolve(outOption)
  const wrapper = path.join(output, 'clat.mjs')
  const id = optionString(options, '--id') ?? sanitizeId(inspected.metadata.name)
  if (!/^[a-z0-9][a-z0-9._-]{0,127}$/.test(id)) {
    throw new Error('CLAT plugin id must be lowercase ASCII [a-z0-9._-] and at most 128 bytes')
  }
  if (inspected.metadata.name.length > 128 || /[\u0000-\u001f\u007f]/.test(inspected.metadata.name)) {
    throw new Error('package display name contains controls or exceeds 128 characters')
  }
  if (inspected.metadata.version.length > 64 || /\s/.test(inspected.metadata.version)) {
    throw new Error('package version contains whitespace or exceeds 64 characters')
  }
  await prepareOutput(output, options.get('--force') === true)
  const plan: PortPlan = {
    schemaVersion: 1,
    id,
    name: inspected.metadata.name,
    version: inspected.metadata.version,
    sourceRoot: inspected.root,
    sourceEntry: inspected.metadata.entry,
    wrapper,
    compatibility: inspected.compatibility,
    dshRevision: inspected.revision,
  }
  await writeFile(wrapper, wrapperSource(plan), { encoding: 'utf8', mode: 0o755 })
  await writeFile(path.join(output, 'clat-port.json'), `${JSON.stringify(plan, null, 2)}\n`, 'utf8')
  await writeFile(path.join(output, 'compatibility.json'), `${JSON.stringify({
    schemaVersion: 2,
    source: { package: plan.name, revision: plan.dshRevision },
    compatibility: plan.compatibility,
  }, null, 2)}\n`, 'utf8')
  const warning = plan.compatibility.unsupportedSeams.length === 0
    ? ''
    : `; TODO unsupported=${plan.compatibility.unsupportedSeams.join(',')}`
  return { code: 0, stdout: `ported ${plan.name} -> ${output}${warning}\n` }
}

async function readPlan(portDir: string): Promise<PortPlan> {
  const plan = JSON.parse(await readFile(path.join(portDir, 'clat-port.json'), 'utf8')) as PortPlan
  if (plan.schemaVersion !== 1 || typeof plan.id !== 'string' || typeof plan.wrapper !== 'string') {
    throw new Error('invalid clat-port.json')
  }
  return plan
}

async function commandTest(positional: string[], options: Map<string, string | true>): Promise<CliResult> {
  const portDir = positional[0]
  if (portDir === undefined || positional.length !== 1) throw new Error('test requires one port directory')
  const root = path.resolve(portDir)
  const plan = await readPlan(root)
  const bun = optionString(options, '--bun') ?? 'bun'
  await smoke(bun, [plan.wrapper], root)
  return { code: 0, stdout: `MCP smoke test passed: ${plan.id}\n` }
}

async function commandPackage(positional: string[], options: Map<string, string | true>): Promise<CliResult> {
  const portDir = positional[0]
  const outOption = optionString(options, '--out')
  if (portDir === undefined || positional.length !== 1 || outOption === undefined) {
    throw new Error('package requires one port directory and --out <dir>')
  }
  const root = path.resolve(portDir)
  const plan = await readPlan(root)
  if (plan.compatibility.unsupportedSeams.length > 0 && options.get('--allow-partial') !== true) {
    throw new Error(`port has unsupported seams: ${plan.compatibility.unsupportedSeams.join(', ')} (use --allow-partial only after review)`)
  }
  const output = path.resolve(outOption)
  const outputExists = await preparePackageTarget(output, options.get('--force') === true)
  const staging = await mkdtemp(path.join(path.dirname(output), '.clat-package-staging-'))
  const entryName = process.platform === 'win32' ? 'plugin.exe' : 'plugin'
  const entry = path.join(staging, entryName)
  const bun = optionString(options, '--bun') ?? 'bun'
  try {
    await compileWithBun(bun, plan.wrapper, entry, root)
    if (process.platform !== 'win32') await chmod(entry, 0o755)
    const digest = await sha256(entry)
    const manifest = {
      manifestVersion: 1,
      id: plan.id,
      name: plan.name,
      version: plan.version,
      description: `DSH-compatible package generated by @artec/clat-dsh-adapter`,
      runtime: { kind: 'mcp-stdio', entry: entryName, sha256: digest, args: [] },
      capabilities: inferredCapabilities(plan.compatibility),
      configSchema: { type: 'object' },
      compatibility: { kind: 'dsh', ...(plan.dshRevision === undefined ? {} : { revision: plan.dshRevision }) },
    }
    await writeFile(path.join(staging, 'clat-plugin.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')
    await writeFile(path.join(staging, 'compatibility.json'), `${JSON.stringify({
      schemaVersion: 2,
      source: { package: plan.name, revision: plan.dshRevision },
      compatibility: plan.compatibility,
    }, null, 2)}\n`, 'utf8')
    const signing = [
      optionString(options, '--publisher'),
      optionString(options, '--publisher-key'),
      optionString(options, '--minisign-key'),
    ]
    if (signing.some(value => value !== undefined)) {
      if (signing.some(value => value === undefined)) {
        throw new Error('signing requires --publisher, --publisher-key, and --minisign-key together')
      }
      await signPackage(
        staging,
        manifest,
        signing[0] as string,
        signing[1] as string,
        signing[2] as string,
      )
    }
    await smoke(entry, [], staging)
    await publishPackageOutput(staging, output, outputExists)
    return { code: 0, stdout: `packaged ${plan.id} -> ${output}\nsha256 ${digest}\n` }
  } finally {
    await rm(staging, { recursive: true, force: true })
  }
}

export async function runDshCli(argv: string[]): Promise<CliResult> {
  const command = argv[0]
  if (command === undefined || command === '-h' || command === '--help' || command === 'help') {
    return { code: 0, stdout: usage() }
  }
  try {
    const { positional, options } = parseOptions(argv.slice(1))
    switch (command) {
      case 'scan': return await commandScan(positional, options)
      case 'inspect': return await commandInspect(positional)
      case 'port': return await commandPort(positional, options)
      case 'test': return await commandTest(positional, options)
      case 'package': return await commandPackage(positional, options)
      default: throw new Error(`unknown command ${command}`)
    }
  } catch (error) {
    return { code: 1, stderr: `${error instanceof Error ? error.message : String(error)}\n` }
  }
}

async function main(): Promise<void> {
  const result = await runDshCli(process.argv.slice(2))
  if (result.stdout !== undefined) process.stdout.write(result.stdout)
  if (result.stderr !== undefined) process.stderr.write(result.stderr)
  process.exitCode = result.code
}

const invoked = process.argv[1] !== undefined
  && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
if (invoked) void main()

// Exported only for black-box tests that need isolated scratch directories.
export async function temporaryPortRoot(): Promise<string> {
  return await mkdtemp(path.join(os.tmpdir(), 'clat-dsh-port-'))
}
