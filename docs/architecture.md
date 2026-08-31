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
              frontend-neutral use cases
                           │ typed ports
                           ▼
          TrustedProjectComposition (internal)
        catalog · resolve · freeze · wire · close
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
provider, MCP, plugin, composition, execution, and registry types remain
crate-private. Frontend implementation trees are not library APIs; crate-root
exports are limited to the Application facade and deliberately supported
domain contracts.

## Lifetimes and static composition

CLAT uses a Rust-native plugin kernel for compile-time components. Explicit
catalogs choose and order built-in plugins; there is no dynamic Rust library or
JavaScript discovery. Configured WebAssembly components are the only extension
code loaded into the CLAT process; MCP executables remain separate processes.
Dynamic native libraries are deliberately not a plugin ABI: Rust's compiler ABI
is not stable enough for a durable market contract. Rust-authored third-party
plugins compile to the versioned WIT/WASM component boundary instead.

Third-party packages of both runtime kinds are activated from one immutable
local store. The package tree is copied and fsynced before an atomic registry
pointer exposes it; update failure leaves the old pointer intact, and rollback
swaps two already-reviewed activations. Package mutation owns the same
storage-root lease as a Trusted Project Scope, so a running application and the
installer never race plugin state. User `plugins.json` / `mcp.json` entries are
late same-id overrides, not a second package database.

Remote discovery remains outside runtime activation. `plugin::market` verifies
the short-lived official Minisign index, publisher/key state, target and CLAT
compatibility, deterministic dependency solution, revocations and advisories.
`plugin::bundle` streams a bounded content-addressed `.clatpkg` into a fresh
tree. Only after every transitive package passes the existing manifest/tree/
publisher checks does `PackageStore::install_batch` replace the registry once.
The market never introduces a second activation pointer or an install-hook
execution path.

| Scope | Lifetime | Owns |
|---|---|---|
| Bootstrap | process open → trust decision | control-format classification and read-only trust lookup |
| Trusted Project | trust accepted → project close | storage lease, sessions, providers, tools, prompts, commands, Plan/Skills/LSP/Context services, MCP/WASM adapters, monitors, compaction, titles, todos |
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
Providers, commands, and the tool pipeline freeze during project composition;
tools and prompts freeze only after the first run's bounded MCP/DSH startup
wait, so asynchronous contributions are complete. Teardown can still revoke
existing leases.

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
- remote-control binding, pairing, delivery admission, and chat/session routing;
- lightweight frontend DTOs such as `WorkbenchSnapshot`.

DTOs contain display-ready facts, not subsystem handles. The workbench
snapshot deliberately excludes credentials and transcript replay; the server
combines it with its active-run ledger at the wire boundary.

`application/composition.rs` is the only owner of the Trusted Project
`PluginManager`. Its narrow `mount` transition builds the static catalog,
resolves the complete typed `ProjectPorts` set, applies the mount-time freeze
points, and wires project-owned notices. It also hides Run child creation and
reverse project teardown. The Application keeps trust, session selection,
admission, and active-worker policy; it cannot ask the manager for additional
services after mount.

`application/remote_control.rs` is the application owner for WeChat remote
control. It exposes closed outcomes for machine binding and poll checkpoints,
pairing and delivery admission, and chat/session start or steering. In
particular, the frontend cannot compose pending mapping intents, admission
owner scans, session switching, or mapping compensation itself.
`control_storage/im.rs` remains the sole owner of the 0600 atomic `im.json`
unit, while `im/ilink` owns the official wire and `serve/wechat` owns commands,
approval presentation, media download, the bounded outbox, and process-local
run claims. No transport or frontend lock enters the application port.

## Agent run lifecycle

The single-agent runtime is the daily-driver baseline. One trusted project can
have at most one active run.

