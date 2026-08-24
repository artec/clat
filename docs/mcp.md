# MCP integration

CLAT connects to Model Context Protocol servers over local stdio subprocesses
and remote Streamable HTTP. Their tools join the same registry, permission
pipeline, cancellation path, and run event stream as native tools.

Use MCP for capabilities that need a separate process, network service, or
language runtime. For portable in-process local tools, compare
[WASM plugins](wasm.md).

## Configuration

For a distributable local package, use `clat plugin install <package-dir>` with
an `mcp-stdio` manifest; see [CLAT plugins](plugins.md). The package entry must
be an executable and is launched with its immutable artifact directory as cwd.

`~/.clat/mcp.json` remains the optional user-managed escape hatch:

```json
{
  "filesystem": {
    "command": "npx",
    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
  },
  "memory": {
    "command": "mcp-memory",
    "env": { "STORE": "/data" },
    "cwd": "mcp-work"
  },
  "web-search": {
    "url": "https://example.com/mcp",
    "headers": { "Authorization": "Bearer YOUR_API_KEY" }
  }
}
```

Each entry chooses exactly one transport:

- **stdio** — `command`, optional `args`, `env`, and fixed `cwd`;
- **Streamable HTTP** — `url` and optional `headers`.

Restart CLAT after changing the file. `/mcp` shows configured, connecting, and
connected counts, plus transport, negotiated versions, tool counts, and
isolated failures.

## Project and startup lifecycle

MCP servers are global capabilities mounted inside a Trusted Project Scope.
Nothing is spawned or contacted before project trust succeeds.

Startup is asynchronous: CLAT becomes interactive while a background worker
connects servers and lists tools. Servers that advertise MCP prompts are also
queried, but CLAT automatically imports only entries explicitly marked by the
DSH adapter as system-prompt contributions. The first run waits up to 20
seconds for that initial surface before the tool and prompt registries freeze.
If a server finishes after that freeze, its registrations are rejected and
`/mcp` reports the failure; restart CLAT after fixing or warming that server.

User-configured stdio subprocesses use `~/.clat` as their default working
directory, never the project directory. A relative explicit `cwd` also resolves
under `~/.clat`. Installed packages use their immutable artifact root so
bundled resources remain reachable. The DSH adapter receives the real project
root only as the controlled prompt argument named `cwd`. Servers are shared
across runs in the mounted project and closed when that project scope closes. Teardown revokes
prompt and tool leases before closing stdin, waiting a bounded grace,
killing/reaping if needed, and joining I/O threads.

A server failure is isolated. Spawn, handshake, `tools/list`, name collision,
or registration failure removes only that server's contributions. A marked
DSH prompt discovery/resolution failure is reported but does not discard tools
that the same server registered successfully.

### Stderr diagnostics

Subprocess stderr never writes directly into the TUI or protocol stream. CLAT
keeps a bounded tail of the last 20 lines and attaches it to startup failures.
Key-shaped values such as Bearer tokens and common API-key forms are redacted
before a diagnostic reaches `/mcp` or status surfaces. Redaction limits
secondary leakage; it does not prevent the subprocess from seeing inherited
environment variables.

## Transport behavior

### Stdio

CLAT owns one bounded writer queue and a dedicated reader thread. The reader
routes out-of-order responses by request id and dispatches server-initiated
requests without blocking response progress.

Modern discovery is probed with a disposable subprocess. If the probe fails,
CLAT starts a fresh subprocess before performing the legacy initialize /
initialized handshake. This prevents a strict legacy state machine from seeing
an unsupported probe followed by initialize in the same process.

### Streamable HTTP

Each client request POSTs a JSON-RPC message to the configured URL. Responses
may be plain JSON or SSE-framed JSON. If initialize returns `Mcp-Session-Id`,
CLAT echoes it on later requests.

Remote headers are caller-provided and stay in the in-memory configuration.
CLAT never writes them elsewhere, but the source `mcp.json` is plaintext if the
user stores secrets there.

## GLM Coding Plan pack

When GLM 5.3 Coding Plan and its API key are active at project mount, CLAT
merges four provider-supplied capabilities in memory:

