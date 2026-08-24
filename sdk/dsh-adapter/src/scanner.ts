import { readFile, readdir, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import type * as T from 'typescript'

let ts: typeof T

async function ensureTypeScript(): Promise<void> {
  try {
    const loaded = await import('typescript')
    ts = (loaded as unknown as { default?: typeof T }).default ?? loaded
  } catch (error) {
    throw new Error(`semantic DSH scanning requires TypeScript >=5.7 in the author workspace: ${String(error)}`)
  }
}

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
  analysis: {
    contextBindings: number
    serviceClasses: number
    staticInjects: number
  }
}

export interface CompatibilityMatrix {
  schemaVersion: 2
  source: { root: string; revision?: string }
  counts: Record<CompatibilityStatus, number> & { packages: number; candidates: number }
  packages: PackageCompatibility[]
}

const PORTABLE = new Set([
  'tools', 'llm', 'userQuestions', 'web', 'systemPrompt',
  'reflect', 'get', 'set', 'provide', 'effect', 'logger', 'inject',
  'on', 'once', 'emit', 'parallel', 'serial', 'bail', 'waterfall',
])
const HOST_BRIDGED = new Set(['clat', 'fs', 'shell', 'sessions', 'agents'])
const HOST_BRIDGED_MEMBERS = new Set([
  'clat.context', 'clat.callTool',
  'fs.sandboxMode', 'fs.resolve', 'fs.processPath', 'fs.fileUrl', 'fs.contains',
  'fs.stat', 'fs.lstat', 'fs.readText', 'fs.streamText', 'fs.readBytes',
  'fs.listDir', 'fs.writeText', 'fs.editText',
  'shell.sandboxMode', 'shell.resolve', 'shell.run',
  'sessions.get', 'sessions.list',
  'agents.get', 'agents.list', 'agents.roots',
])
const SKIP_DIRS = new Set(['node_modules', 'dist', 'coverage', '.git', '.turbo', '.cache', 'tests', 'test', '__tests__'])
const SOURCE_EXTENSIONS = new Set(['.ts', '.tsx', '.mts', '.cts', '.js', '.mjs', '.cjs'])
const CONTEXT_MODULE = /(?:^cordis$|@deepseek-ai\/cordis|@deepseek-ai\/dsh|deepseek-harness|clat-dsh-adapter)/

interface SourceAnalysis {
  seams: Set<string>
  candidate: boolean
  contextBindings: number
  serviceClasses: number
  staticInjects: number
}

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
  const primaryRoots = ['src', 'client', 'server'].map(name => path.join(directory, name))
  const files: string[] = []
  const pending: string[] = []
  for (const root of primaryRoots) {
    try { if ((await stat(root)).isDirectory()) pending.push(root) } catch { /* absent */ }
  }
  // Published npm packages often contain only dist/. Prefer source when it is
  // present, but fall back to generated JS so `clat-dsh inspect/port` works on
  // the artifact plugin authors and users actually have.
  if (pending.length === 0) {
    for (const name of ['dist', 'lib']) {
      const root = path.join(directory, name)
      try { if ((await stat(root)).isDirectory()) pending.push(root) } catch { /* absent */ }
    }
  }
  while (pending.length > 0) {
    const current = pending.pop()
    if (current === undefined) continue
    let entries
    try { entries = await readdir(current, { withFileTypes: true }) } catch { continue }
    for (const entry of entries) {
      const candidate = path.join(current, entry.name)
      if (entry.isDirectory() && !SKIP_DIRS.has(entry.name)) pending.push(candidate)
      else if (entry.isFile()
        && !entry.name.endsWith('.d.ts')
        && SOURCE_EXTENSIONS.has(path.extname(entry.name))) files.push(candidate)
    }
  }
  return files.sort()
}

function scriptKind(file: string): T.ScriptKind {
  switch (path.extname(file)) {
    case '.tsx': return ts.ScriptKind.TSX
    case '.js': case '.mjs': case '.cjs': return ts.ScriptKind.JS
    default: return ts.ScriptKind.TS
  }
}

