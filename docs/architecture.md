# Architecture

This document is the stable map of CLAT's runtime boundaries. It explains
where responsibilities live and which contracts frontends and extensions may
depend on. User-facing operation belongs in [Using CLAT](usage.md); detailed
persistence and security rules belong in [Persistent state](storage.md) and
[Permissions](permissions.md).

## Architectural invariants

CLAT is organized around a small set of non-negotiable boundaries:

1. **The core never depends on a frontend.** Terminal, browser, headless, IDE,
   and future clients consume core ports and DTOs.
2. **Run state is observed through events.** Frontends do not poll or reach
   into the agent loop.
3. **Side effects cross one permission pipeline.** Frontends supply an
   approver; they do not implement policy semantics.
4. **Persistence is owned by core.** Frontends never write session or control
   files directly.
5. **Providers and tools are replaceable adapters.** No vendor or extension
   protocol defines the agent algorithm.
6. **Lifetimes are explicit.** Bootstrap, trusted-project, and run resources
   close in a defined order and background workers have owners.
7. **One binary remains the product boundary.** Rust implementations and
   explicitly configured WASM components do not require a user-side language
   runtime.

## System shape

```text
                        local clients
       ┌───────────┬────────────┬──────────────┐
       │ TUI       │ clat exec  │ clat serve  │
       │ terminal  │ stdout     │ HTTP/SSE/PWA│
       └─────┬─────┴──────┬─────┴──────┬───────┘
             └────────────┴────────────┘
                           │
                           ▼
                BootstrapApplication
                  zero-write preflight
                           │ trust transition
                           ▼
             TrustedProjectApplication
       sessions · config · tools · providers · plugins
                           │ start_run
                           ▼
                      Run Scope
             model → tools → model, unbounded
                  │                   │
                  ▼                   ▼
          RunEvent / EventSink   Session journal

clat dsh is a separate client path: it reuses the TUI shell but projects
events and commands from a DSH web host instead of mounting local core state.
```

The Application facade is the only supported local-client entry point. It
offers trust transition, session commands, snapshots, model state, permission
mode, run lifecycle, and frontend-neutral command dispatch. Concrete storage,
provider, MCP, plugin, and registry types remain crate-private.

## Lifetimes and static composition

CLAT uses a Rust-native plugin kernel for compile-time components. Explicit
catalogs choose and order built-in plugins; there is no dynamic Rust library or
JavaScript discovery. Configured WebAssembly components are the only extension
code loaded into the CLAT process; MCP executables remain separate processes.
Dynamic native libraries are deliberately not a plugin ABI: Rust's compiler ABI
is not stable enough for a durable market contract. Rust-authored third-party
plugins compile to the versioned WIT/WASM component boundary instead.

| Scope | Lifetime | Owns |
|---|---|---|
| Bootstrap | process open → trust decision | control-format classification and read-only trust lookup |
| Trusted Project | trust accepted → project close | storage lease, sessions, providers, tools, prompts, commands, MCP/WASM adapters, monitors, compaction, titles, todos |
| Run | one active agent run | cancel token, permission approver, user asker, plugin-host context, model/tool worker |

Bootstrap deliberately has no plugin scope. A catalog is validated before any
plugin mount can create a side effect. Duplicate IDs or services, missing or
mis-scoped dependencies, parent-service overrides, and dependency cycles fail
early.

Mount and teardown are symmetric:

- plugins on the same dependency layer preserve catalog order;
- a failed mount rolls back the current and prior mounts in reverse order;
- close is reverse-ordered, idempotent, and panic-isolated;
- a parent scope refuses to close while child scopes are active;
- collection contributions carry a plugin owner and a revocable lease.

Provider factories, tools, prompt fragments, command handlers, tool
middleware, and post observers use domain registries. Registries freeze before
a run, preventing mid-run additions from changing the model's tool surface.
Teardown can still revoke existing leases.

## Application boundary

The facade is split structurally:

