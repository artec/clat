import { readFile, readdir, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'

export type CompatibilityStatus = 'portable' | 'host-bridged' | 'partial' | 'unsupported' | 'not-plugin'

export interface PackageCompatibility {
  name: string
  directory: string
  status: CompatibilityStatus
  seams: string[]
  portableSeams: string[]
  hostBridgedSeams: string[]
  unsupportedSeams: string[]
  evidenceFiles: string[]
}

export interface CompatibilityMatrix {
  schemaVersion: 1
  source: { root: string; revision?: string }
  counts: Record<CompatibilityStatus, number> & { packages: number; candidates: number }
  packages: PackageCompatibility[]
}

const PORTABLE = new Set([
  'tools', 'llm', 'userQuestions', 'web', 'systemPrompt',
  'reflect', 'get', 'set', 'provide', 'effect', 'logger', 'inject',
  'on', 'once', 'emit', 'parallel', 'serial', 'bail', 'waterfall',
])
const HOST_BRIDGED = new Set(['fs', 'shell', 'sessions', 'agents'])
const HOST_BRIDGED_MEMBERS = new Set([
  'fs.sandboxMode', 'fs.resolve', 'fs.processPath', 'fs.fileUrl', 'fs.contains',
  'fs.stat', 'fs.lstat', 'fs.readText', 'fs.streamText', 'fs.readBytes',
  'fs.listDir', 'fs.writeText', 'fs.editText',
  'shell.sandboxMode', 'shell.resolve', 'shell.run',
  'sessions.get', 'sessions.list',
  'agents.get', 'agents.list', 'agents.roots',
])
const SKIP_DIRS = new Set(['node_modules', 'dist', 'coverage', '.git', '.turbo', '.cache', 'tests', 'test', '__tests__'])
const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.mts', '.cts', '.js', '.mjs', '.cjs'])

async function directoriesWithPackageJson(root: string): Promise<string[]> {
  const found: string[] = []
  const pending = [root]
  while (pending.length > 0) {
    const directory = pending.pop()
    if (directory === undefined) continue
    let entries
    try { entries = await readdir(directory, { withFileTypes: true }) } catch { continue }
    if (entries.some(entry => entry.isFile() && entry.name === 'package.json')) found.push(directory)
    for (const entry of entries) {
      if (!entry.isDirectory() || SKIP_DIRS.has(entry.name) || entry.name.startsWith('.')) continue
      pending.push(path.join(directory, entry.name))
    }
  }
  return found.sort()
}

async function sourceFiles(directory: string): Promise<string[]> {
  const roots = ['src', 'client', 'server'].map(name => path.join(directory, name))
  const files: string[] = []
  const pending: string[] = []
  for (const root of roots) {
    try { if ((await stat(root)).isDirectory()) pending.push(root) } catch { /* absent */ }
  }
  while (pending.length > 0) {
    const current = pending.pop()
    if (current === undefined) continue
    let entries
    try { entries = await readdir(current, { withFileTypes: true }) } catch { continue }
    for (const entry of entries) {
      const candidate = path.join(current, entry.name)
      if (entry.isDirectory() && !SKIP_DIRS.has(entry.name)) pending.push(candidate)
      else if (entry.isFile() && SOURCE_EXTENSIONS.has(path.extname(entry.name))) files.push(candidate)
    }
  }
  return files.sort()
}

function seamsInSource(source: string): Set<string> {
  const seams = new Set<string>()
  for (const match of source.matchAll(/(?:this\s*\.\s*)?ctx\s*\.\s*([A-Za-z_$][\w$]*)(?:\s*\.\s*([A-Za-z_$][\w$]*))?/g)) {
    const root = match[1]
    if (root === undefined) continue
    seams.add(root)
    const member = match[2]
    if (member !== undefined && HOST_BRIDGED.has(root)) seams.add(`${root}.${member}`)
  }
  for (const match of source.matchAll(/(?:export\s+const\s+inject|static\s+inject)\s*=\s*\[([^\]]*)\]/g)) {
    for (const item of match[1]?.matchAll(/['"]([^'"]+)['"]/g) ?? []) {
      const root = item[1]?.split('.')[0]
      if (root !== undefined && root !== '') seams.add(root)
    }
  }
  return seams
}