function nameText(name: T.PropertyName | T.BindingName | undefined): string | undefined {
  if (name === undefined) return undefined
  if (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNumericLiteral(name)) return name.text
  return undefined
}

function hasModifier(node: T.Node, kind: T.SyntaxKind): boolean {
  return ts.canHaveModifiers(node) && ts.getModifiers(node)?.some(modifier => modifier.kind === kind) === true
}

function trustedImports(sourceFile: T.SourceFile): { contexts: Set<string>; services: Set<string> } {
  const contexts = new Set<string>()
  const services = new Set<string>()
  for (const statement of sourceFile.statements) {
    if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)) continue
    if (!CONTEXT_MODULE.test(statement.moduleSpecifier.text)) continue
    const bindings = statement.importClause?.namedBindings
    if (bindings === undefined || !ts.isNamedImports(bindings)) continue
    for (const specifier of bindings.elements) {
      const imported = specifier.propertyName?.text ?? specifier.name.text
      if (imported === 'Context' || imported === 'DshContext') contexts.add(specifier.name.text)
      if (imported === 'Service') services.add(specifier.name.text)
    }
  }
  return { contexts, services }
}

function typeRootName(type: T.TypeNode | undefined): string | undefined {
  if (type === undefined) return undefined
  if (ts.isTypeReferenceNode(type)) {
    if (ts.isIdentifier(type.typeName)) return type.typeName.text
    return type.typeName.right.text
  }
  return undefined
}

function isApplyFunction(node: T.SignatureDeclaration): boolean {
  if ((ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node)) && nameText(node.name) === 'apply') return true
  const parent = node.parent
  if (ts.isVariableDeclaration(parent) && nameText(parent.name) === 'apply') return true
  if ((ts.isPropertyAssignment(parent) || ts.isPropertyDeclaration(parent)) && nameText(parent.name) === 'apply') return true
  return false
}

function classExtendsTrustedService(node: T.ClassLikeDeclaration, services: Set<string>): boolean {
  return node.heritageClauses?.some(clause =>
    clause.token === ts.SyntaxKind.ExtendsKeyword && clause.types.some(type => {
      const expression = type.expression
      return ts.isIdentifier(expression) && services.has(expression.text)
    }),
  ) === true
}

function injectRoots(initializer: T.Expression | undefined): string[] {
  if (initializer === undefined || !ts.isArrayLiteralExpression(initializer)) return []
  const roots: string[] = []
  for (const element of initializer.elements) {
    if (!ts.isStringLiteralLike(element)) continue
    const root = element.text.split('.')[0]
    if (root !== undefined && root !== '') roots.push(root)
  }
  return roots
}

function isStaticInject(node: T.Node): node is T.VariableDeclaration | T.PropertyDeclaration | T.PropertyAssignment {
  if (ts.isVariableDeclaration(node) && nameText(node.name) === 'inject') {
    const statement = node.parent.parent
    return ts.isVariableStatement(statement) && hasModifier(statement, ts.SyntaxKind.ExportKeyword)
  }
  if (ts.isPropertyDeclaration(node) && nameText(node.name) === 'inject') {
    return hasModifier(node, ts.SyntaxKind.StaticKeyword)
  }
  return ts.isPropertyAssignment(node) && nameText(node.name) === 'inject'
}

function contextChain(expression: T.Expression, active: Set<string>, inService: boolean): string[] | undefined {
  const parts: string[] = []
  let current: T.Expression = expression
  while (ts.isPropertyAccessExpression(current)) {
    parts.unshift(current.name.text)
    current = current.expression
  }
  if (ts.isIdentifier(current) && active.has(current.text)) return parts
  if (current.kind === ts.SyntaxKind.ThisKeyword && inService && parts[0] === 'ctx') {
    return parts.slice(1)
  }
  return undefined
}

