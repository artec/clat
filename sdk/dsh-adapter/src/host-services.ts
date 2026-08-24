import { createHash } from 'node:crypto'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { AdapterError } from './errors.js'
import type { HostChannel } from './shim.js'
import type {
  AgentMirrorLike,
  AgentRegistryLike,
  ClatHostContextLike,
  ClatHostLike,
  DshContext,
  FileSystemLike,
  FsInfoLike,
  FsTargetLike,
  SessionMirrorLike,
  SessionStoreLike,
  ShellLike,
} from './types.js'

const READ_MAX_BYTES = 65_536
const DEFAULT_TIMEOUT_MS = 120_000
const MAX_TIMEOUT_MS = 600_000

function unavailable(message: string): never {
  throw new AdapterError('CLAT_HOST_UNAVAILABLE', message)
}

function readOnly(service: string, operation: string): never {
  throw new AdapterError(
    'READ_ONLY_HOST_SERVICE',
    `ctx.${service}.${operation} is unavailable: the CLAT adapter exposes only a detached current-run mirror`,
  )
}

function aborted(signal?: AbortSignal): void {
  if (signal?.aborted === true) throw new AdapterError('HOST_CALL_ABORTED', 'host call aborted')
}

function versionOf(value: string): string {
  return createHash('sha256').update(value).digest('hex')
}

function record(value: unknown, subject: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new AdapterError('BAD_HOST_RESPONSE', `${subject} returned a non-object response`)
  }
  return value as Record<string, unknown>
}

function decodeNumberedText(value: unknown): { text: string; truncated: boolean } {
  const output = record(value, 'read_file')
  const numbered = typeof output.content === 'string' ? output.content : ''
  const lines = numbered.split('\n')
  if (lines.at(-1) === '') lines.pop()
  const text = lines.map(line => line.replace(/^\d+ \| ?/, '')).join('\n')
    + (numbered.endsWith('\n') && lines.length > 0 ? '\n' : '')
  return { text, truncated: output.truncated === true }
}

/** Shared projection of the CLAT host protocol into DSH-shaped services. */
export class HostServicesSeam {
  readonly clat: ClatHostLike
  readonly fs: FileSystemLike
  readonly shell: ShellLike
  readonly sessions: SessionStoreLike
  readonly agents: AgentRegistryLike
  #context: ClatHostContextLike | undefined
  #dshContext: DshContext | undefined
  #session: SessionMirrorLike | undefined
  #agent: AgentMirrorLike | undefined

  constructor(private readonly host: HostChannel) {
    this.clat = {
      context: () => this.refresh(),
      callTool: (name, arguments_) => this.callTool(name, arguments_),
    }
    this.fs = this.buildFileSystem()
    this.shell = this.buildShell()
    this.sessions = this.buildSessions()
    this.agents = this.buildAgents()
  }

  attachContext(context: DshContext): void {
    this.#dshContext = context
    this.rebuildMirrors()
  }

  updateContext(context: ClatHostContextLike | null): void {
    this.#context = context ?? undefined
    this.rebuildMirrors()
  }

  private async refresh(): Promise<ClatHostContextLike> {
    if (this.host.context === undefined || !this.host.capabilities.hostServices) {
      return unavailable('the connected host did not negotiate CLAT host-services 0.1.0')
    }
    try {
      const context = await this.host.context()
      this.updateContext(context)
      return context
    } catch (error) {
      // A failed authoritative refresh (most commonly run teardown) must not
      // leave the adapter presenting the previous run's detached mirror.
      this.updateContext(null)
      throw error
    }
  }

  private current(): ClatHostContextLike {
    return this.#context ?? unavailable('no CLAT run is active')
  }

  private async callTool(name: string, arguments_: Record<string, unknown>): Promise<unknown> {
    if (this.host.hostTool === undefined || !this.host.capabilities.hostServices) {
      return unavailable('the connected host did not negotiate CLAT host-services 0.1.0')
    }
    this.current()
    return this.host.hostTool(name, arguments_)
  }

  private target(path_: string, cwd?: string): FsTargetLike {
    const root = this.current().project.root
    const absolute = path.resolve(cwd ?? root, path_)
    return { targetKey: absolute, displayPath: absolute }
  }

