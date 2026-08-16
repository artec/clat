# Architecture

## Core and static plugin composition

CLAT has a UI-independent Application facade over a small, Rust-native static
plugin kernel. Plugin implementations are compiled into the single `clat`
binary; explicit catalogs select and order them at runtime. CLAT does not load
Rust dynamic libraries, WASM, JavaScript, or automatically discovered code.

```text
TUI / demo / future desktop or headless client
                    │
                    ▼
 BootstrapApplication ── trust transition ──► TrustedProjectApplication
                    │                              │
                    ▼                              ▼
              Bootstrap Scope              Trusted Project Scope
                                                   │
                                                   ▼
                                               Run Scope
                    ┌──────────────────────────────┴────────────────────┐
                    ▼                                                   ▼
       ServiceRegistry + PluginManager                 EventSink / RunEvent
       dependency plan, mount, rollback,               stable client protocol
       child scopes, reverse teardown
```

There are three scope-specific explicit catalogs:

| Scope | Lifetime | Built-in responsibilities |
|---|---|---|
| Bootstrap | Application open → exit | shared storage backend, narrow `TrustStore` only |
| Trusted Project | trust accepted → project close | Session/Config stores, Tool/Provider/Prompt registries, native tools, MCP adapter, permission and Agent services, monitor |
| Run | one active run | `CancelToken` and injected `PermissionApprover`; worker ownership stays in Application |

The catalog is validated before the first plugin produces a side effect.
Duplicate plugin IDs or services, missing dependencies, scope mismatches, parent
service overrides, and required/optional dependency cycles all fail early.
Plugins on the same dependency layer retain catalog order. A failed mount rolls
back the current plugin and every prior plugin in reverse order while retaining
the primary error and all cleanup errors. Scope close is idempotent, reverse
ordered, panic-isolated, and refuses to close a parent with active children.

Services have typed `ServiceKey<T>` values. Collection extension points use
domain registries instead of competing service providers: Provider factories,
Tools, Prompt fragments, Tool middleware, and post observers each carry an
unforgeable plugin owner and a revocable lease. Registries freeze before a run,
but existing leases can still revoke contributions during teardown.

The kernel, domain DTOs/contracts, and frontend ports are deliberately **not**
plugins. `Project`, `ModelItem`, `RunEvent`, `EventSink`, `PermissionApprover`,
and Application DTOs remain ordinary typed interfaces. `PluginContext`, raw
registries, storage, concrete providers, MCP clients, and the `Run` algorithm
are crate-private so frontends cannot bypass the facade.

`Run` still owns the streaming model → tool → model algorithm, capped at a
configurable number of turns. Every observable step (`ModelRequested`,
`ModelStream`, `ToolRequested`, `ToolFinished`, `PermissionChecked`,
`RunCompleted`, …) is emitted through `EventSink`. Application-level facts such
as quota refreshes use `ApplicationEvent`; plugin hooks never enter the
`RunEvent` protocol.

Tool execution failures are returned to the model as structured error results
so the agent can recover. Cancellation is cooperative: the same `CancelToken`
is checked between turns, before each tool call, between SSE chunks, inside
native tools, and inside MCP waits.

## Agentic loop (v0.4.0)

CLAT is now a **minimal viable agent**: the read → change → verify loop is
closed. What the agent can do autonomously:

```text
inspect (list/read/search)
   → locate a problem
      → change it (edit_file / write_file)
         → verify (run_command: build, test)
            → read the failure → change again → until green
```

Current boundaries of that capability, and what is deliberately **not**
built yet:

| Capability | State |
|---|---|
| Multi-turn run with cancellation and failure persistence | done |
| Read tools (list/read/search), project-scoped | done |
| Write/edit tools with capability-relative path binding | done |
| Command execution with process-tree ownership, timeout, output budget | done |
| Permission review for every side-effecting call | done |
| Project context injection (CLAT.md, git state into the system prompt) | not built — the system prompt is a fixed string today |
| Context management (compaction/truncation for long sessions) | not built — `message_items` grows unbounded |
| Turn budget configurability | not built — fixed 32 turns, exceeding it fails the run |
| Headless Application API | done — tests and `demo` execute without TUI; a public `clat exec` command is still not built |
| Subagents, image input, multi-agent orchestration | deferred by constitution |

The intended growth order is: project context injection → context management →
a user-facing headless CLI, each driven by real dogfood need. The underlying
Application API is already frontend-neutral.

## Model Protocol v0.1

CLAT models exchange typed, provider-neutral data:

```text
ModelRequest
├── instructions
├── ModelItem[]
├── ToolDefinition[]   (JSON Schema)
├── ModelOptions
└── CancelToken
       │
       ▼
Model::stream(...)
       │
       ├── TextDelta
       ├── ToolArgumentsDelta
       ├── ToolCallCompleted
       ├── ReasoningDelta
       ├── ReasoningSummaryDelta
       ├── Usage
       └── ResponseCompleted
       │
       ▼
ModelResponse
├── text
├── tool_calls[]
├── finish_reason
├── usage
├── provider_state
└── reasoning
```