```text
user prompt
  → reject active-run/compaction conflicts and validate the configured route
  → settle/freeze extension registries and freeze request-bound inputs
  → create Run Scope and spawn a waiting worker (no durable user fact yet)
  → prepare attachments and append+flush turn/start + user/message
  → assemble the Run Context and activate the waiting worker exactly once
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

`application/run_execution.rs` owns this two-phase worker boundary. Its
crate-private `WaitingRunExecution` can only be aborted before admission or
activated with committed input. The execution worker owns the mounted Run
Scope handle returned by project composition, plus the start channel,
process/subagent/todo bindings, terminal merge, plugin-host slots, completion
signal, and join handle, so callers do not reproduce cleanup order.
`TrustedProjectApplication` still owns active-run/compaction exclusion and the
admission commit point; execution never decides whether a user fact may be
made durable.

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

Immediately before each agent request, core derives a detached image projection
from that surface. It counts user, assistant, and typed tool-result images in
provider order, enforces the independent 12-block/20,000,000-byte request
bounds, and compares the full estimated input plus output reserve with a
1024-token-quantized 80% pressure line. If necessary, older images become one
fixed path-free notice in oldest-first order. Images at and after the latest
user turn are protected; if that turn still cannot fit, the run fails before
provider I/O. The journal and transcript retain the original image blocks.
`/context`, spend reservation, compaction, and the actual `ModelRequest` share
this estimator/projection rather than maintaining frontend-specific counts.

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
overridden by user-provided extra body data. Attachment admission, route image
policy, and final JSON body bytes are independent gates; both adapters stream
their final JSON into one buffer capped at 32 MiB before HTTP send. See
[Providers](providers.md) for preset and retry details.

Each durable `request/header` freezes the model route, complete image policy,
and estimator, calibration, and normalized-encoder versions. A route or version
change therefore produces a new header only at a run boundary. The normalized
content-addressed blob is the first-release provider variant: CLAT performs no
route-specific silent resize, so a second disk variant cache would only
duplicate identical bytes. If route-specific encoding is introduced later,
that header identity becomes the required cache key prefix.

## Native tools and permission pipeline

Built-in read tools are `list_files`, `read_file`, and `search`. Search is an
independent internal plugin: literal remains the default, while regex,
include/exclude globs, extensions, gitignore/hidden policy, stable ordering and
snapshot-bound pagination remain bounded inside the Rust binary. Read tools
enforce byte, depth, entry, match and result limits. Project-relative paths
reject traversal and use symlink-aware capability discipline. Explicit
absolute reads remain allowed by CLAT's current permission contract.

Trusted projects also receive:

- `view_image` — a per-run, visual-capability-gated read tool for reachable
  session attachment IDs, project-relative no-follow reads, and core-minted
  run scratch references. It is absent from text-model and read-only child
  catalogs; Full Access never widens it to arbitrary absolute paths. Live
  provider requests use transient fenced paths, while wire and journal
  surfaces carry descriptor-only content blocks. Its invoke-to-result
  authority cache is bounded to one model step and consumes an entry for each
  transformed result, so a long tool loop cannot retain historic references;
- `write_file` — bounded atomic replacement with permission preservation;
- `edit_file` — one exact unique replacement plus conflict revalidation;
- `apply_patch` — one existing UTF-8 file, multiple exact hunks, complete
  in-memory validation followed by one snapshot-checked atomic commit;
- `exec_command` + `write_stdin` — run-owned command sessions with incremental
  stdout/stderr or PTY output, stdin/poll/terminate, bounded cursors and TTL;
- `run_command` — a one-shot compatibility wrapper over the same
  `ProcessService`, fixed to the project root.

`ProcessService` is the only agent-command spawn seam. One Trusted Project
owns the service; each active Run binds a fresh generation and session id.
Process ids never cross that boundary, at most eight jobs run concurrently,
and run cancellation/terminal, TTL, explicit termination and Application
close all terminate the owned process group and ordinary descendants. Each
stdout, stderr and PTY stream has a 256 KiB transient ring; tool results are
separately bounded. Raw streams and stdin have no persistence path. `write_stdin`
redacts its characters from the durable `tool/call` while permission review
and invocation still see the complete arguments.

The lifecycle claim is limited to code paths where CLAT can execute teardown
and to descendants that remain in the owned group. A daemon that deliberately
creates a new session/process group can escape this local boundary. Fatal
native crashes, `SIGKILL`, power loss and equivalent failures also require an
external supervisor or container; macOS has no parent-death primitive that
would make those paths an in-process guarantee.

`SandboxService` plans every command before spawn. On macOS, Read Only,
Project Write and classic headless execution use a functionally probed
Seatbelt profile with a policy digest; its file-write and network rules are
enforced by the OS. Account-readable files, inherited environment variables
and the writable `/tmp`/process-temporary roots remain available; this is not
a container boundary. Full Access is deliberately unconfined. Linux and
Windows currently report an explicit supervised/no-enforcement fallback for
`auto`, and reject `required`; they are not described as sandboxed. Execute
approval remains in front of this OS boundary on every platform.

`ProjectInstructionsPlugin` supplies cached scope-aware instructions rather
than a one-time root prompt. It starts at the project root, observes only
approved successful file tools through a post-tool observer, and adds nested
`AGENTS.md`/`CLAUDE.md` scopes before the next model request. The complete
system snapshot and source path/scope/digest metadata travel in
`request/header`; resume restores the active scopes from that durable fact.

### Workflow and intelligence plugins

Agent phase 3 is four removable Trusted Project catalog entries rather than one
new runtime layer:

- `builtin.plan_mode` owns durable plan state, `/plan`, `exit_plan_mode`, and the
  per-run `ToolAccessPolicy`;
- `builtin.skills` scans bundled/user/project `SKILL.md` layers and installs one
  run-bound frozen catalog plus the read-only `skill` loader;
- `builtin.language_intelligence` reads user-level `lsp.json`, exposes one
  `ExternalRead` `lsp` tool, and borrows project-owned managed stdio from
  `ProcessService`; it does not add a second spawn seam;
- `builtin.context_inspector` registers `/context` only. The Application derives
  its snapshot from the same prompt, project-instruction, plan, skill, tool and
  model-history readers used by the next request.

At each run start one `RunContextSnapshot` freezes Plan tool access and the skill
catalog. The same objects drive model-visible schemas, workflow instructions,
`request/header`, forged-call admission, permission/plugin-host gating, and
budget estimation. `/context` is outside a Run and takes a fresh read-only
snapshot; its component estimates are incremental calls to the same
`model::estimate_request_tokens` function, with output reserve added separately.
The snapshot's Plan → Skills → Memory → Goal instruction layers, tool view, and
durable header assembly have one owner in `application/run_context.rs`;
`/context` consumes those layers instead of reconstructing workflow policy.

Agent phase 4 adds three more removable Trusted Project entries:

- `builtin.memory` owns the versioned local memory store, `/memory`, bounded
  run injection, and the read-only `memory_search` tool. Human control-plane
  calls are its only writers;
- `builtin.goal` owns the one-goal-per-session state machine, `goal/change`
  projection, `/goal`, verifier-gated `update_goal`, and the explicitly armed
  bounded continuation driver;
- `builtin.subagent` owns the default-off `/subagents` experiment and
  `delegate_readonly`. A child is a scoped `Run` over a fixed three-tool
  project-confined registry view, independent history/model instance, child
  cancellation token, hard budgets, joined worker, and parent-journal
  provenance. Child reservation/usage is charged to the same per-round spend
  ledger as the parent model, so ordinary run and Goal caps cover both.

The shipped HTTP provider adapters turn child deadlines into request deadlines
and consume the cancellation token. `ModelProvider` cancellation remains a
cooperative contract for in-process adapters; the runtime joins children and
rejects new work while closing, but it does not pretend it can safely kill an
arbitrary third-party provider implementation inside the process.

Goal continuation remains one core run worker: every round closes its durable
turn before the next user/goal message is appended. Subagent workers belong to
one tool invocation and are joined before it returns; Application close first
marks the service closing, cancels registered children, and waits with a bound.
Neither frontend owns state transitions, model factories, journals, budgets,
or worker threads. TUI and local web only supply their normal event/approval
ports; headless control commands cannot silently invent an interactive goal
continuation.

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

Manual PWA compaction is exposed as `session.compact`. Serve owns only a clone
of the core `CompactHandle` so it can report an `active_compaction` snapshot and
route cancellation; summary generation, journal replacement and replay folding
remain Application/session responsibilities. `CompactionUpdated` notices clear
the frontend slot and drive the browser projection, including recovery after a
reload. A run and manual compaction remain mutually exclusive at the core
boundary.

The embedded `web/` workbench is a projection client. Static assets and pairing
shell contain no credential. Authenticated API calls carry the persistent
`~/.clat/web-token` as Bearer; no URL or Cookie token path exists. Browser
storage contains only UI preferences and the origin-scoped paired token, never
conversation facts.

Image drafts remain frontend presentation state until the server admits them:
the workbench sends one bounded PNG/JPEG stream into a server-minted, selection-
bound draft scope, then references only opaque upload IDs in `prompt.send` or
`steer.send`. Core imports a queued steering image before it can be claimed,
but its descriptor is durable only at the recorder's append-and-flush point.
History metadata travels through replay/SSE; image bytes are loaded later from
an authenticated active-session reachability endpoint into revocable browser
blob URLs. Thus neither a host path nor a bearer token becomes image authority.

The read-only Plugin Index panel is a special public-data projection: it fetches
`https://pi.at.cn/catalog.json` with credentials omitted and never sends the
local token cross-origin. It has no install RPC. Model trace protocol ids are
mapped to human-readable presentation labels while the original `RunEvent`
vocabulary remains unchanged and available as diagnostic metadata.