  private async read(target: FsTargetLike, signal?: AbortSignal): Promise<string> {
    aborted(signal)
    const decoded = decodeNumberedText(await this.callTool('read_file', {
      path: this.fs.processPath(target),
      max_bytes: READ_MAX_BYTES,
    }))
    aborted(signal)
    if (decoded.truncated) {
      throw new AdapterError(
        'FS_TOO_LARGE',
        `ctx.fs.readText is bounded to ${READ_MAX_BYTES} bytes by the CLAT host bridge`,
      )
    }
    return decoded.text
  }

  private buildFileSystem(): FileSystemLike {
    const seam = this
    return {
      sandboxMode: undefined,
      async resolve(path_, opts) {
        aborted(opts?.signal)
        return seam.target(path_, opts?.cwd)
      },
      processPath(target) {
        return path.resolve(target.targetKey)
      },
      fileUrl(target) {
        return pathToFileURL(path.resolve(target.targetKey)).href
      },
      contains(parent, child) {
        const relative = path.relative(path.resolve(parent.targetKey), path.resolve(child.targetKey))
        return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative))
      },
      async stat(target, signal) {
        aborted(signal)
        const targetPath = path.resolve(target.targetKey)
        try {
          const listing = record(await seam.callTool('list_files', {
            path: targetPath,
            max_depth: 0,
            max_entries: 1,
          }), 'list_files')
          aborted(signal)
          const serialized = JSON.stringify(listing)
          return { type: 'directory', version: versionOf(serialized) }
        } catch {
          try {
            const text = await seam.read(target, signal)
            return { type: 'file', version: versionOf(text), size: new TextEncoder().encode(text).byteLength }
          } catch (error) {
            if (error instanceof AdapterError && error.code === 'FS_TOO_LARGE') {
              return { type: 'file', version: 'clat:bounded-file' }
            }
            return undefined
          }
        }
      },
      async lstat(path_, opts, signal) {
        aborted(signal)
        const target = seam.target(path_, opts?.cwd)
        const parent = path.dirname(target.targetKey)
        try {
          const listing = record(await seam.callTool('list_files', {
            path: parent,
            max_depth: 0,
            max_entries: 2_000,
          }), 'list_files')
          const entries = Array.isArray(listing.entries) ? listing.entries : []
          const found = entries.map(entry => record(entry, 'list_files entry')).find(entry =>
            path.basename(String(entry.path ?? '')) === path.basename(target.targetKey))
          if (found === undefined) return undefined
          const kind = found.kind === 'file' || found.kind === 'directory' || found.kind === 'symlink'
            ? found.kind
            : 'other'
          return { type: kind, version: versionOf(JSON.stringify(found)) }
        } catch {
          return undefined
        }
      },
      readText: (target, signal) => seam.read(target, signal),
      async streamText(target, signal) {
        const text = await seam.read(target, signal)
        return (async function* () { yield text })()
      },
      async readBytes(target, signal, maxBytes) {
        const text = await seam.read(target, signal)
        const bytes = new TextEncoder().encode(text)
        if (bytes.byteLength > maxBytes) throw new AdapterError('FS_TOO_LARGE', `file exceeds ${maxBytes} bytes`)
        return bytes
      },
      async listDir(target, signal) {
        aborted(signal)
        const listing = record(await seam.callTool('list_files', {
          path: this.processPath(target),
          max_depth: 0,
          max_entries: 2_000,
        }), 'list_files')
        if (listing.truncated === true) throw new AdapterError('FS_TOO_LARGE', 'directory listing was truncated')
        const entries = Array.isArray(listing.entries) ? listing.entries : []
        return entries.map(raw => {
          const entry = record(raw, 'list_files entry')
          const child = seam.target(String(entry.path ?? ''), seam.current().project.root)
          const type: 'file' | 'directory' | 'other' =
            entry.kind === 'file' || entry.kind === 'directory' ? entry.kind : 'other'
          return { name: path.basename(child.displayPath), type, target: child, version: versionOf(JSON.stringify(entry)) }
        }).sort((a, b) => a.name.localeCompare(b.name))
      },
      async writeText(target, content, expected, signal) {
        aborted(signal)
        let before: string | null = null
        try { before = await seam.read(target, signal) } catch { /* absent or unreadable */ }
        if (expected !== undefined) {
          throw new AdapterError(
            'FS_GUARD_UNSUPPORTED',
            'CLAT host tools cannot provide DSH atomic version guards; omit expected or port the plugin to a native CLAT capability',
          )
        }
        await seam.callTool('write_file', { path: this.processPath(target), content })
        aborted(signal)
        return { operation: before === null ? 'create' : 'update', version: versionOf(content), before, after: content }
      },
      async editText(target, edit, expected, signal) {
        aborted(signal)
        if (expected !== undefined || edit.replaceAll) {
          throw new AdapterError(
            'FS_GUARD_UNSUPPORTED',
            'CLAT host edit supports one unique literal replacement, not DSH version guards or replaceAll',
          )
        }
        const before = await seam.read(target, signal)
        await seam.callTool('edit_file', {
          path: this.processPath(target),
          old_str: edit.oldString,
          new_str: edit.newString,
        })
        const after = before.replace(edit.oldString, edit.newString)
        return { version: versionOf(after), before, after }
      },
    }
  }

  private buildShell(): ShellLike {
    const seam = this
    return {
      sandboxMode: undefined,
      resolve(request) {
        const context = seam.current()
        if (request.env !== undefined || request.dshEnv !== undefined || request.stdin !== undefined) {
          throw new AdapterError('SHELL_OPTION_UNSUPPORTED', 'CLAT host shell does not accept env, dshEnv, or stdin')
        }
        const workdir = typeof request.workdir === 'string' ? path.resolve(request.workdir) : context.project.root
        if (workdir !== path.resolve(context.project.root)) {
          throw new AdapterError('SHELL_OPTION_UNSUPPORTED', 'CLAT host shell always executes in the project root')
        }
        const requested = typeof request.timeoutMs === 'number' ? request.timeoutMs : DEFAULT_TIMEOUT_MS
        const timeoutMs = Math.max(1_000, Math.min(MAX_TIMEOUT_MS, requested))
        return {
          ...request,
          workdir,
          timeoutMs,
          stdoutMaxBytes: typeof request.stdoutMaxBytes === 'number' ? request.stdoutMaxBytes : 32 * 1_024,
          sandboxPolicy: undefined,
        }
      },
      async run(spec) {
        aborted(spec.signal as AbortSignal | undefined)
        const output = record(await seam.callTool('run_command', {
          command: spec.command,
          timeout_seconds: Math.max(1, Math.ceil(spec.timeoutMs / 1_000)),
        }), 'run_command')
        return {
          exitCode: typeof output.exit_code === 'number' ? output.exit_code : null,
          signal: output.signal === null || output.signal === undefined ? null : String(output.signal),
          timedOut: output.timed_out === true,
          aborted: false,
          timeoutMs: spec.timeoutMs,
          stdout: { text: String(output.stdout ?? ''), truncated: output.stdout_truncated === true },
          stderr: { text: String(output.stderr ?? ''), truncated: output.stderr_truncated === true },
        }
      },
      start() {
        return unavailable('ctx.shell.start is unavailable: CLAT host services do not expose background process handles')
      },
    }
  }

  private buildSessions(): SessionStoreLike {
    return {
      get: id => this.#session?.id === id ? this.#session : undefined,
      list: () => this.#session === undefined ? [] : [this.#session],
      create: () => readOnly('sessions', 'create'),
      prepare: () => readOnly('sessions', 'prepare'),
      enter: () => readOnly('sessions', 'enter'),
      announce: () => readOnly('sessions', 'announce'),
      flush: () => readOnly('sessions', 'flush'),
      fork: () => readOnly('sessions', 'fork'),
    }
  }

  private buildAgents(): AgentRegistryLike {
    return {
      get: id => this.#agent?.id === id ? this.#agent : undefined,
      list: () => this.#agent === undefined ? [] : [this.#agent],
      roots: () => this.#agent === undefined ? [] : [this.#agent],
      create: () => readOnly('agents', 'create'),
      resume: () => readOnly('agents', 'resume'),
      setFactory: () => readOnly('agents', 'setFactory'),
    }
  }

  private rebuildMirrors(): void {
    const sessionId = this.#context?.run.sessionId
    if (sessionId === undefined) {
      this.#session = undefined
      this.#agent = undefined
      return
    }
    const messages = Object.freeze(structuredClone(this.#context?.run.messages ?? []))
    const session: SessionMirrorLike = Object.freeze({
      id: sessionId,
      header: Object.freeze({
        version: 1,
        id: sessionId,
        createdAt: 0,
        cwd: this.#context?.project.root,
        origin: 'clat-host-mirror',
      }),
      events: Object.freeze(messages.map((message, seq) => Object.freeze({
        type: 'clat/model-item', seq, time: 0, data: message,
      }))),
      messages,
    })
    this.#session = session
    this.#agent = this.#dshContext === undefined ? undefined : Object.freeze({
      id: sessionId,
      session,
      ctx: this.#dshContext,
    })
  }
}