function addContextSeam(seams: Set<string>, chain: string[]): void {
  const root = chain[0]
  if (root === undefined) return
  seams.add(root)
  const member = chain[1]
  if (member !== undefined && HOST_BRIDGED.has(root)) seams.add(`${root}.${member}`)
}

function analyzeSource(file: string, source: string): SourceAnalysis {
  const sourceFile = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, scriptKind(file))
  const imports = trustedImports(sourceFile)
  const seams = new Set<string>()
  let contextBindings = 0
  let serviceClasses = 0
  let staticInjects = 0

  const visit = (node: T.Node, inherited: Set<string>, inService: boolean): void => {
    let active = inherited
    let service = inService
    if (ts.isClassLike(node) && classExtendsTrustedService(node, imports.services)) {
      service = true
      serviceClasses += 1
    }
    if (ts.isFunctionLike(node)) {
      active = new Set(inherited)
      const apply = isApplyFunction(node)
      for (const [index, parameter] of node.parameters.entries()) {
        const parameterName = nameText(parameter.name)
        if (parameterName === undefined) continue
        active.delete(parameterName)
        const typed = imports.contexts.has(typeRootName(parameter.type) ?? '')
        if ((apply && index === 0) || typed) {
          active.add(parameterName)
          contextBindings += 1
        }
      }
    }
    if (isStaticInject(node)) {
      const roots = injectRoots(node.initializer)
      if (roots.length > 0) staticInjects += 1
      for (const root of roots) seams.add(root)
    }
    if (ts.isPropertyAccessExpression(node)) {
      const chain = contextChain(node, active, service)
      if (chain !== undefined) addContextSeam(seams, chain)
    }
    ts.forEachChild(node, child => visit(child, active, service))
  }
  visit(sourceFile, new Set(), false)
  return {
    seams,
    candidate: seams.size > 0 || contextBindings > 0 || serviceClasses > 0 || staticInjects > 0,
    contextBindings,
    serviceClasses,
    staticInjects,
  }
}

function statusOf(candidate: boolean, portable: string[], bridged: string[], unsupported: string[]): CompatibilityStatus {
  if (!candidate) return 'not-plugin'
  if (unsupported.length > 0 && portable.length + bridged.length > 0) return 'partial'
  if (unsupported.length > 0) return 'unsupported'
  if (bridged.length > 0) return 'host-bridged'
  return 'portable'
}

async function resolveLocalModule(packageRoot: string, importer: string, specifier: string): Promise<string | undefined> {
  const base = path.resolve(path.dirname(importer), specifier)
  const candidates = [
    base,
    ...['.ts', '.tsx', '.mts', '.cts', '.js', '.mjs', '.cjs'].map(extension => `${base}${extension}`),
    ...['.ts', '.tsx', '.js', '.mjs', '.cjs'].map(extension => path.join(base, `index${extension}`)),
  ]
  for (const candidate of candidates) {
    try {
      const canonical = await (await import('node:fs/promises')).realpath(candidate)
      if (isWithin(packageRoot, canonical) && (await stat(canonical)).isFile()) return canonical
    } catch { /* try next */ }
  }
  return undefined
}

function isWithin(parent: string, child: string): boolean {
  const relative = path.relative(parent, child)
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative))
}

function relativeModuleSpecifiers(file: string, source: string): string[] {
  const sourceFile = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, scriptKind(file))
  const specifiers: string[] = []
  for (const statement of sourceFile.statements) {
    if ((ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement))
      && statement.moduleSpecifier !== undefined
      && ts.isStringLiteral(statement.moduleSpecifier)
      && statement.moduleSpecifier.text.startsWith('.')) {
      specifiers.push(statement.moduleSpecifier.text)
    }
  }
  return specifiers
}

/**
 * Inspect only the package's main plugin entry and its relative import graph.
 * Companion exports such as `./invariant` remain separate packages/seams and
 * do not incorrectly downgrade an otherwise portable main plugin.
 */