```text
BootstrapApplication::open(project, storage_root)
├── classify the control-plane sentinel and legacy state
├── validate session-root shape read-only
├── query trust read-only
└── authorize_and_mount(ProjectAuthorization)
      ├── acquire the storage-root kernel lease
      ├── persist trust when explicitly authorized
      ├── publish/upgrade the control-plane sentinel
      └── mount TrustedProjectApplication
```

Pre-trust code has no API for project reads, credentials, sessions, tools,
models, MCP, or control-plane writes. `authorize_and_mount` is the only trust
write path.

`TrustedProjectApplication` exposes use cases rather than subsystems. Examples
include:

- `session_list`, `session_info`, `new_session`, `switch_session`, and rename;
- model state/profile reads and writes;
- `dispatch_command` for the shared slash-command catalog;
- permission-mode get/set through the core mode cell;
- `start_run`, steering, cancellation, and join;
- lightweight frontend DTOs such as `WorkbenchSnapshot`.

DTOs contain display-ready facts, not subsystem handles. The workbench
snapshot deliberately excludes credentials and transcript replay; the server
combines it with its active-run ledger at the wire boundary.

## Agent run lifecycle

The single-agent runtime is the daily-driver baseline. One trusted project can
have at most one active run.

```text
user prompt
  → validate/prepare attachments
  → journal turn/start + user/message durably
  → freeze extension registries
  → create Run Scope
  → request model stream
  → collect text/reasoning/tool calls/usage
  → permission check each tool call
  → middleware → Tool::invoke → post observers
  → append results and continue the model loop
  → durable turn/end
  → publish exactly one run terminal event
```

The loop has no turn-count limit. It stops on model completion/refusal,
cancellation, failure, or the per-run token spend guard. Tool failures and
permission denials become structured `ToolResult` errors so the model can
adapt without aborting the whole run.

Steering is accepted at model-request boundaries. The queue is sealed
atomically before a terminal event becomes visible, so a late message either
joins the active run or becomes a new submission; it is never stranded.

Cancellation is cooperative but propagated across the full call path: model
SSE reads, native tools, command process trees, MCP waits, WASM execution, and
plugin-host model/question waits observe the shared token. Components that may
block outside cooperative cancellation have bounded join policies.

## Event contracts

One run produces two related streams:

- **`RunEvent`** is the live client protocol. `EventSink` receives run start,
  model requests and stream deltas, tool requests/results, permission checks,
  retry metadata, and exactly one terminal outcome.
- **`SessionEvent`** is the durable DSH-compatible journal vocabulary. It is
  folded into transcript, model-context surface, title, todo, permission,
  usage, and compaction projections.

The live and durable streams are ordered so a frontend cannot observe success
that persistence later contradicts. The initial user turn is durable before
the model call. Approval requests and decisions are journaled around the human
gate. A successful run terminal is emitted only after closing journal events
commit; failure to persist turns the client outcome into `RunFailed`.

Application-wide facts such as quota refreshes, title updates, compaction, and
MCP startup use `ApplicationEvent`, not new run events.

`clat exec --json` and `clat serve` serialize the same v1 RunEvent envelopes.
Changing an event variant or required field is therefore a client-protocol
change, not an internal refactor.

## Conversation and context model

At runtime the provider-neutral conversation is an ordered `ModelItem` list:

- `User` and `Assistant` text, with optional reasoning;
- paired `ToolCall` and `ToolResult` items;
- opaque `ProviderState` required for a vendor's next turn.

The journal, not a frontend copy, is the durable source of truth. Projections
derive two intentionally different views:

- the **transcript** preserves the full human-visible history;
- the **surface** supplies model context and applies compaction shadowing.

Tool-result pruning reduces transient model context. Compaction appends a
summary marker and shadows older surface ranges without deleting original
events. Preset context windows seed automatic compaction; manual configuration
remains authoritative.

Provider-specific replay stays opaque to `Run`. For example, OpenAI Responses
reasoning items travel through `provider_state`, while DeepSeek-compatible
`reasoning_content` is attached only to assistant messages that made tool calls.

## Providers

`Model` exposes one streaming, provider-neutral protocol:

```text
ModelRequest
├── instructions
├── ModelItem[]
├── ToolDefinition[]
├── ModelOptions
└── CancelToken
       │
       ▼
Model::stream
       ├── text/reasoning deltas
       ├── tool argument deltas and completed calls
       ├── usage
       └── response completion
```

