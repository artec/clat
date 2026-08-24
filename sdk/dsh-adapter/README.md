# @artec/clat-dsh-adapter

English | [中文](README.zh.md)

Serve the portable capabilities of an existing
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) plugin as
an MCP stdio server for
[CLAT](https://github.com/artec/clat) or any MCP host—without modifying the
plugin itself.

The adapter runs in the plugin author's distribution. CLAT does not embed a
JavaScript runtime; to the end user, the result is an ordinary MCP server.

## Is this adapter a fit?

Good fits contribute tools, system-prompt material, model sampling, user
questions, web providers, local services, or Cordis events/effects. Plugins
that depend directly on host sessions, agents, subagents, filesystem/shell
seams, permissions, or UI services still need corresponding native CLAT host
services or must be split at that boundary.

For the complete compatibility matrix and migration guidance, read the
[porting guide](https://github.com/artec/clat/blob/main/docs/dsh-plugins.md).

## Quick start

Given an existing plugin:

```ts
// src/index.ts — unchanged DSH plugin
import { defineTool } from '@deepseek-ai/dsh-tools'

export const name = 'my-plugin'
export const inject = [] as const

export function Config(config: { apiKey: string }) {
  if (!config.apiKey) throw new Error('MY_API_KEY is required')
  return config
}

export function apply(ctx, config: { apiKey: string }) {
  ctx.tools.register(defineTool({ /* ... */ }))
}
```

add one bin:

```ts
// bin/clat.mjs
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '../src/index.js'

serveClat({ apply, Config, inject, name }, {
  name: 'my-plugin',
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' },
  toolHints: { my_tool: 'network' },
}).catch(error => {
  console.error(error)
  process.exit(1)
})
```

Add a `bin` entry to `package.json`. DSH users keep the original Cordis entry;
MCP users run this bin.

A CLAT user configures `~/.clat/mcp.json`:

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "..." }
  }
}
```

stdout is protocol-only. Use `ctx.logger` or `console.error` for diagnostics.

## API

`serveClat(plugin, options)` accepts the plugin export object, a bare `apply`
function, or a static `class Foo extends Service` plugin. MCP initialization
waits for startup to settle. If it throws, the adapter shuts down before
rejecting startup.

| Option | Type | Purpose |
|---|---|---|
| `name` | `string` | MCP server name; defaults to `plugin.name`, then `dsh-plugin` |
| `version` | `string` | MCP server version |
| `config` | `unknown` | passed to `apply`; validated by `Config` when exported |
| `toolHints` | `Record<string, ToolHint>` | effect declarations for registered tools |
| `input`, `output` | streams | test seams; default to process stdio |

## Supported surface

| DSH API | MCP behavior |
|---|---|
| `ctx.tools.register(defineTool(...))` | `tools/list` + `tools/call`; compiled schema and `output.render` are preserved |
| `ctx.llm.stream(...)` | `sampling/createMessage` using the host model, permission gate, spend budget, and usage ledger |
| `ctx.userQuestions.ask(...)` | `elicitation/create`; fields are asked sequentially |
| `ctx.web.registerSearchProvider(...)` | built-in `web_search` with multi-query merge, URL deduplication, and bounded results |
| `ctx.web.registerFetchProvider(...)` | built-in, bounded `web_fetch` |
| `ctx.systemPrompt` | sections, contexts, ordering, complete sections, variables, tool providers, change events, and assembly waterfall |
| `ctx.get/set/provide`, `ctx.reflect.provide` | process-local service registration and disposal |
| `ctx.on/once`, `emit/parallel/serial/bail/waterfall` | process-local Cordis dispatch semantics |
| `launchEnvironmentOf(ctx)` | falls back to `process.env` for plugin environment lookup |
| `ctx.effect`, callable `ctx.logger(name)` | direct/promise/generator cleanup in reverse order; stderr logging |
| `class Foo extends Service` | constructor, `initHooks`, `Service.init`, and yielded cleanup |
| exported `Config` | startup validation before `apply` |

Static `inject` declarations succeed for adapter/local services and fail with
a precise diagnostic for missing host-spine services. Runtime
`ctx.inject(deps, callback)` runs immediately when all services exist and
returns an awaitable/disposable static Fiber-shaped handle; otherwise it
follows the DSH "not mounted" contract and skips the callback.
This is a static single-scope Cordis subset: it does not emulate hot reload,
scope chains, isolate/intercept filtering, or dependency-driven restart.
Function/object `apply()` results and Service lifecycle results may be direct,
promised, or sync/async-generator cleanups; the adapter owns all of them.

System-prompt contributions are exposed through a marked MCP prompt. CLAT
imports only that marked prompt, passes the real project directory as `cwd`,
and freezes imported prompts with tools before the first run. Runtime-context
text remains MCP metadata because CLAT does not yet have DSH's user-role
context-snapshot registry.

## Tool hints

DSH tools do not carry a static effect. Omission uses the conservative
`destructive` behavior.

| Hint | Meaning |
|---|---|
| `'read-only'` | reads closed-world data; no side effects |
| `'network'` | read-oriented open-world/network access |
| `'write'` | mutates files or external state |
| `'destructive'` or omitted | destructive or unknown behavior |

Hints become MCP annotations. The host still owns the final effect mapping and
permission policy.

## Narrowings

- `apply()` must settle within the host handshake timeout (10 seconds in CLAT).
- `ctx.llm.stream({ tools })` is rejected; MCP sampling does not carry tool
  calling.
- Sampling messages are text-only. Image blocks return `NON_TEXT_CONTENT`.
- `stopSequences` are sent, but the current CLAT host bridge ignores them.
- `multiSelect` questions become comma-separated text.
- One ask contains at most 16 questions and 16 options per question.
- `exec.deferContext()` and `exec.concludeTurn()` are warning + no-op seams.
- `web_fetch` caps rendered content at 100,000 characters and does not reproduce
  DSH's complete HTML-to-Markdown pipeline.
- Agent/session/fs/shell/permission/settings/commands/UI services and scoped
  prompt shadowing remain native-host responsibilities.

Host `notifications/cancelled` and adapter shutdown abort the active
`tools/call` signal and pending sampling/elicitation promises. Plugin work must
observe `exec.signal` to stop cooperatively.

## Security boundary

The adapter is an MCP stdio process running arbitrary plugin code, not a WASM
capability sandbox. Under CLAT it inherits the host process environment and the
operating-system account's filesystem, process, and network authority. CLAT
sets an MCP subprocess's cwd to `~/.clat`; the real project root is passed
only as the controlled prompt argument `cwd`. Neither measure is isolation.

`toolHints` affect pre-call approval classification only. They cannot restrict
what the process can do outside a tool handler. Review the plugin as an
executable dependency, use narrowly scoped credentials, and launch it through
a clean-environment wrapper when appropriate. See CLAT's full
[MCP security posture](https://github.com/artec/clat/blob/main/docs/mcp.md#security-posture).

## Distribute without a user-side Node runtime

Compile the bin with Bun:

```bash
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

Point the MCP `command` at that executable. The runtime is included in the
artifact, so end users install no JavaScript environment.

## Compatibility and verification

- Author runtime: Node.js 22.19 or newer.
- Adapter runtime dependencies: zero.
- API target: `dsh-v0.1.1-rc.2`, source revision
  `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`.
- Acceptance fixture: the npm-published
  `@deepseek-ai/dsh-web-search-exa` mounts unmodified under
  [`examples/exa`](https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/exa).

```bash
npm install
npm test
```

Repository: [artec/clat](https://github.com/artec/clat) ·
[sdk/dsh-adapter](https://github.com/artec/clat/tree/main/sdk/dsh-adapter)

MIT
