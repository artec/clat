# MCP integration

CLAT connects to Model Context Protocol servers over the stdio transport
(local subprocesses) **and the remote Streamable HTTP transport**
(2026-08-19), exposing their tools to the model alongside the native
read tools.

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
  },
  "web-search-prime": {
    "url": "https://open.bigmodel.cn/api/mcp/web_search_prime/mcp",
    "headers": { "Authorization": "Bearer YOUR_API_KEY" }
  }
}
```

The file is optional; without it CLAT runs with native tools only.
The built-in MCP Adapter plugin reads it only after project trust succeeds.
Server subprocesses belong to that Trusted Project Scope and are shared across
runs in the project; closing the project tears them down.

**Startup is non-blocking (2026-08-21)**: connecting servers and listing
their tools happens on a background worker — CLAT becomes ready immediately,
and `clat exec` doesn't pay MCP latency either. A run that starts before
MCP finishes connecting waits for it (bounded at 20s) so the model always
sees the complete tool set; a server slow enough to exceed the cap lands on
the next run instead. `/mcp` shows the three states — `connecting` in the
overview line while servers are still starting, then connected servers with
their protocol/version/tool counts, plus isolated failures.

A server subprocess's **stderr never reaches your terminal** (2026-08-20):
it is drained into a bounded tail buffer (last 20 lines) and appended to
the server's failure message when a connection or `tools/list` fails —
so `npx` download progress or server banners can neither corrupt the
TUI nor hide the actual error.

A server entry with a `url` is a remote Streamable HTTP server (no local
process): each call POSTs one JSON-RPC message with the configured
`headers`, the initialize response's `Mcp-Session-Id` is echoed on
subsequent requests, and JSON or SSE-framed response bodies are both
accepted. Entries without `url` keep the stdio form (`command` / `args`
/ `env`). The remote transport's key never touches disk unless you put
it in this file yourself.

### GLM Coding Plan pack (2026-08-19)

When the active model is the GLM preset **and** an API key is
configured, CLAT automatically injects the four GLM Coding Plan
exclusive MCP servers at mount time (in-memory merge — the key is read
from the model credentials and never written to `mcp.json`):

| Name | Transport | Tools |
|---|---|---|
| `glm-search` | remote `…/mcp/web_search_prime/mcp` | `webSearchPrime` |
| `glm-reader` | remote `…/mcp/web_reader/mcp` | `webReader` |
| `glm-zread` | remote `…/mcp/zread/mcp` | `search_doc`, `get_repo_structure`, `read_file` |
| `glm-vision` | stdio `npx -y @z_ai/mcp-server@latest` (env `Z_AI_API_KEY`; needs Node.js ≥ 18) | 8 vision tools (screenshot→code, OCR, diagram/chart analysis, video) |

A same-named entry in your `mcp.json` always wins — that is the escape
hatch for disabling or replacing any pack server (e.g. point `glm-search`
at your own proxy, or define it with a broken `url` to effectively
disable it). The pack is evaluated at mount: switching the model vendor
takes effect on the next CLAT start.

Connection state is exposed through Application DTOs (`configured`,
`connected`, `connecting`, per-server protocol/version, and isolated
failures). Frontends do not receive `McpServer`, `StdioSession`, or the
Tool Registry.

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
- Remote Tool contributions carry the MCP plugin owner and a revocable lease.
  A name collision is reported and never silently replaces a native or earlier
  remote tool.

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

The transport uses a single reader thread that routes responses by request id
(out-of-order and concurrent responses are handled) and a bounded writer
queue. Shutdown is explicit and idempotent: reject new work and wake pending
calls → close stdin → wait for a bounded grace period → kill/reap if necessary
→ join writer and reader threads. Project-scope teardown revokes MCP Tool
leases before shutting down their server, so a silent server or stale Tool
reference cannot hang or outlive CLAT's project scope. `Drop` remains only a
best-effort fallback.

`Esc` cancellation propagates through `Tool::invoke` into an in-flight
MCP request. CLAT removes the pending response slot, returns within a
short polling interval, and best-effort sends `notifications/cancelled`
with the original request id; the 120-second timeout remains the final
fallback for a server that ignores cancellation.