Provider factories create a fresh adapter for each retry attempt. The two
built-in protocol families are OpenAI Responses and streaming
`/chat/completions`-compatible APIs. Managed request fields cannot be
overridden by user-provided extra body data. See [Providers](providers.md) for
preset and retry details.

## Native tools and permission pipeline

Built-in read tools are `list_files`, `read_file`, and `search`. They enforce
byte, depth, entry, and result limits. Project-relative paths reject traversal
and are resolved with symlink-aware capability discipline. Absolute reads are
allowed by CLAT's current permission contract.

Trusted projects also receive:

- `write_file` — bounded atomic replacement with permission preservation;
- `edit_file` — one exact unique replacement plus conflict revalidation;
- `run_command` — project-root command execution with timeout, bounded output,
  and whole-process-tree termination.

Writes are project-relative in Read Only, Project Write, and headless runs;
Full Access can unlock absolute writes. Command execution remains rooted in the
project. Every call flows through:

```text
final tool arguments
  → PermissionPolicy
  → ordered middleware
  → Tool::invoke
  → ordered post observers
```

Middleware never sees an unapproved side effect. The frontend-supplied
`PermissionApprover` only presents and returns a decision. Effect
classification, escalation options, write scope, and fail-closed behavior stay
in core. See [Permissions](permissions.md).

## Extension bridge

CLAT has three extension delivery paths with one semantic center:

| Path | Boundary | Typical use |
|---|---|---|
| MCP stdio | subprocess + JSON-RPC | local tools in any language |
| MCP Streamable HTTP | remote HTTP session | hosted services |
| WebAssembly component | in-process WIT + WASI capabilities | portable local tools without a runtime dependency |

MCP framing/session code lives under `mcp/`. The transport classifies incoming
responses, requests, and notifications and keeps reader/writer queues bounded.
The `McpAdapterPlugin` starts configured servers only after project trust. Its
background startup does not block the TUI; `start_run` waits up to 20 seconds
for the initial tool and marked DSH system-prompt surface before freezing both
registries. Ordinary MCP prompts are not imported automatically.

`plugin_host.rs` owns transport-neutral host callbacks:

- **sampling** lets an extension borrow the active model after payload,
  per-run spend, and permission gates;
- **elicitation** lets it ask the user through the injected `UserAsker` port.
- **context** exposes a bounded, detached run/session mirror;
- **host tool calls** reach only an explicit native allowlist through the same
  permission policy, project fence, cancellation token, and execution pipeline.

Usage returns to the same run ledger and journal event as ordinary model
usage. Requests are served only during an active run. MCP, WASM, and the DSH
adapter share these semantics instead of implementing parallel policy paths.

The DSH adapter is a static Cordis compatibility layer, not only a tool
wrapper. It accepts function/object plugins and `Service` classes, implements
process-local services, events/effects, system-prompt assembly, web providers,
sampling, and elicitation, then projects portable contributions through MCP.
Its `ctx.fs` / `ctx.shell` services call the Rust-owned host bridge, while
`ctx.sessions` / `ctx.agents` are explicitly read-only current-run mirrors.
Mutable session/agent state, subagents, permissions, settings, commands, UI,
and scoped dynamic restart remain Rust-owned core services.

WASM components implement `wit/plugin.wit`. The host supplies no ambient
environment, stdio, or network capability; filesystem preopens come from the
permission mode and separate hash-bound write grants. Fuel, memory, and epoch
interruption bound guest execution. See [MCP integration](mcp.md),
[WASM plugins](wasm.md), [DSH plugin porting](dsh-plugins.md), and the
[plugin package model](plugins.md).

## Persistence

Core persistence has two layers:

- append-only, zstd-framed DSH-compatible session journals;
- a small JSON control plane for settings, credentials, trust, workspace
  selection, and listing projections.

A kernel lease prevents concurrent CLAT processes from writing the same
storage root. Atomic file replacement, fsync discipline, append commit states,
torn-tail recovery, projection reconciliation, and two-phase session switching
keep memory, disk, and frontend outcomes aligned. Frontends consume
`SessionService` and Application use cases, never filesystem paths.

