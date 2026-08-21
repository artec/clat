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

## Server-initiated requests: sampling & elicitation (2026-08-21)

While CLAT is executing a remote tool, the server may send its own
requests back — the two CLAT serves are **sampling** and
**elicitation**. They run through a transport-agnostic host bridge
(`src/plugin_host.rs`) that owns the permission gate, usage accounting,
and user questions; stdio is the reference transport (HTTP serves
requests found inside POST response streams).

- **sampling** (`sampling/createMessage`) — the server asks CLAT to run
  the configured model on its behalf. Every call passes the **permission
  gate** first: the approval dialog's arguments are the **full outbound
  payload** — the complete `systemPrompt`, every message (role + full
  text), maxTokens, and temperature — not a truncated preview, so what
  you review is exactly what leaves for the provider (the total outbound
  text is capped at 262,144 **characters** — a character cap, not bytes;
  larger requests are rejected outright). Deny/unavailable fail closed.
  Approved calls run on the session's model (`modelPreferences`/
  `includeContext` are deliberately ignored — the latter never sends
  conversation context to a server). Their token usage is accounted: it
  lands in the current step's `assistant/message` usage (so live and
  replayed session stats stay equal) and in the run totals.
- **sampling spend budget (per run)** — plugin sampling also passes a
  **budget gate** that is independent of the permission mode: at most
  **64 requests per run** across every transport (MCP, WASM, DSH
  adapter), plus a **1,000,000-token spend guard**. A reservation
  (input estimate at ~4 characters/token + the requested max output) is
  taken **before** the model call and fails closed when exceeded; it is
  reconciled against the actual usage when the provider reports one
  (otherwise the reservation stands). The token figure is an
  **approximate guard, not a precise ceiling** — the heuristic can
  underestimate CJK or code — so the strict bounds are the 64-request
  hard cap and the 8,192-token per-request output clamp. Full Access
  skips the approval dialog, not the budget. The budget resets on the
  next run.
- **elicitation** (`elicitation/create`, protocol ≥ 2025-06-18) — the
  server asks **you** a form. CLAT asks the fields one at a time through
  the same dialog the `ask_user` tool uses (first question carries the
  form's message): booleans become yes/no, enums become option lists,
  strings/numbers are free input (numbers re-ask up to twice on a bad
  parse). Esc cancels the whole form; declining answers `declined`.
  Headless `clat exec` has no frontend: the server receives a clean
  error. v1 supports the primitive field subset (string / number /
  boolean / enum, ≤16 fields); multi-select, `mode:"url"`, and nested
  schemas are rejected with explicit errors.

Only requests arriving during an active run are served; anything else
gets an error ("no active run") — an idle connection is not a standing
model-call channel. Unknown server requests (other than `ping`, which
is answered with `{}`) get `-32601`; a panicking handler is contained
as `-32603`.

While a request **on that same connection** is in flight, the pending
`tools/call` deadline extends (60 s steps, capped at +10 min) — your
thinking time on an elicitation never kills the tool call. The pending
count is per server: one server's (or a WASM plugin's) in-flight request
never extends an unrelated server's deadline. `Esc` still cancels
immediately.

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
| `tools/call` timeout | 120 s (+60 s/step while a server request is pending **on that connection**, ≤ +10 min) |
| tool result size | 1 MiB |
| server request queue | 16 (flood drops with diagnostics) |
| sampling output | ≤ 8192 tokens; ≤ 32 messages; text content only |
| sampling outbound text | ≤ 262,144 characters, systemPrompt + messages (character cap, not bytes) |
| sampling budget | 64 requests per run (hard) + 1,000,000-token spend guard (approximate; see above) |

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