| Name | Transport | Tools |
|---|---|---|
| `glm-search` | Streamable HTTP | `webSearchPrime` |
| `glm-reader` | Streamable HTTP | `webReader` |
| `glm-zread` | Streamable HTTP | `search_doc`, `get_repo_structure`, `read_file` |
| `glm-vision` | stdio `npx -y @z_ai/mcp-server@latest` | screenshot/code, OCR, diagram/chart, and video tools |

The API key comes from model credentials and is not copied into `mcp.json`.
`glm-vision` requires Node.js 18 or newer because it is a subprocess; this is an
exception to CLAT's own one-binary runtime, not a requirement for core CLAT.

A same-named user entry wins over the automatic pack and is the supported way
to replace one server with a pinned version, proxy, or private implementation.
The pack is evaluated at mount, so changing provider takes effect on the next
CLAT start.

## Protocol support

CLAT supports the legacy and modern MCP eras:

| Version | Behavior used by CLAT |
|---|---|
| `2024-11-05` | legacy initialize and tools |
| `2025-03-26` | Streamable HTTP groundwork |
| `2025-06-18` | structured output and elicitation |
| `2025-11-25` | current legacy initialize version |
| `2026-07-28` | stateless modern discovery and per-request envelope |

Modern requests carry protocol version, CLAT client identity, and per-request
capabilities in `_meta`. Responses must declare `resultType`. Normal
`complete` results are supported; `input_required` multi-round-trip results
return an explicit unsupported-feature error rather than being mistaken for a
finished call.

## Tool mapping and effects

MCP tools become `mcp_{server}_{tool}`. Both name segments are normalized to
ASCII letters, digits, and underscores. Names longer than 64 characters use a
stable hash suffix. A collision after normalization is reported and skipped;
it never silently replaces a native or earlier extension tool.

MCP annotations are untrusted effect hints:

| Server annotation | CLAT effect |
|---|---|
| read-only + closed world | `ExternalRead` |
| read-only + open world | `Network` |
| non-read-only + non-destructive | `Write` |
| destructive, missing, or ambiguous | `Destructive` |

