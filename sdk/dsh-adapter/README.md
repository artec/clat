# @artec/clat-dsh-adapter

English | [中文](README.zh.md)

Serve an existing [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (DSH) leaf plugin as an **MCP stdio server** — so [CLAT](https://github.com/artec/clat) users, and users of any MCP host, can run your plugin **without modifying it**.

The compatibility direction is inverted on purpose: CLAT never embeds a JS runtime. Your distribution mounts the adapter around your own plugin; to MCP clients the result is just another MCP server.

## Quick start

The whole port is one bin file. Given your existing plugin exports:

```ts
// src/index.ts — your existing DSH plugin, unchanged
import { defineTool } from '@deepseek-ai/dsh-tools'

export const name = 'my-plugin'
export const inject = [] as const

export function Config(config: { apiKey: string }) {
  if (!config.apiKey) throw new Error('MY_API_KEY is required')
  return config
}

export function apply(ctx, config: { apiKey: string }) {
  ctx.tools.register(defineTool({ /* … */ }))
}
```

add this bin:

```ts
// bin/clat.mjs — the only new file
import { serveClat } from '@artec/clat-dsh-adapter'
import { apply, Config, inject, name } from '../src/index.js'

serveClat({ apply, Config, inject, name }, {
  name: 'my-plugin',                          // MCP serverInfo name
  version: '1.0.0',
  config: { apiKey: process.env.MY_API_KEY ?? '' },
  toolHints: { my_tool: 'network' },          // optional, see below
}).catch(error => {
  console.error(error)
  process.exit(1)
})
```

plus a `"bin"` entry in package.json, and one package serves both runtimes: DSH users keep the Cordis entry, CLAT users point their MCP config at the bin.

A CLAT user configures it in `~/.clat/mcp.json` (any MCP stdio client works the same way):

```json
{
  "my-plugin": {
    "command": "node",
    "args": ["/path/to/your/package/bin/clat.mjs"],
    "env": { "MY_API_KEY": "…" }
  }
}
```

## Options

`serveClat(plugin, options)` accepts your plugin's exports (or a bare
`apply` function). It resolves once `apply()` has settled — the MCP
`initialize` response is gated on it — and rejects (after shutdown) if
`apply` throws.

| Option | Type | Purpose |
|---|---|---|
| `name` | `string` | MCP serverInfo name; defaults to `plugin.name`, then `dsh-plugin` |
| `version` | `string` | MCP serverInfo version |
| `config` | `unknown` | Passed to `apply(ctx, config)`; validated by your `Config` export when present |
| `toolHints` | `Record<string, ToolHint>` | Declare your tools' effect level (see below) |
| `input` / `output` | streams | Test seam; defaults to process stdio |

## toolHints

DSH tools carry no static effect field; without a hint the host applies
the most conservative gate (`destructive` — every call may prompt).
Declare the truth about your own tools:

| Hint | Meaning |
|---|---|
| `'read-only'` | reads data, no side effects |
| `'network'` | read-only, but reaches the network |
| `'write'` | mutates files or external state |
| `'destructive'` / omitted | conservative default |

## Supported surface

| DSH side | Served as |
|---|---|
| `ctx.tools.register(defineTool(…))` | MCP `tools/list` + `tools/call` (compiled schemas; `output.render` produces model-visible content) |
| `ctx.llm.stream(…)` | MCP `sampling/createMessage` — host session model, host permission gate, usage accounted |
| `ctx.userQuestions.ask(…)` | MCP `elicitation/create` — one form, fields asked sequentially |
| `ctx.web.registerSearchProvider(…)` | built-in `web_search` tool (dsh-tool-web semantics: multi-query merge, URL dedup, max 8 results) |
| `ctx.get` / `ctx.effect` / `ctx.logger` | in-process (`launchEnvironmentOf` falls back to `process.env`; cleanup runs LIFO; logs go to stderr) |

Two-tier policy (matching DSH host semantics):

- **Rejected at startup with a clear error**: a static `inject` export
  declaring spine services (`fs`, `shell`, `sessions`, `agents`,
  `subagents`, `settings`, `commands`, `systemPrompt`, UI services), or
  runtime direct access to `ctx.<spine>`. Those seams are the host's own
  engineering — restructure the capability as leaf tools. Class plugins
  (`extends Service`) are rejected too.
- **Graceful degradation**: runtime `ctx.inject(deps, callback)` wiring for
  optional services (e.g. dsh-settings panels) follows the host
  "not mounted" contract — the callback is skipped with a stderr note and
  the plugin keeps working, exactly as in a UI-less DSH host.

## Known narrowings (v0)

- stdout is protocol-only — never `console.log`; use `ctx.logger` or `console.error`
- `apply()` must settle within the host handshake timeout (10 s in CLAT)
- `multiSelect` questions degrade to comma-separated text
- `ctx.llm.stream({ tools })` is rejected (MCP sampling has no tool-calling); message content is text-only (image blocks error as `NON_TEXT_CONTENT`)
- cancellation is not forwarded to `exec.signal`; the host bounds calls by deadline
- one ask form: ≤ 16 questions, ≤ 16 options per question

Full porting guide — compatibility matrix (algorithm / external-adapter /
spine / UI / content-asset), every narrowing, smoke tests:
[docs/dsh-plugins.md](https://github.com/artec/clat/blob/main/docs/dsh-plugins.md)

## Users without Node: compile your bin

```sh
bun build bin/clat.mjs --compile --outfile clat-my-plugin
```

Point `command` at the resulting binary — the JS runtime ships inside
the executable, end users install nothing.

## Requirements & status

- Node.js ≥ 22.19 on the **author** side (end users need none if you compile with Bun)
- Zero runtime dependencies
- Pinned to the DSH plugin API surface of `dsh-v0.1.0-rc.7` (verified equivalent on rc.8; re-checked unchanged on 0.1.1-rc.1)

A real npm-published plugin — `@deepseek-ai/dsh-web-search-exa` — is
mounted unmodified by this repo's acceptance test:
[examples/exa](https://github.com/artec/clat/tree/main/sdk/dsh-adapter/examples/exa).

## Development

```sh
npm install
npm test        # tsc build + node:test
```

Repository: [artec/clat](https://github.com/artec/clat) ·
[sdk/dsh-adapter](https://github.com/artec/clat/tree/main/sdk/dsh-adapter)

MIT