Provider-specific state can be preserved for subsequent turns without
leaking provider wire formats into `Run` (the OpenAI Responses reasoning
items travel through `provider_state`, for example).

## Conversation model

A conversation is an ordered list of `ModelItem`s:

- `User` / `Assistant` — text turns; assistant items may carry reasoning
  (chain-of-thought) that providers requiring replay can read back
- `ToolCall` / `ToolResult` — tool interactions, paired by call id
- `ProviderState` — opaque provider data preserved across turns

The item list is the source of truth for model context and is persisted
verbatim (see [storage](storage.md)).

## Native tools

CLAT ships project-scoped read tools:

- `list_files` — inspect repository structure with depth and entry limits
- `read_file` — read UTF-8 files by line range with byte limits
- `search` — search project text files and return path/line/text matches

and, for trusted projects, three side-effecting tools that close the
agentic loop (read → change → verify):

- `write_file` — create or overwrite files atomically (temp file +
  rename; a failed write never leaves partial content); existing file
  permissions are preserved
- `edit_file` — replace one exact, unique text snippet; ambiguous or
  missing matches error without touching the file; the commit
  re-verifies the read snapshot under a parent-directory lock, so
  cooperating CLAT writers conflict instead of overwriting each other
- `run_command` — run a shell command in the project root; termination
  covers the whole process tree (Unix process group TERM → unconditional
  KILL after a fixed grace; Windows Job Object), output past the 32 KiB
  cap is drained without changing command semantics; `exit_code` /
  `signal` / `timed_out` form the result contract

All paths are constrained to the current project root. Absolute paths and
`..` traversal are rejected. Reads reject paths that resolve outside the
project; writes perform parent creation, inspection, temporary-file creation,
and rename relative to opened capability directory handles, so a symlink or
concurrent directory replacement cannot retarget later I/O outside the root.
Common generated and dependency directories such as `.git`, `node_modules`,
`target`, `dist`, and `build` are skipped by default. Side-effecting
tools pass through the permission model on every call — see
[permissions](permissions.md) and the audit trail under
[docs/audit](audit/) for the adversarial review these guarantees went
through.

## Project trust

The trust gate is part of the startup state machine, not a UI overlay:

```text
BootstrapApplication::open
├── Bootstrap Catalog: storage backend + TrustStore
└── untrusted: no Session/Config/Tool/Provider service,
               no project reads, MCP, monitor, or model request
       │ trust_project + into_trusted
       ▼
TrustedProjectApplication
├── mounts Project Catalog once
├── exposes Session/Config/Provider/MCP DTO use cases
└── starts Run child scopes only through start_run
```

Trust is persisted per canonical directory path. The two Application types make
the boundary structural: pre-trust code has no API that can list sessions,
load credentials, access tools, start MCP, or run a model. The storage backend
is shared across the transition, but only the narrow `TrustStore` is registered
in Bootstrap Scope.

## MCP adapter

The built-in `McpAdapterPlugin` mounts only in Trusted Project Scope. A
`StdioSession` hosts each subprocess and `McpTool` contributes remote tools to
the shared Tool Registry through leases. Teardown first revokes those tools,
then explicitly closes stdin, bounds the child grace period, kills/reaps if
needed, and joins both I/O threads. See [MCP integration](mcp.md) for the
protocol posture, naming rules, and resource limits.

## TUI event loop

The TUI is a thin frontend. It owns terminal input, rendering, dialog state,
view-model mapping, an approver adapter, and channels that multiplex
`RunEvent`/`ApplicationEvent` into UI events. It never opens Storage, builds a
Model or `Run`, registers tools, launches MCP, persists completion state, or
owns provider business logic.

`TrustedProjectApplication::start_run` creates the run scope and core worker;
`RunHandle` provides cancel/join/finished. Application persists the user turn
before starting work and persists successful or partial assistant items before
publishing completion. It rejects a second concurrent run with `Busy` and
cancels/joins the active run before project teardown.

The provider quota monitor is a Project plugin. Its stop/wake channel, bounded
provider requests, five-minute refresh policy, and thread join belong to core;
the TUI only renders `ApplicationEvent::MonitorUpdated`.

## Adding a built-in extension

For an existing model protocol or runtime extension, implement the narrow
domain trait, wrap registration in a scope-correct `Plugin`, declare its stable
ID and service dependencies, and add it to the explicit catalog. Do not add a
central provider match, expose a raw registry to a frontend, or create another
assembly path. The test-only extension catalog demonstrates a typed service,
ordered Tool contribution, short-circuit middleware, post observer, and full
revocation without modifying TUI code.
