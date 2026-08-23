# @artec/clat-dsh-adapter

English | [中文](README.zh.md)

Serve an existing
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) leaf plugin
as an MCP stdio server for
[CLAT](https://github.com/artec/clat) or any MCP host—without modifying the
plugin itself.

The adapter runs in the plugin author's distribution. CLAT does not embed a
JavaScript runtime; to the end user, the result is an ordinary MCP server.

## Is this adapter a fit?

Good fits are pure algorithms and wrappers around search, SaaS, database, or
other external APIs. Plugins that depend directly on host sessions, agents,
subagents, filesystem/shell seams, or UI services are host-spine plugins and
must be redesigned as leaf tools first.

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

`serveClat(plugin, options)` accepts the plugin export object or a bare
`apply` function. MCP initialization waits for `apply()` to settle. If it
throws, the adapter shuts down before rejecting startup.

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
| `ctx.web.registerFetchProvider(...)` | registration accepted; no `web_fetch` tool in v0 |
| `ctx.get(key)` | always `undefined` |
| `launchEnvironmentOf(ctx)` | falls back to `process.env` for plugin environment lookup |
| `ctx.effect`, `ctx.logger` | in-process LIFO cleanup and stderr logging |
| exported `Config` | startup validation before `apply` |

Static spine-service `inject` declarations, runtime direct spine access, and
class plugins (`extends Service`) fail startup with a migration hint. Optional
runtime `ctx.inject(deps, callback)` follows the DSH "not mounted" contract:
the callback is skipped with a stderr note and the plugin keeps running.

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

Host `notifications/cancelled` and adapter shutdown abort the active
`tools/call` signal and pending sampling/elicitation promises. Plugin work must
observe `exec.signal` to stop cooperatively.

## Security boundary

The adapter is an MCP stdio process running arbitrary plugin code, not a WASM
capability sandbox. Under CLAT it inherits the host process environment and the
operating-system account's filesystem, process, and network authority. CLAT
sets an MCP subprocess's cwd to `~/.clat`, but that is not isolation.

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
- API target: `dsh-v0.1.0-rc.7`, verified equivalent on rc.8 and rechecked
  against the plugin surface of `0.1.1-rc.1`.
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