export async function inspectDshPackageEntry(
  packageRootInput: string,
  entryInput: string,
  packageName: string,
): Promise<PackageCompatibility> {
  await ensureTypeScript()
  const packageRoot = await (await import('node:fs/promises')).realpath(packageRootInput)
  const entry = await (await import('node:fs/promises')).realpath(entryInput)
  if (!isWithin(packageRoot, entry)) throw new Error('package entry escapes the package root')
  const pending = [entry]
  const visited = new Set<string>()
  const seams = new Set<string>()
  const evidenceFiles: string[] = []
  let candidate = false
  let contextBindings = 0
  let serviceClasses = 0
  let staticInjects = 0
  while (pending.length > 0) {
    const file = pending.pop()
    if (file === undefined || visited.has(file)) continue
    visited.add(file)
    const source = await readFile(file, 'utf8')
    const analysis = analyzeSource(file, source)
    if (analysis.candidate) {
      candidate = true
      evidenceFiles.push(path.relative(packageRoot, file))
    }
    for (const seam of analysis.seams) seams.add(seam)
    contextBindings += analysis.contextBindings
    serviceClasses += analysis.serviceClasses
    staticInjects += analysis.staticInjects
    for (const specifier of relativeModuleSpecifiers(file, source)) {
      const resolved = await resolveLocalModule(packageRoot, file, specifier)
      if (resolved !== undefined && !visited.has(resolved)) pending.push(resolved)
    }
  }
  evidenceFiles.sort()
  const all = [...seams].sort()
  const portableSeams = all.filter(seam => PORTABLE.has(seam))
  const hostBridgedSeams = all.filter(seam => HOST_BRIDGED.has(seam) || HOST_BRIDGED_MEMBERS.has(seam))
  const unsupportedSeams = all.filter(seam =>
    !PORTABLE.has(seam) && !HOST_BRIDGED.has(seam) && !HOST_BRIDGED_MEMBERS.has(seam))
  return {
    name: packageName,
    directory: '.',
    status: statusOf(candidate, portableSeams, hostBridgedSeams, unsupportedSeams),
    seams: all,
    portableSeams,
    hostBridgedSeams,
    unsupportedSeams,
    evidenceFiles,
    analysis: { contextBindings, serviceClasses, staticInjects },
  }
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
    const ref = head.slice(5)
    const roots = [gitDir]
    try {
      const common = (await readFile(path.join(gitDir, 'commondir'), 'utf8')).trim()
      roots.push(path.resolve(gitDir, common))
    } catch { /* ordinary checkout */ }
    for (const candidate of roots) {
      try { return (await readFile(path.join(candidate, ref), 'utf8')).trim() } catch { /* packed? */ }
    }
    for (const candidate of roots) {
      try {
        const packed = await readFile(path.join(candidate, 'packed-refs'), 'utf8')
        for (const line of packed.split('\n')) {
          if (line.startsWith('#') || line.startsWith('^')) continue
          const [revision, name] = line.trim().split(' ')
          if (name === ref && revision !== undefined) return revision
        }
      } catch { /* no packed refs */ }
    }
    return undefined
  } catch {
    return undefined
  }
}

export async function scanDshCompatibility(rootInput: string): Promise<CompatibilityMatrix> {
  await ensureTypeScript()
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
    let contextBindings = 0
    let serviceClasses = 0
    let staticInjects = 0
    for (const file of files) {
      const analysis = analyzeSource(file, await readFile(file, 'utf8'))
      if (analysis.candidate) {
        candidate = true
        evidenceFiles.push(path.relative(root, file))
      }
      for (const seam of analysis.seams) seams.add(seam)
      contextBindings += analysis.contextBindings
      serviceClasses += analysis.serviceClasses
      staticInjects += analysis.staticInjects
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
      analysis: { contextBindings, serviceClasses, staticInjects },
    })
  }
  packages.sort((a, b) => a.name.localeCompare(b.name) || a.directory.localeCompare(b.directory))
  const count = (status: CompatibilityStatus) => packages.filter(entry => entry.status === status).length
  return {
    schemaVersion: 2,
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
