# Architecture

## Core and static plugin composition

CLAT has a UI-independent Application facade over a small, Rust-native static
plugin kernel. Plugin implementations are compiled into the single `clat`
binary; explicit catalogs select and order them at runtime. CLAT does not load
Rust dynamic libraries, WASM, JavaScript, or automatically discovered code.

```text
TUI / demo / exec (headless CLI) / future desktop or IDE client
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
| Bootstrap | Application open → trust decision | no plugin scope at all — a zero-write control-plane preflight (`sentinel` classification, read-only trust lookup) |
| Trusted Project | trust accepted → project close | control-plane `ConfigStore`, DSH session persistence (`SessionService`), Tool/Provider/Prompt registries, native tools, MCP adapter, permission and Agent services, monitor, project instructions, tool-result pruning, compaction, per-session todo, session-title services |
| Run | one active run | `CancelToken` and injected `PermissionApprover`; worker ownership stays in Application |

The batch-1 capability plugins (`ProjectInstructionsPlugin`,
`ToolResultPrunerPlugin`, `CompactionPlugin`, `TodoPlugin`,
`SessionTitlePlugin`) are optional: a minimal catalog without them keeps
every existing test green. `ToolResultTransformer` is a narrow post-result
seam with exactly one real consumer (the pruner) — it is not a general
hook, and the tool pipeline remains the only extension point that invokes
it. Compaction and title generation run as background work with enforced
total deadlines and attempt caps; application close cancels and joins
their workers, so no scope teardown waits unboundedly.

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

## Frontends

Three frontends exist today, all talking to the same Application facade:

- **TUI** (`tui*.rs`) — full-screen terminal client; owns raw-mode handling
  and dialog rendering only.
- **demo** (`demo.rs`) — the deterministic model → tool → model loop used by
  `clat demo` and as a living proof that core runs without a UI.
- **exec** (`exec.rs`) — the headless one-shot CLI behind `clat exec`. It
  supplies an `EventSink` that streams assistant text to stdout and status
  to stderr, a permission approver (terminal prompt via a request-scoped
  input port, deny-on-pipe by default, `--yes` to allow all), and a
  completion channel. Interrupt routing (`ExecCancel`) is injected by the
  process boundary in `main.rs`, so the library entry is repeatable
  in-process and never installs global signal handlers. A stdout write
  failure (broken pipe) cancels the active run instead of pretending
  success; stdin is read with an explicit byte budget and combined with
  the positional instruction into one prompt.

Every frontend is presentation and input handling only; run lifecycle,
persistence, and permission semantics stay in core. A future desktop or
IDE client reuses the facade the same way, supplying its own approver and
event presentation.

`Run` still owns the streaming model → tool → model algorithm, capped at a
configurable number of turns. Every observable step (`ModelRequested`,
`ModelStream`, `ToolRequested`, `ToolFinished`, `PermissionChecked`,
`RunCompleted`, …) is emitted through `EventSink`. Application-level facts such
as quota refreshes use `ApplicationEvent`; plugin hooks never enter the
`RunEvent` protocol.

Protocol notes (session-persistence cutover): `ModelResponded` carries an
optional `provider_replay` payload (lossless provider state such as OpenAI
Responses reasoning items) so the journal can persist it as
`source.replayState`, and `ModelStream` can carry `RetryScheduled` /
`RetryStarted` meta-events from the retry wrapper (they journal as
`llm/retry` / `llm/retry-started`). Frontends may ignore both.
`PermissionDecision::Unavailable` marks fail-closed decisions from
approvers that could not ask anyone; run semantics match `Deny`, the
journal records the DSH outcome `unavailable`.

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
| Project instruction injection (`AGENTS.md`, then `CLAUDE.md`) | done — capability-bound read in Trusted Project Scope |
| Context management | done — tool-result pruning, append-only compaction markers, manual `/compact`, optional automatic budget |
| Per-session agent todo state | done — `SessionWrite`, append-only snapshots, dynamic model context |
| Automatic session titles | done — first successful run, bounded background worker, CAS against manual rename |
| Typed provider retry | done — fresh model attempts, Retry-After, event-safe retry, internal deadlines |
| Unbounded agent loop (DSH parity) | done — the run loop has **no turn budget** (2026-08-19; DSH's `kick()` is `while (await turn())`, and Claude Code / opencode are the same): it ends only on completion/refusal, user abort, or failure. Context pressure belongs to pruning/compaction, cost to the visible usage + user cancel. The earlier fixed-32-turn interruption and its bounded `[auto-continue]` patch were emergency measures and are removed |
| Headless CLI (`clat exec`) | done — one-shot runs with stdout-only assistant output; dual-input prompt (instruction + piped context, 8 MiB budget); TTY permission prompts with stale-input discard, deny-on-pipe default, `--yes` bypass; `--continue` / `--session` resume; graceful Ctrl-C everywhere (including pending-run and permission-wait windows); broken-pipe cancels the run and fails the exit; closed by the 2026-08-17 headless audit (HL-01…09) |
| Subagents, image input, multi-agent orchestration | deferred by constitution |

The next growth step is driven by real dogfood need; candidates include a
structured `--json` event stream for `clat exec` (consumed by editors and CI).
The Application API remains frontend-neutral:
every frontend — TUI, `exec`, a future desktop or IDE client — supplies only
an `EventSink`, a `PermissionApprover`, and a completion channel.

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
BootstrapApplication::open            (zero-write preflight; no plugin scope)
├── classifies config.json + clat.db against the sentinel matrix
├── read-only trust lookup
└── untrusted: no Session/Config/Tool/Provider service,
               no project reads, MCP, monitor, or model request
       │ authorize_and_mount(ProjectAuthorization)   ── the only trust write
       │   storage-root lease → session-root preflight → control commit
       ▼
TrustedProjectApplication
├── mounts the Trusted Project catalog once (holds the root lease)
└── starts Run child scopes only through start_run
```

Trust is persisted per canonical directory path. The two Application types make
the boundary structural: pre-trust code has no API that can list sessions,
load credentials, access tools, start MCP, or run a model — and no API that
can write the control plane before the session-root preflight passed.

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