### DSH client

`dsh/` implements the HTTP/WebSocket client and host lifecycle for `clat dsh`.
The TUI reducer maps DSH events into the same presentation state, but commands
and session facts stay on the DSH host. Local access to `~/.dsh/storages` is
read-only and used only to populate session selection. The sole client-local
write is the bounded, fail-soft `~/.clat/dsh-last-session` presentation
preference; it is owned by `dsh/last_session.rs`, not by control storage.

## Source map

| Path | Responsibility |
|---|---|
| `src/application.rs`, `src/application/` | client-neutral use-case facade, DTOs, run/session lifecycle |
| `src/application/composition.rs` | Trusted Project catalog, typed port resolution, freeze/wiring, Run children, and teardown |
| `src/application/run_context.rs` | request-bound workflow composition, tool view, and durable request-header assembly |
| `src/application/run_execution.rs` | two-phase run worker activation, round execution, terminal merge, and run-resource cleanup |
| `src/application/remote_control.rs` | remote binding/pairing/delivery use cases and durable chat-to-session recovery orchestration |
| `src/run.rs` | agent loop |
| `src/model.rs`, `src/providers/` | provider-neutral model contract and adapters |
| `src/tool.rs`, `src/native_tools.rs`, `src/apply_patch.rs`, `src/search.rs` | tool contract and native coding tools |
| `src/process.rs`, `src/plugins/process.rs`, `src/sandbox.rs` | run-owned process sessions, exec tools and platform sandbox policy |
| `src/project_instructions.rs`, `src/plugins/instructions.rs` | scoped project-instruction discovery, caching and observation |
| `src/permission.rs` | effects, modes, policies, approver port, write scope |
| `src/event.rs` | RunEvent vocabulary and EventSink |
| `src/interaction.rs`, `src/media.rs` | user-question port and image preparation |
| `src/draft.rs` | process-local, core-owned pre-admission image staging |
| `src/plugin/`, `src/plugins/` | plugin kernel and built-in catalogs/adapters |
| `src/plugin_host.rs` | extension sampling and elicitation semantics |
| `src/mcp.rs`, `src/mcp/` | MCP config, transport, client, and tool mapping |
| `src/session/` | journals, projections, recovery, checkpoints |
| `src/control_storage/` | JSON control plane, atomic IM state, and workspace registry |
| `src/private_fs.rs` | 0600, symlink-safe atomic publication and platform fsync for CLAT-owned private files |
| `src/command.rs` | frontend-neutral slash-command contract |
| `src/tui.rs`, `src/tui/` | terminal frontend and structured image-draft presentation |
| `src/exec.rs` | headless frontend |
| `src/serve.rs`, `src/serve/`, `web/` | local API, SSE, and embedded PWA |
| `src/dsh/` | crate-private DSH client protocol, host lifecycle, and client-local last-session preference |
| `src/demo.rs` | deterministic offline composition |
| `src/upgrade.rs` | authenticated self-update |
| `wit/`, `sdk/clat-plugin/` | WASM contract and author SDK |
| `sdk/dsh-adapter/` | static Cordis/DSH compatibility adapter package |

Image-bearing TUI starts use an ownership handoff rather than decoding on the
terminal thread: a bounded frontend worker temporarily owns
`TrustedProjectApplication`, performs core admission, and returns it in a
`RunStartFinished` message. A one-shot event barrier blocks the core run at its
first event until the TUI has restored the application, installed the run
handle and usage baselines, and cleared the accepted draft. Pre-commit failure
returns the same application and leaves the ordered draft untouched. This is
frontend scheduling only; validation, normalization, persistence, spawning,
and the commit point remain core-owned.

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

The architecture test suite automatically classifies TUI, DSH, exec, and serve
as local clients, enforces the core-to-frontend dependency direction, and
rejects direct frontend references to storage/session/composition/context/
execution owners. Behavioral tests must also exercise lifecycle sequences—
open, run, switch, close, reopen—because static boundaries alone cannot prove
state correctness.
