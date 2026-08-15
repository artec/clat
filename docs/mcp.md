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

CLAT speaks the **legacy handshake era** of the protocol
(`initialize` / `notifications/initialized`) and accepts these negotiated
versions:

| Version | Notes |
|---|---|
| `2024-11-05` | initial spec |
| `2025-03-26` | streamable HTTP, structured output groundwork |
| `2025-06-18` | structured output, elicitation |
| `2025-11-25` | tasks, simplified authorization |

The MCP 2.0 stateless core (`2026-07-28`) removed the initialize
handshake and moved per-request metadata into `_meta`. CLAT does **not**
implement that envelope yet: a server that rejects the legacy handshake
is reported as an error (with an explicit "MCP 2.0 not yet supported"
message) rather than being silently mis-handled.

## Tool mapping

- Remote tools are named `mcp_{server}_{tool}`. Both segments are
  normalized to `[a-zA-Z0-9_]`; names longer than 64 characters are
  truncated with a stable hash suffix. Collisions after normalization
  are skipped and reported instead of silently routed.
- Every MCP tool is classified as `Execute` — see
  [permissions](permissions.md).

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
| `tools/list` page timeout | 30 s, at most 32 pages |
| tools per server | 512 |
| pagination cursors | repeated cursor aborts |
| `tools/call` timeout | 120 s |
| tool result size | 1 MiB |

The transport uses a single reader thread that routes responses by
request id (out-of-order and concurrent responses are handled), a
bounded writer queue, and a shutdown sequence of close-stdin → grace
period → kill, so a silent server can never hang CLAT's exit.