See [Persistent state](storage.md) for the file map and failure semantics.

## Frontends

### TUI

`tui.rs` and `tui/` own terminal initialization, input, rendering, dialogs,
view models, and channel multiplexing. They do not create providers, tools,
storage, MCP processes, or run workers. An approver adapter and user-question
adapter translate core requests into dialog state.

### Headless runner

`exec.rs` maps stdout/stderr/stdin to the same Application ports. It injects a
request-scoped terminal approver, denies side effects when stdin is not a TTY,
supports versioned NDJSON events, and owns no process-global signal handler.
`main.rs` translates Ctrl-C into an injected `ExecCancel`.

### Local server and PWA

`serve.rs` and `serve/` implement a loopback-only HTTP+SSE frontend. The server
bridges permission requests to `approval.requested` events and
`approval.respond` RPC calls, exposes core command/session/run use cases, and
uses the same event envelopes as headless JSON output.

The embedded `web/` workbench is a projection client. Static assets and pairing
shell contain no credential. Authenticated API calls carry the persistent
`~/.clat/web-token` as Bearer; no URL or Cookie token path exists. Browser
storage contains only UI preferences and the origin-scoped paired token, never
conversation facts.

### DSH client

`dsh/` implements the HTTP/WebSocket client and host lifecycle for `clat dsh`.
The TUI reducer maps DSH events into the same presentation state, but commands
and persistence stay on the DSH host. Local access to `~/.dsh/storages` is
read-only and used only to populate session selection.

## Source map

| Path | Responsibility |
|---|---|
| `src/application.rs`, `src/application/` | client-neutral use-case facade, DTOs, run/session lifecycle |
| `src/run.rs` | agent loop |
| `src/model.rs`, `src/providers/` | provider-neutral model contract and adapters |
| `src/tool.rs`, `src/native_tools.rs` | tool contract and native tools |
| `src/permission.rs` | effects, modes, policies, approver port, write scope |
| `src/event.rs` | RunEvent vocabulary and EventSink |
| `src/interaction.rs`, `src/media.rs` | user-question port and image preparation |
| `src/plugin/`, `src/plugins/` | plugin kernel and built-in catalogs/adapters |
| `src/plugin_host.rs` | extension sampling and elicitation semantics |
| `src/mcp.rs`, `src/mcp/` | MCP config, transport, client, and tool mapping |
| `src/session/` | journals, projections, recovery, checkpoints |
| `src/control_storage/` | JSON control plane and workspace registry |
| `src/command.rs` | frontend-neutral slash-command contract |
| `src/tui.rs`, `src/tui/` | terminal frontend |
| `src/exec.rs` | headless frontend |
| `src/serve.rs`, `src/serve/`, `web/` | local API, SSE, and embedded PWA |
| `src/dsh/` | DSH client protocol and host lifecycle |
| `src/demo.rs` | deterministic offline composition |
| `src/upgrade.rs` | authenticated self-update |
| `wit/`, `sdk/clat-plugin/` | WASM contract and author SDK |
| `sdk/dsh-adapter/` | static Cordis/DSH compatibility adapter package |

## Adding a core capability

Before adding code, identify its owner and contract:

1. If another client would need the behavior, implement it in core and expose
   a facade use case or event—not a TUI method.
2. If it contributes a provider, tool, prompt, command, middleware, or
   observer, register it through the correct scoped catalog with an owner
   lease.
3. If it creates durable events, update admission validation, projection
   folding, checkpoint/restore, and live/replay parity together.
4. If it performs a side effect, classify it and route it through the shared
   permission policy before middleware or invocation.
5. If it owns a thread, process, channel, or child scope, define cancellation,
   join, and reverse teardown behavior.
6. If it changes user-visible behavior or a public event shape, update the
   matching public document in the same change.

The architecture test suite enforces the core-to-frontend dependency direction.
Behavioral tests must also exercise lifecycle sequences—open, run, switch,
close, reopen—because static boundaries alone cannot prove state correctness.