function isCandidate(source: string, seams: Set<string>): boolean {
  return seams.size > 0
    || /\bapply\s*\(\s*(?:ctx|context)\b/.test(source)
    || /\bextends\s+Service\b/.test(source)
    || /\bctx\.tools\.register\b/.test(source)
}

function statusOf(candidate: boolean, portable: string[], bridged: string[], unsupported: string[]): CompatibilityStatus {
  if (!candidate) return 'not-plugin'
  if (unsupported.length > 0 && portable.length + bridged.length > 0) return 'partial'
  if (unsupported.length > 0) return 'unsupported'
  if (bridged.length > 0) return 'host-bridged'
  return 'portable'
}

async function gitRevision(root: string): Promise<string | undefined> {
  try {
    const git = path.join(root, '.git')
    const gitStat = await stat(git)
    const gitDir = gitStat.isDirectory()
      ? git
      : path.resolve(root, (await readFile(git, 'utf8')).trim().replace(/^gitdir:\s*/, ''))
    const head = (await readFile(path.join(gitDir, 'HEAD'), 'utf8')).trim()
    if (!head.startsWith('ref: ')) return head
    return (await readFile(path.join(gitDir, head.slice(5)), 'utf8')).trim()
  } catch {
    return undefined
  }
}

export async function scanDshCompatibility(rootInput: string): Promise<CompatibilityMatrix> {
  const root = path.resolve(rootInput)
  const directories = await directoriesWithPackageJson(root)
  const packages: PackageCompatibility[] = []
  for (const directory of directories) {
    let packageJson: { name?: unknown }
    try { packageJson = JSON.parse(await readFile(path.join(directory, 'package.json'), 'utf8')) as { name?: unknown } } catch { continue }
    const files = await sourceFiles(directory)
    const seams = new Set<string>()
    const evidenceFiles: string[] = []
    let candidate = false
    for (const file of files) {
      const source = await readFile(file, 'utf8')
      const found = seamsInSource(source)
      if (isCandidate(source, found)) {
        candidate = true
        evidenceFiles.push(path.relative(root, file))
      }
      for (const seam of found) seams.add(seam)
    }
    const all = [...seams].sort()
    const portableSeams = all.filter(seam => PORTABLE.has(seam))
    const hostBridgedSeams = all.filter(seam => HOST_BRIDGED.has(seam) || HOST_BRIDGED_MEMBERS.has(seam))
    const unsupportedSeams = all.filter(seam =>
      !PORTABLE.has(seam) && !HOST_BRIDGED.has(seam) && !HOST_BRIDGED_MEMBERS.has(seam))
    packages.push({
      name: typeof packageJson.name === 'string' ? packageJson.name : path.relative(root, directory),
      directory: path.relative(root, directory) || '.',
      status: statusOf(candidate, portableSeams, hostBridgedSeams, unsupportedSeams),
      seams: all,
      portableSeams,
      hostBridgedSeams,
      unsupportedSeams,
      evidenceFiles,
    })
  }
  packages.sort((a, b) => a.name.localeCompare(b.name) || a.directory.localeCompare(b.directory))
  const count = (status: CompatibilityStatus) => packages.filter(entry => entry.status === status).length
  return {
    schemaVersion: 1,
    source: { root, revision: await gitRevision(root) },
    counts: {
      packages: packages.length,
      candidates: packages.filter(entry => entry.status !== 'not-plugin').length,
      portable: count('portable'),
      'host-bridged': count('host-bridged'),
      partial: count('partial'),
      unsupported: count('unsupported'),
      'not-plugin': count('not-plugin'),
    },
    packages,
  }
}

export async function writeCompatibilityMatrix(matrix: CompatibilityMatrix, output: string): Promise<void> {
  await writeFile(output, `${JSON.stringify(matrix, null, 2)}\n`, 'utf8')
}