No remote tool can claim the native `Read` effect. The resulting effect flows
through the normal mode table; see [Permissions](permissions.md#mcp-effects).

## CLAT host-services extension

CLAT advertises experimental capability `io.artec.clat/hostServices` version
`0.1.0`. A server that advertises the matching server capability may use:

- `io.artec.clat/context/get` — a bounded detached snapshot of the active
  project/run/session context;
- `io.artec.clat/tools/call` — an audited native host-tool call;
- `io.artec.clat/context/changed` — a best-effort CLAT-to-stdio-server
  notification carrying the latest snapshot, or `null` when the run ends.

Only `list_files`, `read_file`, `search`, `write_file`, `edit_file`, and
`run_command` are callable. Each invocation still passes through the current
run's permission policy, project path fence, cancel token, and tool execution
pipeline. Credentials, approver objects, registries, and journal writers never
cross the protocol. Notifications never block a CLAT run; `context/get` is the
authoritative snapshot and the fallback for HTTP peers. Generic MCP servers
that do not opt in receive no context notifications.

The extension exists so the DSH adapter and WASM/WIT guests consume one
language-neutral host contract. It is not a general path for recursively
calling arbitrary MCP tools.

## Server-initiated sampling

During an active `tools/call`, a server can request
`sampling/createMessage`. The transport forwards it to the shared
`PluginHostBridge`; the MCP client does not call a provider directly.

Sampling passes three gates:

1. **Payload** — at most 32 messages, text-only content, at most 262,144
   characters across system prompt and messages, and at most 8,192 requested
   output tokens.
2. **Run budget** — no more than 64 extension-sampling requests per run across
   MCP, WASM, and DSH adapter calls, plus an approximate 1,000,000-token spend
   guard. Reservation happens before the provider request and is reconciled
   with actual usage.
3. **Permission** — the approval arguments contain the complete outbound
   payload, not a shortened preview. Full Access skips this dialog; payload and
   budget gates remain.

Approved sampling uses the active session model. Server-supplied provider/model
preferences and `includeContext` do not select another provider or expose the
conversation. Usage is added to the current run step so live and replayed
session totals remain equal.

Only active-run requests are served. An idle MCP connection is not a standing
channel for model calls.

## Server-initiated elicitation

For protocol `2025-06-18` or newer, a server can request
`elicitation/create`. CLAT translates one primitive form into the same
`UserAsker` port used by the native `ask_user` tool:

- strings and numbers become text inputs;
- booleans become yes/no;
- enums become single-choice lists;
- fields are asked sequentially, with the form message on the first question.

The supported subset is at most 16 fields with at most 16 choices each.
Nested schemas, multi-select, URL mode, and other unsupported shapes return
explicit errors. Invalid numbers are re-asked up to twice. Esc cancels the form;
decline returns `declined`.

Headless `clat exec` has no interactive question frontend, so elicitation fails
cleanly there.

While a request on one connection is waiting for sampling or elicitation, that
connection's `tools/call` deadline extends in 60-second steps up to ten extra
minutes. Pending counts are per server; one plugin cannot extend another
server's timeout. User cancellation still takes effect immediately.

## Cancellation and server requests

`Esc` or client cancellation removes the pending response slot and
best-effort sends `notifications/cancelled` with the original request id. The
server may ignore it; CLAT's local wait still ends promptly and the final
timeout remains a fallback.

Unknown server requests receive JSON-RPC `-32601`; `ping` receives `{}`.
Handler panics are contained and returned as `-32603`. A bounded queue rejects
request floods with diagnostics rather than blocking transport reads.

## Security posture

Treat every MCP server as executable code or a remote service you explicitly
trust.

### Local subprocesses

- They start only after project trust.
- Their working directory is `~/.clat`, preventing a project from hijacking
  cwd-sensitive launchers such as `npx`.
- They inherit CLAT's full process environment, then receive configured `env`
  values. A server can therefore see unrelated exported secrets. Launch CLAT
  from a clean environment or use a wrapper such as `env -i` for untrusted
  servers.
- `glm-vision` uses `@latest`, which is a supply-chain surface. Override it in
  `mcp.json` with a same-named pinned package version if reproducibility matters.

### Remote servers and secrets

- `mcp.json` is user-managed plaintext. Authorization headers stored there are
  not encrypted at rest.
- Remote servers see tool arguments the model sends and any sampling payload
  the user explicitly approves.
- No CORS/browser trust boundary applies here; these requests originate from
  the CLAT process.

Stderr redaction and permission prompts reduce accidental exposure but are not
process isolation or secret storage. Prefer OS disk protection and narrowly
scoped credentials.

## Resource limits

| Surface | Limit |
|---|---:|
| one stdio frame | 4 MiB |
| legacy handshake | 10 s |
| modern discovery probe | 3 s, then fresh legacy fallback |
| one `tools/list` page | 30 s |
| `tools/list` pages | 32 |
| tools per server | 512 |
| marked DSH prompts per server | 128 |
| one resolved DSH system prompt | 256 KiB |
| one `tools/call` | 120 s, extendable only for same-connection host requests |
| total host-request extension | 10 min |
| tool result | 1 MiB |
| inbound server-request queue | 16 |
| sampling messages | 32, text only |
| sampling output request | 8,192 tokens |
| sampling outbound text | 262,144 characters |
| shared sampling budget | 64 requests/run + approximate 1,000,000 tokens |

Repeated pagination cursors abort listing. Bounded queues never let a slow or
flooding server block the agent or shutdown path.

## Troubleshooting

1. Open `/mcp` and check whether the server is `connecting`, connected, or
   failed.
2. Read the attached stderr tail for stdio launch/package errors.
3. Run the command manually from `~/.clat` with the same environment.
4. Confirm the entry uses either `command` or `url`, not both.
5. If a tool is missing, check normalized-name collisions and the 512-tool cap;
   if a DSH prompt is missing, confirm the adapter marker and 128-prompt cap.
6. If the first run freezes the registry before a very slow server is ready,
   restart CLAT after fixing or warming that server; the project-scope registry
   does not thaw between runs.

For DSH plugin-specific packaging problems, use the
[DSH plugin porting guide](dsh-plugins.md).
