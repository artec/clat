# MCP integration

CLAT connects to Model Context Protocol servers over the stdio transport
and exposes their tools to the model alongside the native read tools.

## Configuration

```json
// ~/.clat/mcp.json
{
  "filesystem": {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  },
  "memory": {
    "command": "mcp-memory",
    "env": { "STORE": "/data" }
  }
}
```

The file is optional; without it CLAT runs with native tools only.
Server subprocesses live for the whole CLAT session and are shared across
runs.

## Protocol support

CLAT auto-negotiates both protocol eras. It first probes a disposable
stdio session with modern `server/discover`; if that fails, it starts a
fresh process and performs the legacy `initialize` /
`notifications/initialized` handshake. The fresh fallback prevents the
probe request from contaminating a strict legacy server's state machine.

Supported versions:

| Version | Notes |
|---|---|
| `2024-11-05` | initial spec |
| `2025-03-26` | streamable HTTP, structured output groundwork |
| `2025-06-18` | structured output, elicitation |
| `2025-11-25` | tasks, simplified authorization |
| `2026-07-28` | modern stateless core and per-request envelope |

Every modern request carries the protocol version, CLAT client identity,
and per-request client capabilities in `_meta`. Modern responses must
declare `resultType`; normal `complete` results are supported.
`input_required` multi-round-trip results are rejected with an explicit
unsupported-feature error instead of being mistaken for completed output.

## Tool mapping

- Remote tools are named `mcp_{server}_{tool}`. Both segments are
  normalized to `[a-zA-Z0-9_]`; names longer than 64 characters are
  truncated with a stable hash suffix. Collisions after normalization
  are skipped and reported instead of silently routed.
- MCP annotations refine the approval label into `ExternalRead`,
  `Network`, `Write`, or `Destructive`. The annotations are untrusted
  hints: no remote tool can become the auto-allowed native `Read` effect.
  Missing annotations use the protocol's conservative defaults.

## Security posture

MCP servers are **global capabilities, not project content**:

- Subprocesses are spawned with `~/.clat` as their working directory —
  never the project directory. An untrusted project cannot hijack
  cwd-sensitive commands such as `npx` by placing local files.
- Servers are only started after the project trust gate passes.
- Each server failure (spawn error, handshake failure, tool-list failure)
  only skips that server; the rest of CLAT keeps working.

Resource limits protect against misbehaving or malicious servers:

| Limit | Value |
|---|---|
| single frame (line) size | 4 MiB |
| handshake timeout | 10 s |
| modern discovery timeout | 3 s, then fresh legacy fallback |
| `tools/list` page timeout | 30 s, at most 32 pages |
| tools per server | 512 |
| pagination cursors | repeated cursor aborts |
| `tools/call` timeout | 120 s |
| tool result size | 1 MiB |

The transport uses a single reader thread that routes responses by
request id (out-of-order and concurrent responses are handled), a
bounded writer queue, and a shutdown sequence of close-stdin → grace
period → kill, so a silent server can never hang CLAT's exit.

`Esc` cancellation propagates through `Tool::invoke` into an in-flight
MCP request. CLAT removes the pending response slot, returns within a
short polling interval, and best-effort sends `notifications/cancelled`
with the original request id; the 120-second timeout remains the final
fallback for a server that ignores cancellation.
