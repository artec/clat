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
7. **The core CLI remains one binary.** Rust implementations and
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

Pending Web/Desktop input is owned by the core input module, exposed through
Application ports. Its versioned FIFO retains typed messages and attachment
reservations until the recorder confirms durable admission. The runtime receives
one pending item per model-request boundary; existing terminal steering and the
after-run queue remain compatibility paths. A single core interruption intent
reserves a pending item and cancels the active worker. Host completion dispatch
continues only after the old worker has closed its scope and joined; Esc can
revoke that continuation while cancellation is still in progress. Frontends never
orchestrate a cancel RPC followed by a send RPC. Snapshot/reference provenance is
recorded against the actual committed user message; no new journal event or
rewritten model content is introduced for queue management.

The product itself ships two compilation units: `clat` (public facade,
CLI and TUI/DSH terminal client) depends on `clat-core` (runtime,
Application and terminal-independent headless/HTTP surfaces); the repo
workspace additionally contains non-default members: development/example
plugins and the plugin SDK, plus `sdk/wit-proposals/net-runtime`
(`clat-wasm-net`), which is a **production dependency of clat-core**, not a
development-only component. The core
manifest has no dependency on `clat`, ratatui, crossterm or arboard;
the dependency direction is enforced by Cargo, not only source inspection.
Both packages are default workspace members, so ordinary delivery commands
still cover both. Only `clat` has a binary target. Source remains under
`src/`; `crates/core/Cargo.toml` explicitly selects `src/core.rs`.

`client_ports` exports only existing client DTOs, replay, path encoding and
controlled staging/private-file operations. It does not export session,
storage or composition owners. The explicit `test-support` feature shares
scripted providers and fixture readers with frontend tests; release builds
do not enable it. This internal workspace surface is not a stable plugin ABI.

## Lifetimes and static composition

Native Web access is owned by `web_tools::WebService`. Tools prepare a
configuration snapshot before ordinary Network permission review, then use
the existing HTTP/DNS authority for public-address validation, connection pins
and bounded decoding. Credentials remain in control storage; model selection
does not select the Web backend. Frontends use the same secret-write-only
configuration ports. See [Native Web tools](web.md).

CLAT uses a Rust-native plugin kernel for compile-time components. Explicit
catalogs choose and order built-in plugins; there is no dynamic Rust library or
JavaScript discovery. Configured WebAssembly components are the only extension
code loaded into the CLAT process; MCP executables remain separate processes.
Dynamic native libraries are deliberately not a plugin ABI: Rust's compiler ABI
is not stable enough for a durable market contract. Rust-authored third-party
plugins compile to the versioned WIT/WASM component boundary instead.

Static plugin observability reads `PluginManager`'s successful mounts directly.
The manager retains each mounted descriptor until removal or rollback;
`plugins::inventory` combines it with owner-attributed tool and command
registrations. Composition returns typed snapshots and Host aggregates their
project instances into the additive `plugin.list.builtins` projection. Display
metadata is separate from mount membership and never grants mutation rights.
Frontends render this through the same client ports as external packages.

The development workspace and transient draft-image stores are project plugins
(`development::plugin`, `draft::plugin`), resolved through typed services rather
than constructed by the Application facade. Plugin cleanup closes the services
even while clients hold clones. Draft creation and close share a serialization
lane; scope creation and upload publication check the closed state under the
draft-state lock. Application still owns run shutdown and flushing pending-input
references before project plugin teardown, preserving the admission transaction.

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
| Trusted Project | trust accepted → project close | storage lease, sessions, providers, tools, prompts, commands, Plan/Skills/LSP/Context services, development workspace, transient drafts, MCP/WASM adapters, monitors, compaction, titles, todos |
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

The host plugin control plane pauses external-plugin mutation until every
mounted project is idle and MCP startup is settled. Under the ordered project
locks it revokes MCP/WASM adapters, atomically publishes the package registry,
reopens tool/prompt registration and remounts only those adapters. Core services,
sessions and drafts retain their owners. The next run freezes the new tool
surface; management replies remain outside the durable run vocabulary.

## Application boundary

`HostApplication` groups already trusted project runtimes under one storage-root
owner. That owner retains the kernel lease and exactly one `ControlStorage`
cache; each canonical project path has one `TrustedProjectApplication`, with
its own session service, permission state, draft store and plugin composition.
Dropping a client handle does not remove a hosted project. Explicit host close
requires client handles to drain before project producers and journals close.
Fresh-root initialization extends the ancestor lease to the newly created root
before exposing its writable control plane.
On Windows, the named mutex hashes a case-folded canonical ancestor plus the
missing suffix in one trailing-separator directory form. Its name remains
stable when the root is created, and retains compatibility with existing-root
leases held by older processes. Identity discovery never creates storage.

Serve routes additional projects through `/workspace/<id>/api/...`. The default
project retains `/api/...` compatibility. The 16-project bound applies to
simultaneously mounted domains, not historical visits. Capacity pressure
reclaims idle non-default domains after checking transport ownership, runs,
compaction, arbitration, drafts and remote bindings. Atomic `Arc` extraction
fences concurrent `Weak` upgrades; reclamation drains producers and removes
both the route and core project owner without deleting history or releasing
the host's root lease. Routes are process-local identities;
an expired route returns an error and never selects another project.

`HostApplication::workspace_directory` merges ordered registration metadata with
lightweight mounted-project facts. It does not register, mount, authorize, probe
project paths, load transcripts, or read model credentials. Mounted identities
reuse the canonical path captured by the Application at mount time; transport
adds only the current route id and API prefix. Stable registration ids remain
separate from transient route ids, and untouched fresh projects have no stable
id yet. Missing registered projects remain in the directory. For an unmounted
entry, the active-session field is its saved pointer, not a newly opened session.

Explicit `clat attach` uses the core's authenticated loopback HTTP/SSE client without
mounting a second application writer. The default TUI uses spawn-or-attach;
`--standalone` explicitly selects a process-local application. Startup treats
an authenticated, root/instance-matched incompatible description as terminal,
distinct from an unavailable or stale endpoint hint. Before a host accepts
clients it caches a build fingerprint derived from version, target, executable
length and modification time, with only the hash crossing the wire; paths,
inode/device identities, case and Windows short names never enter the value.
The description also carries the plain product version as an additive field.
When fingerprints differ but protocol/root/instance still match, a strictly
newer native client may request an instance-fenced takeover. The host accepts
only a semantically newer replacement and only when every mounted project has
no active run, compaction, approval/question wait, pending steering, remote
binding, upload/download or live draft. It marks all routes shutting down before
acknowledging, so later mutations cannot enter the yielded process. Busy hosts,
same-version alternate builds and older clients remain attached/refused under
the existing explicit-stop policy rather than being replaced automatically.
The startup client waits for the storage-root lease and fixed port to be released
before spawning the replacement. The explicit status/stop control path ignores
only build difference so the old host remains safely manageable; protocol and
root fences still apply. Missing version/fingerprint fields retain additive
rollout compatibility, and raw HTTP/PWA clients have no executable identity to
compare. Host lifetime remains
independent of attached frontends. The native title marker consumes the existing
connection projection (`CLAT ● host` online, `CLAT ○ host` offline); standalone
and DSH keep their existing identities.

Manual suggestion admission freezes session identity, selection generation,
context and budget under the project's short mutation lock. Provider I/O runs
outside that lock, so another client can send, cancel or switch sessions.
Terminals retain composer ownership during generation and publish replies only
as request- and input-generation-fenced previews; adoption is explicit and
never submits a run.

Attached read-only information dialogs consume existing `command.run`
projections; `/help` carries the structured host command catalog so the TUI can
append its frontend-local Composer and Keys sections, while the other dialogs
retain their text projections. Scroll state and request identity remain
frontend-local.
The core `HostClient` supplies canonical read-only command surface hints;
the terminal does not own the command-name dispatch table.
Responses are fenced by connection epoch, selection generation, request number,
and an open dialog. No new persistence owner or wire method is introduced.

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

- `list_sessions`, `session_history`, `new_session`, `switch_session`, and
  `rename_session`;
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
cancellation, failure, or the per-run token spend guard — which is armed only
by an explicit positive `run_token_budget` set by the user; an unset budget
means no CLAT-local cumulative cap (BUDGET-1 ruling, 2026-10-15: the former
silent 10M default is removed; `Some(0)` is an explicit off). Accounting still
runs without a cap. Tool failures and
permission denials become structured `ToolResult` errors so the model can
adapt without aborting the whole run.

`application/run_admission.rs` owns the shared durable transaction for selected
human/Goal runs and targeted Job/Schedule runs. It constructs the message identity,
opaque submission digest, birth/turn/user batch, replay cursor and committed
receipt together. An admitted batch carries its witness through fallible
application projections and history reconstruction; only then can it produce
the worker's `PreparedRun`. Its private construction seal prevents other
application modules from manufacturing uncommitted worker input. Selected-session
birth and registration policy stays inside `run_admission/selected.rs`; targeted
admission does not mutate the foreground selection.

`run_context/start.rs` owns the context transition around that commit. It captures
project instructions, Skills and Memory before admission, then freezes workflow
facts, tool visibility and the request header after Fresh birth events are durable.
Selected runs, targeted runs and `/context` use one surface-plus-runtime-Todo
history composer in `application/context.rs`; the Todo item remains non-durable.

`application/run_execution.rs` owns the two-phase worker boundary. Its
crate-private `WaitingRunExecution::start` accepts an admission closure and owns
abort/join on admission failure and activation/slot cleanup on channel failure.
Both callers receive committed, non-retryable receipts with the failure phase
when startup fails after admission; neither reconstructs that policy.
The execution worker owns the mounted Run
Scope handle returned by project composition, plus the start channel,
process/subagent/todo bindings, terminal merge, plugin-host slots, completion
signal, and join handle, so callers do not reproduce cleanup order.
`TrustedProjectApplication` still owns active-run/compaction exclusion and the
admission commit point; execution never decides whether a user fact may be
made durable.

Manual compaction has one lifecycle owner in `application/compaction/manual.rs`.
Application owns single-flight admission, the start timestamp, cancellation and
worker result. TUI, headless and RPC callers observe the same operation; serve
keeps no second compaction slot. A completion notice is presentation only and
cannot clear a newer operation. Duplicate starts are rejected before any wait;
requesting cancellation does not release the slot until summary work exits.
A panicking summary service becomes a localized failure result and releases the
slot. Starting/completing an operation does not depend on a client consuming
notices. Concurrent handle joins wait for the same worker settlement.

Manual and automatic title updates live in `application/title.rs`. A targeted
rename validates the selected session and performs its durable write within one
Application mutable borrow, so an intent for an earlier session cannot rename a
later selection. Session RPC adaptation is grouped in `serve/protocol/sessions.rs`;
its identity/title/watermark reads share one Application lock. WeChat RPC
adaptation lives in `serve/protocol/wechat.rs`; root routing and existing
binding/outbound locks are retained by the transport boundary.

Internally, `run_execution/worker.rs` owns mutable worker progress and Goal
continuation; `run_execution/round.rs` owns a round's bindings, shared spend
ledger, invocation, process cleanup and durable settlement. Cleanup is explicit:
its errors participate in the existing terminal precedence before completion is
reported. The activation boundary remains in `run_execution.rs` and admission
remains in Application.

The agent loop delegates request preparation, model response handling and one
serial tool call to `run/request.rs`, `run/response.rs` and `run/tools.rs`.
`run/state.rs` keeps history, turn count and usage together when constructing a
terminal result. These are internal boundaries: tool order, steering seals,
permission checks, result transforms and event publication retain their order.
`run/context.rs` defines an internal request-boundary maintenance port: the loop
passes its live items, usage, actual instructions, frozen tool definitions, and
model options before image projection and spend reservation. The round owner
injects the Application implementation; the loop does not own session I/O or
summary models.

Steering is accepted at model-request boundaries. The queue is sealed
atomically before a terminal event becomes visible, so a late message either
joins the active run or becomes a new submission; it is never stranded.

Next-turn followups are a separate Application-owned, process-local FIFO of
bounded text items. `queue.list`, `queue.enqueue` and idempotent `queue.recall`
expose it through the RPC method catalog; `workbench.info` includes
`next_turn_queue`. It is neither steering nor a durable inbox: enqueue writes
no journal event, and dispatch calls ordinary `start_run`, preserving human
prompt/goal-disarm semantics and the existing `turn/start` + `user/message`
admission contract. A successful terminal boundary dispatches one item; success
continues draining, while cancellation/failure retains the remainder. Items
leave the queue only after successful admission or an authoritative committed
receipt. Session switches refuse a non-empty queue, and host takeover/reclaim
considers it busy. Standalone TUI and serve dispatch through the same core API;
attached TUI and PWA only own rendering, key/button actions and draft recovery.
No durable event or `RunEvent` vocabulary is added.

Cancellation is cooperative but propagated across the full call path: model
SSE reads, native tools, command process trees, MCP waits, WASM execution, and
plugin-host model/question waits observe the shared token. Components that may
block outside cooperative cancellation have bounded join policies.

## Event contracts

The journal catalog owns a static seat for each supported event type:
payload validation, surface membership, version retirement and replay action
are declared together. Envelope-only metadata is explicitly preserved without
interpreting its payload. Adding such a DSH type no longer requires a separate
replay skip-list edit. Stateful projections still own fold/checkpoint/restore;
the recorder's RunEvent-to-journal lifecycle and the v1 wire encoder remain
separate contracts because these vocabularies are not one-to-one.

The V4 catalog also admits DSH 0.2.1-rc.2 cwd/job records through payload
validators and `Skip` replay seats. They remain preserved facts without a
CLAT working-directory or job-registry consumer. Adding these reader seats
does not add a recorder producer, RunEvent variant, or model-facing message.

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
- the bounded **message outline** supplies `{seq, turn, role, preview}` rows
  for navigation without forcing a browser to hold the full transcript.

Tool-result pruning reduces transient model context. Compaction appends a
summary marker and shadows older surface ranges without deleting original
events. Preset context windows seed automatic compaction; manual configuration
remains authoritative.

`application/compaction/active.rs` maintains context inside a single running
model/tool loop. At pressure it closes and flushes a completed recorder step
without ending the user turn, reads the committed surface, invokes the shared
compactor, and admits the existing atomic compaction event family. Only then
does it replace the corresponding live interval. No recorder/session lock is
held during model I/O or subscriber callbacks. A failed durable write prevents
another model request; no completed tool is replayed.

`plugins/conversation/compaction/selection.rs` selects complete exchange spans,
protecting genuine user nodes and the newest complete exchange. Selection has
no aggregate token allowance. The private `summary::SummaryJob` owns the derived
internal text profile, per-request capacity, chunk preparation, map/reduce,
result validation and recovery for manual, startup and in-run compaction.
Callers supply history and its protection semantics; they do not assemble
summary requests. `chunks.rs` splits rendered text at UTF-8 boundaries, using the
model's configured input capacity after reserving instructions and generation.
History that fits is summarized in one request. Longer histories use as many map
and reduction requests as needed. Unknown model capacity uses a conservative
24,000-token per-request size; it does not cap history size.
Accepted summary sizes scale down for small windows so two summaries and their
headings fit the next reduction request.

The visible summary bound is separate from generation tokens, which also leave
room for reasoning. The prompt uses a character limit compatible with the
conservative token estimate. Only a completed, nonempty answer can become a
replacement. An empty or length-limited answer gets at most one recovery attempt;
a complete oversized answer can instead be summarized again in full, without
truncating it. Repeated invalid answers stop that request. Transport retries
apply per request rather than sharing a job-wide request cap. Compaction adds
no per-request or whole-job deadline, cumulative token budget, or request-count
limit. It observes the caller's cancellation signal. Provider cancellation is
reported separately. The primary model configuration is unchanged.
`accounting.rs` records every actual attempt, including failures, in the shared
run spend ledger without using the ledger's cap to stop compaction. Ordinary
agent requests still enforce an explicitly configured run cap after maintenance.
In-run maintenance continues while each checkpoint strictly shrinks the request,
until it fits or no safe reduction remains; it has no fixed pass limit.
Unapplied attempts record usage on an existing log-only compaction end event;
successful replacements record it once on the summary event. The existing
Stats/Compaction/Surface folds and checkpoints consume both paths; there is no
new durable event vocabulary, session generation, or projection version.

`application/compaction/alignment.rs` maps durable spans to the live history
using call/result identities. Replay groups calls in assistant nodes while the
serial loop interleaves calls and results; comparison tolerates that ordering
without treating raw item offsets as durable coordinates. Every user is
protected except checkpoints created and identified by this manager. Durable
source seqs are deduplicated because an assistant event projects several model
items. Unshadowed live JSON, image parts, and provider state are copied intact,
instead of replacing rich live results with their redacted journal projection.
The context and image projection share the quantized request pressure line.
Manual/startup compaction retains the older user-turn selection policy.

`read_file` results are constructed to stay under the shared pruner threshold,
so the generic head/tail replacement never fires on them: every segment keeps
its cursor and status fields intact in the model's view (READ-1, 2026-10-15).
Long files arrive as ordered bounded segments with `has_more`, an opaque
`next_cursor` (absolute byte offset + line number + in-line flag), and a
`file_version` identity (`size` + `mtime`); passing `expected_file_version`
back fails explicitly when the file changed between segments instead of
silently stitching two versions. Lines longer than the segment budget are
split at character boundaries and marked `partial_line` — byte-cursor
continuation always progresses, so no input loops forever on the same
truncated piece. `max_bytes` stays an upper bound; the applied context-safe
budget is reported as `content_budget_chars`.

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

Run context assembly advances full-system and workflow-only views through one
ordered sequence. The context owner also refreshes Goal instructions and their
request header together at each continuation boundary; the worker supplies the
new Goal observation without editing prompt layers or header fields itself.
Plan, skills, memory and tool visibility retain their existing freeze rules.

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

Command preparation and current-run lookup preserve internal error identity
through frozen execution. Recovery adapters use that identity, never parse a
human-readable error message; an unrelated error with the same text does not
acquire recovery authority. Command model views, bounded diagnostic retention,
passive verification guidance and journal projections share one policy owner in
`plugins/process/output.rs`. The existing generic result transformer remains the
only downstream pruning pipeline.

Native read/edit argument failures retain their original error plus bounded,
value-free `argument_recovery` diagnostics. Read-range validation happens
before file I/O; edit structural validation collects at most eight issues
before the authorized snapshot read. The existing registry permission gate,
ToolResultPruner, next model request and generic tool-result journal carry the
same metadata; there is no retry scheduler, alternate write path, new durable
event type, or interpretation of supplied text as recovery instructions.
Existing `edit_recovery` remains the separate post-read exact-match locator.


Built-in read tools are `list_files`, `read_file`, and `search`. Search is an
independent internal plugin: literal remains the default, while regex,
include/exclude globs, extensions, gitignore/hidden policy, stable ordering and
snapshot-bound pagination remain bounded inside the Rust binary. Read tools
enforce byte, depth, entry, match and result limits; `read_file` returns
resumable bounded segments (see the pruning contract above). Project-relative
paths reject traversal and use symlink-aware capability discipline. Explicit
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
- `write_file` — bounded atomic creation by default, refusing existing paths even
  at publication; explicit `overwrite: true` opts into whole-file atomic
  replacement with permission preservation;
- `edit_file` — exact unique single/batch replacements planned against one
  original snapshot, rejecting overlapping ranges before one conflict-revalidated
  atomic commit; bounded match-failure line hints reach the next model request
  without extra reads or fuzzy mutation;
- `apply_patch` — one existing UTF-8 file, multiple exact hunks, complete
  in-memory validation followed by one snapshot-checked atomic commit;
- `export_patch` — bounded workspace-to-HEAD Git patch plus source SHA manifest,
  including non-ignored new files, fresh ignored artifact paths and completion
  manifest published last; uses the same isolated Git query policy as review;
- `exec_command` + `write_stdin` — run-owned command sessions with incremental
  stdout/stderr or PTY output, stdin/poll/terminate, bounded cursors and TTL;
  non-PTY stdin defaults to EOF, explicit `stdin: "pipe"` retains interaction;
- `run_command` — a one-shot compatibility wrapper over the same
  `ProcessService`, with shell `command` or literal `argv`, optional bounded
  verification and fenced workdir (project root by default). Both command
  entrypoints share the frozen preparation/permission and receipt pipelines.
  Explicit evidence recording creates unique project-fenced artifacts containing
  retained output, declared log snapshots and bounded before/after file digests;
  the model and journal retain the path/digest, without adding raw streams to
  the journal or user-edit recovery history. Failed publication cannot change
  the command exit or cause an automatic retry; Read Only cannot record.
  Cumulative verification selects Rust/Playwright summary parsing or bounded
  per-test Playwright JSON accounting; expected failures remain separate from
  ordinary passes. Format counts never mix supporting-host output into the
  selected test suite.
  Unknown stdin ids may return bounded current-owner session metadata, never
  command/output/environment or another run's authority; recovery is advisory
  and does not execute, redirect input or consume output.
- `terminal_snapshot` — a Read tool over a current-run PTY's bounded VT text
  projection, folded before raw output eviction/consumption. It sends no input
  or terminal query responses and extends no lifetime. A native `vt100` parser
  stays inside the binary. Unsupported dimensions/input overflow invalidate
  the projection; screen text is transient and omitted from the journal.
- `verify_observation` — a Read tool using the shared capability-relative,
  bounded ordinary-file reader. Explicit independent truth, exact expected
  JSON/sequences and nonempty quantified ordering checks return per-claim
  verdicts bound to artifact bytes by SHA-256. Contract literals are omitted
  from durable arguments; no raw observations enter results. It authenticates
  neither the observer nor the completeness/correctness of the chosen contract.
- `session_info` — a Read tool over the current ProcessService run binding,
  exposing only conversation identity and project root, without storage scans
  or another run's metadata; cancellation/unbind invalidates the result source.

`ProcessService` is the only agent-command spawn seam. One Trusted Project
owns the service; each active Run binds a fresh generation and session id.
Process ids never cross that boundary, at most eight jobs run concurrently,
and run cancellation/terminal, TTL, explicit termination and Application
close all terminate the owned process group and ordinary descendants. Each
stdout, stderr and PTY stream has a 256 KiB transient ring; the LSP
managed-stdio channel uses separate dedicated buffers from the same service
(its stdout ring and stdin-write cap are larger); tool results are
separately bounded. Raw streams stay out of the journal; optional one-shot
evidence artifacts retain only bounded output and explicitly declared files.
Stdin has no persistence path. `write_stdin`
redacts its characters from the durable `tool/call` while permission review
and invocation still see the complete arguments.

`exec_command` may prepare a direct `argv` invocation using the same sandbox
planner and frozen Execute authority as shell commands. Optional verification
mode attaches a run-owned bounded accumulator before output-ring eviction and
model display clipping. Complete Rust output signatures are folded across
reader chunks; stream EOF, overflow and supervision facts constrain the final
receipt. Counts are observations, never authenticated coverage. The OS status
belongs only to the directly launched child. No new frontend lifecycle or
durable-event vocabulary is introduced.

A second bounded accumulator retains Rust failure identities and short panic
lines before output-ring eviction. Its context survives polling and result
pruning, with unavailable/incomplete evidence disclosed; the journal drops
diagnostic text and preserves only bounded observations. It offers failed/affected
check guidance without executing a retry. Evidence file digests are explicit
observations, not dependency analysis, executable attestation or build freshness.

Verification-mode formatting reserves a bounded receipt envelope under the
shared serialized-result threshold before adding diagnostic excerpts. The
production pruner remains enabled for every tool; it does not replace these
already-bounded results. The existing tool/result journal projection keeps
receipt metadata without stdout/stderr, and checkpoint/replay preserves that
recorded value. Raw output retention, verification relevance and actual Agent
adoption remain separate concerns.

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
a container boundary. Full Access is deliberately unconfined. Windows uses
the same binary's internal restricted-token runner, workspace/scratch capability
ACLs, Low integrity and a kill-on-close Job. It reports partial write confinement
with unrestricted reads/network and a hard-link alias limitation. Linux reports
an explicit no-enforcement fallback for `auto` and rejects `required`.
Permission classification remains in core before middleware and invocation.

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

Read Only native writes remain project-relative after approval. Project Write
and headless runs share `WritableRoots` with the OS sandbox provider: workspace
+ run scratch on Windows; Unix additionally retains `/tmp` and process temp.
`Project` and `SandboxService` share the same run-bound roots source, populated
by ProcessService at bind and cleared after process teardown. Each root has an
opened directory capability for native writes. Full Access can unlock arbitrary
absolute writes. `exec_command` also accepts the active scratch as workdir.
Scratch guidance is added to the execution context after binding, without
creating a persistent session fact. Every call flows through:

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

Todo state, validation and append/flush/publication ordering live with the Todo
conversation service. Utility and title contracts live with their conversation
producers. `plugins/services.rs` keeps service keys and compatibility re-exports;
consumers do not acquire a new persistence owner or alternate service registry.

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

### Presentation context

`src/i18n/` owns immutable embedded catalogs, language normalization, safe
named-parameter formatting, optional `UiMessage` descriptors and atomic UI
preference merges. Clients obtain these through `client_ports::presentation`
and pass a `LocaleContext` explicitly. No mutable process-global locale exists.
Core producers can supply English facts plus optional presentation metadata;
client rendering chooses the language. Journal events, RunEvent wire, tool
schemas and model input have no presentation locale.

`web/i18n.js` is shared by browser and native welcome/workbench pages. It
updates existing text/attribute bindings; raw message bodies and user input are
not translated. Desktop injects its own context, while browsers keep their own
preferences. Terminal UI preferences use an independent file lock and never
acquire the business storage-root lease. See [Interface languages](localization.md).

### TUI

`tui.rs` and `tui/` own terminal initialization, input, rendering, dialogs,
view models, and channel multiplexing. They do not create providers, tools,
storage, MCP processes, or run workers. An approver adapter and user-question
adapter translate core requests into dialog state.

A run's real terminal state persists on the status bar after the run ends
(cleared when the next run starts), and a completed run with observable
unfinished todos adds an honest transcript accounting line derived from the
existing `workflow.details` projection — `RunCompleted` is never presented as
verified business-goal completion, and no automatic completion score exists.

### Headless runner

`exec/` maps stdout/stderr/stdin to the same Application ports. It injects a
request-scoped terminal approver, denies side effects when stdin is not a TTY,
supports versioned NDJSON events, and owns no process-global signal handler.
`main.rs` translates Ctrl-C into an injected `ExecCancel`.

### Local server and PWA

`serve.rs` and `serve/` implement a loopback-only HTTP+SSE frontend. The server
bridges permission requests to `approval.requested` events and
`approval.respond` RPC calls, exposes core command/session/run use cases, and
uses the same event envelopes as headless JSON output.

Host runs also inject `ServeAsker` through the existing `UserAsker` port.
Its project-local pending table arbitrates `question.respond` with first-valid-
answer semantics. Pending questions replay to new subscribers under the same
lock ordering used for publication and resolution. Requests/resolutions are
ephemeral `notice` kinds (`question_requested` / `question_resolved`); original
tool calls/results own durable history. Attached terminals only own dialog queues
and response transport, never question arbitration or persistence. PWA renders
inline question forms over the same notices/RPC, with single-flight submission
and DOM/connection ownership guards against late replies. Reconnect rebuilds
pending forms; browser drafts are local, ephemeral, and never authoritative.

Read-only `interaction.catalog` projects the core command registry and existing
skill overview as structured metadata (names, aliases, descriptions, argument
hints, source/constraints and diagnostics), never skill bodies or an invocation.
TUI/PWA own filtering and focus; selecting a candidate only edits input or opens
an existing dedicated UI. Browser text drafts and conversation-find identities
are page-local, non-durable UX state. Text draft ownership follows project,
session selection and lazy session materialization; monotonic composer
generation fences late submission/suggestion/upload completion. Find uses raw
user/assistant body data and frontend-stable message identities, not DOM text
or a second journal projection. `session.history` optionally checks
`expected_selection_generation` under the Application lock; clients additionally
discard responses after selection/connection changes. No new durable events or
RunEvent variants are introduced.

Workbench tool RPCs extend the native host method catalog, not the DSH journal
or RunEvent vocabulary. `turn.changes` / `turn.restore` use a core-owned private
native commit ledger bound before each run round, populated at `WritableTarget`
publish (write/edit/patch). Baseline, captured after and current disk checks are
separate facts. Recovery is confirmed, idle-only, selection/revision fenced,
no-follow and no-clobber; durable per-file partial status and retained detached
material handle conflicts/crashes without claiming universal undo.
`files.search` / `files.preview` / `files.reference` clone a project-scoped
reader for bounded I/O; quoted input is a rechecked fixed editable snapshot.

`session.organize` updates workspace-owned explicit pinned/archived flags;
TUI and PWA share them without altering session selection or deleting journal
content. `workflow.details` composes Plan Mode, versioned plan documents,
TodoService and GoalViewDto facts; typed `workflow.action` and legacy Goal command
actions fence the session and CAS the target state under the Application lock. `tasks.list` / `tasks.logs` expose the
selected current ProcessService generation with bounded, non-consuming tails;
they introduce no process lifecycle or cancellation authority. Browser requests
also fence late UI completions by project, selection and request generation.
Online page notifications remain opt-in presentation of live control facts,
deduplicated by host invocation ids rather than connection-local SSE sequence.

`workspace.changes` / `workspace.diff` use core-owned `WorkspaceReview`, cloned
from a trusted project's capability before bounded Git I/O outside the
Application lock. A try-lock admits one query at a time; child deadlines and
output/file caps bound resource use. Queries strip inherited `GIT_*` redirection,
use literal project-relative paths, disable optional index writes, external
diff/textconv/fsmonitor/filter drivers and lazy fetching, and cannot stage,
commit, revert or push. Untracked preview reads use the existing fenced Project
reader. Errors/non-Git/missing-Git/truncation remain explicit. These DTOs report
working-tree state, including prior or externally produced edits; agent/run
attribution needs a separate captured-baseline/operation/disk model and is not
inferred here. The RPC method catalog registers all three additive methods;
frontends only present the facts and do not spawn Git or persist review data.

Model profile summaries expose a non-secret `limits` object (output tokens,
context tokens, per-run budget). `model.profile.save` accepts that same object
as a whole replacement; omitted limits preserve the existing route's settings,
while null entries remove numeric overrides (budget zero explicitly disables
the budget). Core validates before mutation and updates typed overrides together
with effective fields under the host model-update lock. Saving does not activate
the profile. Headers, extra request bodies and credentials remain unreadable.

The non-secret `tuning` projection/edit carries temperature, parallel-tool calls,
and thinking level using the existing typed `Override` vocabulary: `{"set":value}`,
`"clear"`, or `"inherit"` (also the default for omitted patch fields). Core validates
before mutation and applies only the explicit patch through `ModelConfig`'s
existing override logic, preserving unrelated overrides and hidden JSON. Attached
editors submit changed tuning fields only, except a new name/route carries all
visible tuning values. Thinking clear removes an effort override, not reasoning
itself; parallel clear means omission, not false. No new vendor rules are in UI.

The `extra_headers_edit_supported` capability enables write-only `extra_headers`
whole-object replacement. Omission or null preserves the exact same route's
headers; `{}` clears them. Validation runs before any write and checks string
values, HTTP syntax, control characters and bounded sizes (128 entries, 256-byte
names, 8192-byte values, 64-KiB encoded object). Neither summaries nor errors
contain header names or values. This uses the existing model update lock and
profile persistence owner, does not activate a saved profile, and does not copy
hidden fields across routes.

`extra_body_edit_supported` adds a write-only whole-object `extra_body` edit
(omission/null preserves same-route content; `{}` clears; 64-KiB encoded limit,
depth 32). Replacement migrates legacy overrides, then sets `thinking_level`
to None and its override to Inherit, preserving raw JSON through activation and
reload. Explicit Thinking Set/Clear in the same request is rejected before any
write; other typed tuning remains independent. This does not change provider
reserved-field checks or null-tombstone semantics. Terminal JSON drafts share
masking/parsing/dispatch cleanup; after Body dispatch the tuning form is rebased
so a later save cannot accidentally generate a Thinking Clear from the reset.

The `auth_edit_supported` capability enables write-only `auth` edits containing
optional `header` and `prefix` strings. Omitted/null entries preserve the existing
same-route fields; empty strings explicitly clear them. No auth value appears in
the read DTO. Core validates HTTP header syntax and size/control-character bounds
before applying any part of the save, under the existing model-update lock.
The terminal tracks explicitly edited fields and clears auth drafts on dispatch;
it never reconstructs unknown secrets from a masked display or retries a save.

The initial SSE replay is a message-aligned 50-message tail page. Every replay
variant carries its journal `seq` and owning `turn`; `session.history` pages
backward with an exclusive `before_seq` cursor and never splits a turn. Session
arming retains the already-folded replay prefix in core, so switching performs
one full physical scan, older-page requests are memory-only, and freshness
decodes only a newly committed suffix. Append-only compaction does not
invalidate existing cursors; the `/update` ensure-current publication
(v0, v2, or v3 sources upgraded to the current V4 generation) naturally
re-arms a new cache with the new generation. The full bounded message outline
travels on `subscribed` and refreshes again at each durable `prompt.settled`
boundary, so live navigation never guesses journal sequence numbers.

When a run is active, reconnect replays history strictly before its initial
admitted user-message sequence, then the complete captured run-event prefix
and the queued live suffix. Registration and history capture serialize with
admission and session selection. The frozen cursor also covers goal continuations
and WeChat admissions; identical text is not a deduplication key. Once the run
settles, reconstruction returns to journal-only history.

Manual PWA compaction is exposed as `session.compact`. Serve owns only a clone
of the core `CompactHandle` so it can report an `active_compaction` snapshot and
route cancellation; summary generation, journal replacement and replay folding
remain Application/session responsibilities. `CompactionUpdated` notices clear
the frontend slot and drive the browser projection, including recovery after a
reload. A run and manual compaction remain mutually exclusive at the core
boundary.

The embedded `web/` workbench is a projection client. Static assets and pairing
shell contain no credential. Browsers pair through the authenticated local
trust fence and use a private HttpOnly browser session; raw API clients can use
Bearer credentials. New raw tokens are never persisted in browser storage or
placed in URLs. Browser persistence contains presentation preferences, not
conversation facts.

One document lazily instantiates a view context for each opened project. Each
context owns its lexical state, fixed RPC prefix, event subscription, async
closures and DOM root. Only the active root is attached; inactive roots remain
in memory and continue receiving their own events. Switching changes the visible
context, never the owner of a pending send, approval or query. Keyboard, focus
and page navigation target the active context; project events remain local.
The shared Host view owns the authenticated directory and query/hash routing.
The application frame contains fixed navigation, one content host and an
optional extension host. The conversation page owns its project/session list
and message detail; that list is not a peer of the application content host.
Other features replace the content page with their own views. Hidden
conversation controls are inert, while drafts, subscriptions and reading
anchors stay with their existing project context. Resources and conversation
file review instantiate the same bounded file-browser component; persistence,
path fencing and snapshot admission remain core-owned.
Rust combines the existing embedded source modules into an ordinary factory
function with no eval, iframe, build pipeline or new runtime dependency. The
production factory exposes activation, deactivation and display summaries only;
legacy fixture inspection ports are included exclusively in Rust test builds.
All open contexts register unsaved-work readers with the native navigation
guard. This remains page-local presentation state, not a second persistence
owner or a replacement for core selection/admission checks.

Workbench tool cards pair by call id with the same semantics as the TUI
conversation model: one card per call, opened at `tool_requested` and settled
in place by `tool_finished`/`permission_denied`; replay keys add the journal
turn so identical ids in different runs never merge, duplicate or late
`tool_requested` frames reuse the existing card instead of opening a second
one, and a live finish settles the card a replay prefix opened (unique
suffix-key fallback). Results without a usable call id still render a
fallback card at the finish position — success cards enter the process
summary and error cards stay outside it. Terminal notices are deduplicated
per run and per `prompt_rpc_id`, a settled frame owns the terminal only when
no run is active (a late settled after a new `run_started` changes nothing),
and asynchronous unfinished-work accounting is fenced by an anchored owner
snapshot (session id, selection generation, stream object): identities not
yet materialized anchor at the first observed concrete value inside the same
subscription handshake, and any later concrete change drops the reply
without consuming its dedup slot. After a reload or reconnect the last
durable turn outcome restores the persistent run-state text and re-derives
the accounting from the existing `workflow.details` projection; no new wire
method, durable event, or `RunEvent` variant was added for any of it.

Image drafts remain frontend presentation state until the server admits them:
the workbench sends one bounded PNG/JPEG stream into a server-minted, selection-
bound draft scope, then references only opaque upload IDs in `prompt.send` or
`steer.send`. Core imports a queued steering image before it can be claimed,
but its descriptor is durable only at the recorder's append-and-flush point.
History metadata travels through replay/SSE; image bytes are loaded later from
an authenticated active-session reachability endpoint into revocable browser
blob URLs. Thus neither a host path nor a bearer token becomes image authority.

The Plugin Index panel combines a public catalog projection with authenticated
local plugin control. The projection fetches `https://pi.at.cn/catalog.json`
with credentials omitted and never sends the local token cross-origin; the
public catalog has no install RPC. Installation and package management use the
authenticated local host RPC, which owns signature verification and permission
review. Model trace protocol ids are mapped to human-readable presentation
labels while the original `RunEvent` vocabulary remains unchanged and available
as diagnostic metadata.

The Web/PWA about page offers the public GitHub releases page as an explicit
user-opened recovery link. It opens without an opener or referrer and is not a
Host RPC destination. Static destination checks allow that exact public URL
alongside the catalog; they reject unknown destinations and credential-bearing
URL variants without requiring an exact inventory of visible links.

### DSH client

`dsh/` implements the HTTP/WebSocket client and host lifecycle for `clat dsh`.
The TUI reducer maps DSH events into the same presentation state, but commands
and session facts stay on the DSH host. Local access to `~/.dsh/storages` is
read-only and used only to populate session selection. The sole client-local
write is the bounded, fail-soft `~/.clat/dsh-last-session` presentation
preference; it is owned by `dsh/last_session.rs`, not by control storage.

## Source map

`model.rs` retains the provider-neutral contract while `model/` owns
configuration, thinking controls, credentials, cancellation, spend accounting
and context/image projection. `session/use_cases/` keeps lifecycle, attachment,
history, active-state and folding-journal implementations private to the
SessionService facade. The active session owns the unique lazy folding journal;
producers cannot construct independent append/flush lanes. `plugin_host/`
separates MCP decoding/connection accounting and bounded form interaction from
shared host semantics; WASM grants are private to `plugins/wasm/`. TUI key routing
preserves global, trust, selection, modal and composer priority explicitly.

| Path | Responsibility |
|---|---|
| `src/application.rs`, `src/application/` | client-neutral use-case facade, DTOs, run/session lifecycle |
| `src/application/composition.rs` | Trusted Project catalog, typed port resolution, freeze/wiring, Run children, and teardown |
| `src/application/run_context.rs` | request-bound workflow composition, tool view, and durable request-header assembly |
| `src/application/run_context/start.rs` | pre-admission input capture and post-admission context/header freezing |
| `src/application/run_admission.rs`, `src/application/run_admission/` | atomic run admission, committed witness, and selected-session birth/projection policy |
| `src/application/run_execution.rs`, `src/application/run_execution/` | two-phase activation, worker progress, round settlement, and explicit run-resource cleanup |
| `src/application/remote_control.rs` | remote binding/pairing/delivery use cases and durable chat-to-session recovery orchestration |
| `src/run/` | agent-step orchestration, request preparation, response settlement, serial tool calls, and atomically sealed steering |
| `src/model.rs`, `src/providers/` | provider-neutral model contract and adapters |
| `src/tool.rs`, `src/native_tools.rs`, `src/apply_patch.rs`, `src/search.rs` | tool contract and native coding tools |
| `src/process/`, `src/plugins/process.rs`, `src/sandbox.rs` | run-owned process sessions, bounded output cursors, exec tools and platform sandbox policy |
| `src/project_instructions.rs`, `src/plugins/context/instructions.rs` | scoped project-instruction discovery, caching and observation |
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
| `src/exec/` | headless frontend; interruption/late-handle coordination is private to `cancel` |
| `src/wire/`, `src/session/catalog/` | explicit wire codecs; catalog-owned runtime projection seats generate recorder dispatch, wire encoding/decoding and tags |
| `src/serve.rs`, `src/serve/`, `web/` | local API, SSE, and embedded PWA |
| `src/dsh/` | crate-private DSH client protocol, host lifecycle, and client-local last-session preference |
| `src/demo.rs` | deterministic offline composition |
| `src/upgrade.rs` | authenticated self-update |
| `wit/`, `sdk/clat-plugin/` | WASM contract and author SDK |
| `sdk/dsh-adapter/` | static Cordis/DSH compatibility adapter package |

Built-in context plugins and conversation-maintenance plugins have separate
families under `plugins/context/` and `plugins/conversation/`; Application
remains the explicit composition root. WASM's per-invocation clock budget and
interruptible subscriptions are owned by `plugins/wasm/clock.rs`.
TUI model picking owns navigation/confirmation independently from the edit
form, while DSH history loading owns its page/live reconciliation sequence.

Image-bearing TUI starts use an ownership handoff rather than decoding on the
terminal thread: a bounded frontend worker temporarily owns
`TrustedProjectApplication`, performs core admission, and returns it in a
`RunStartFinished` message. A one-shot event barrier blocks the core run at its
first event until the TUI has restored the application, installed the run
handle and usage baselines, and cleared the accepted draft. Pre-commit failure
returns the same application and leaves the ordered draft untouched. This is
frontend scheduling only; validation, normalization, persistence, spawning,
and the commit point remain core-owned.

## Code shape rules (2026-09-11 consolidation)

The 2026-09 refactor campaign established these module shapes; new code
follows them so the structure does not decay back. `scripts/code-health.py`
measures the budget at every round close: production functions ≥80 lines
and the root-directory production share may improve, never regress
(hand-counted production giants: 91 / 24% at worst → 48 / 19.7% at the
campaign close; the script-measured series — a different口径, not directly
comparable — kept improving after it: 139 / 19.67% on 2026-09-11 →
132 / 18.38% on 2026-09-22, v1.4.2).

- **Seat tables for vocabularies.** Durable journal events and `RunEvent`
  variants are defined exactly once in the `src/session/catalog/` seat
  tables: one row carries the payload validator, surface membership,
  replay kind, retirement flag, and the recorder/wire projections
  (`run_event_seats!` generates both dispatches with compiler-checked
  exhaustiveness — a missing variant fails the build). Adding a
  vocabulary member is one row plus its golden; a new string-match arm
  elsewhere is a violation the architecture tests reject.
- **Priority chains for input dispatch.** Frontend key handling is a
  short chain of ordered handlers (global → trust → copy/cut → modal →
  composer; see `tui/keys.rs`), each returning whether it consumed the
  key. The former 461-line `handle_key` match is the cautionary tale.
- **Per-family files for service groups.** Large service areas split by
  use-case family into directories (`session/use_cases/`,
  `src/model/`, `src/plugins/conversation/`), never into one growing
  file. The `src/` root accepts no new production files — a new domain
  opens a directory; root files are entry shims only.
- **Core and frontends, one binary.** `clat-core` (terminal-independent
  runtime, `src/core.rs` entry) and the root `clat` package (facade,
  TUI/DSH frontends, the binary) share the source tree; the core
  exposes frontend consumption only through `src/client_ports.rs`.
  The core→frontend dependency edge does not exist in the cargo graph,
  and a workspace metadata test pins both the dependency direction and
  the single-binary deliverable.
  The private clat-wasm-net library owns the network engine and authority
  fences. It depends on neither core nor frontends in production; core
  injects its permission lease. Author probes and production compile one
  implementation. This adds no executable or external runtime requirement.

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

## Explicit durable tasks

`task/` owns a bounded, session-scoped work objective and semantic checkpoints.
`TaskService` is its sole semantic writer; `SessionService` validates project,
session and revision under the active-session lock and commits `clat/task`
snapshots through the existing durable admission/append path. The catalog owns
payload validation and replay disposition; `TaskUnit` shares strict transitions
between online folds and checkpoint/tail restore. Task events are log-only,
ignorable extensions for older readers, independent of model-context surface
replacement. Ordinary sessions have a null task projection.

Application owns task creation, user-only resolution/contract authority, completion gates, recovery check,
and explicitly checked continuation. A continuation descriptor binds task ID,
revision and an observation digest, with no durable activation authority. A
live binding exists only during the new Run. Goal scheduling remains separate;
creating/resuming/arming/running a Goal excludes unfinished tasks in both
directions, including restored conflicting states. Every Run entry shares the
unknown-operation admission fence; no objective or counter is mirrored
between them. Todo and summaries cannot certify task progress. The current
RunContextSnapshot assembles recovery observations and task progress into both
the actual model workflow and request-header view. Recovery checks and actual
Runs derive tool access from the same context owner (Plan, subagents, verified
image input/tool results and explicit task tools). The recovery digest binds the
current model configuration too. Pre-run automatic compaction
is skipped for task Runs; request-boundary compaction uses the same spend ledger
as task settlement, including failed/unknown usage reservations.

The task plugin contributes bounded metadata tools, the `/task` command and
execution middleware. Normal permission/prepared-tool checks precede middleware;
it durably records an intent before invoking an external operation and commits
a result classification afterward. Intent/outcome failures return typed fatal
tool errors; a failed task fence also blocks nested follow-up effects before the
outer tool can return. Arguments/results are represented by hashes, not copied
outputs. Interrupted intents remain unknown, confirmed prior effects are not
replayed, and only human resolution can authorize reconsideration. Run-level
process cleanup and ownership remain unchanged.

Each new task Run reserves its bounded token/time slice and Run count under a
unique UUID before model work. `TaskRun.turn` remains null until the session
admission batch commits; a separate durable transition binds the actual turn
before worker activation. Only bound Runs can record operation intents. Failed
starts and interrupted unbound reservations remain distinct budget records;
retry never guesses a strictly increasing future turn or fabricates a message.
Settlement consumes the existing shared ledger and monotonic elapsed
time before terminal publication; an interrupted/failed settlement keeps the
reservation. Recovery never resets totals. Task state pins a hashed stable project-directory witness; replacing the
project root cannot silently rebind old progress, even under the same pathname.
Artifact checks traverse the held project-root capability
without following symlink components, read bounded bytes and report
changed/missing/restricted separately. Injected job queries are read-only,
cooperatively deadline/cancellation bounded and never return process ownership.
Frontends only render progress or pass a continuation descriptor through their
standard channels; they do not own recovery policy or state.


### Task acceptance owner and final commit

Optional `TaskState.acceptance_state` extends the existing Task snapshot and
`clat/task` catalog seat. TaskService owns user-confirmed versioned contracts,
human attestations, immutable verification reports, one pending completion
candidate and single-use recheck grants. Admission allows one transition family
per CAS; reports cannot be rewritten or created in the completion commit. The
same validation runs at admission, live projection and checkpoint/tail restore.
Absent fields retain the GA-1 byte shape; there is no new durable vocabulary,
second completion projection or frontend-owned verdict.

The Task plugin explicitly installs an internal checker registry. Process-domain
adapters implement bounded text/JSON/records algorithms and Rust/Playwright
receipt interpretation. JSON assertions reuse the observation rule owner. The
Agent loop has no checker-specific branches. New native adapters require a
unique `(name, version)`, parameter validation, bounded declared-file access,
three-state results and invariant tests; this is not a dynamic discovery ABI.
Unavailable versions remain inconclusive. Local checks execute no commands,
network calls or model requests.

File checks use the held Task root capability and no-follow traversal. One
snapshot collects declared bytes under a shared read/time budget, computes
clause results and rereads hashes for stability. Reports bind content/contract/
attestation identities independently of Task revision. Only metadata writers
use revision CAS; settling a budget does not stale unchanged content. Explicit
operation maps bind command results to this Task's committed native argv
invocation, process identity and unchanged declared inputs. Prepared native
process tools expose the identity of their frozen argv/shell and resolved cwd;
Task intents save it independently of wrapper names or output/polling options.
Both run_command and exec_command therefore share the cross-Run replay fence.
A terminal non-PTY exec launch can supply a receipt; running launches/polls cannot
upgrade an earlier incomplete Task fact. Raw stdout is not
copied into Task facts. Output signatures do not authenticate coverage.

Read-only check/status calls compute views; explicit save calls append a
report. `read_task` and check tools do not create Task operation facts or advance
its revision; normal Run tool-call/result recording still applies. Task-owned
4 KiB model DTOs page operation/report/contract summaries before generic tool
result pruning. Full rules stay in recovery instructions, full reports stay
in the Task projection, and user commands expose the same persisted state.

Model completion proposals remain pending throughout a Task Run. Worker startup
captures its bound Task Run ID. After normal process cleanup, recorder terminal
commit, Task ledger settlement and actual Run-scope disposal, worker finalization
rechecks evidence and commits completion only with prior user authorization,
all required clauses passed and no unknown operations. For Task Runs the recorder flushes the last step while withholding `turn/end`
and the terminal RunEvent. After actual scope disposal, the Task owner rechecks
acceptance and commits a changed Task state with `turn/end` in one atomic durable
batch. The recorder then publishes exactly one terminal agreeing with the final
Application result. Failed gates, disposal, accounting or final persistence
produce no earlier success terminal; fallback error close is attempted through
the same journal. An unknown durability outcome stays fail-closed until recovery.
Ordinary Runs and intermediate Goal rounds retain their existing lifecycle. An unrelated Run cannot
promote an older candidate, and cold reopen never schedules it. A failed final
commit propagates failure rather than publishing a completed Task.

User verified completion uses the same evidence/currentness gate while idle.
Manual declaration, explicit override, human-only attestation, mixed-source and
machine-check completion retain distinct provenance. External file changes
later affect current applicability, without rewriting the historical terminal
fact. This is a local observation/permission contract, not a guarantee that
user-supplied criteria or declared input scopes are sufficient.

## Derived capability views

`tool::capabilities` owns discovery, matching, output bounds and transient
pagination. The sole tool directory remains `ToolRegistry`: registration stores
owner, definition, a random registration identity and optional producer-owned
`ToolCapability` declarations. Existing `Tool` implementations inherit unknown
metadata; `ToolDefinition` construction is unchanged. Definitions are captured
at registration, without calling external tool implementations during queries.
Native file/process, MCP and WASM implementations supply their own declarations.
External descriptions remain data, with provider trust and missing claims visible.

`RunContextSnapshot` obtains the capability view and actual model definitions
from one registry capture. Activation publishes that immutable view; cleanup and
session selection invalidate it. Model discovery never falls back to idle/global
tools. User commands go through Application's idle-only diagnostic view and the
same query engine. Frontends display core content and perform no matching or
permission decisions. Attached TUI preserves capability JSON arguments when
forwarding through existing `command.run`.

Registration identity, view identity, registry generation and observed adapter
lifetime bind detail ids/cursors. Idle views also bind the existing external
startup observation generation. A query checks generation before/after building
its reply and checks cancellation; no locks enclose arbitrary provider callbacks.
Observations have an epoch and time, with explicit scope: an adapter lifetime or
startup connection is not a health/login test. MCP/WASM status remains owned by
the existing domain; typed, secret-free projections accompany idle queries.

Discovery produces no durable capability events, database or automatic action.
Its model tools use ordinary journal/result projection and are excluded from Task
operation intents. They do not advance progress, settle unknown effects or spend
verification grants; ordinary model-request accounting still applies. Suggested
tools are invoked separately through the original lease, preparation, permissions
and Task/acceptance pipeline. A query result never becomes an execution handle.

Registration also owns public-schema preparation in `tool::capabilities::schema`.
The optional `Tool::public_definition` adapter hook and mandatory registry pass
form one publication boundary. Only annotations on recognized schema nodes may
be cleaned; validation strings/keys and opaque data remain unchanged. Unsafe
structural disclosure rejects registration before registry mutation. MCP keeps
credential values in transport state and supplies private predicates to this
boundary. Model definitions and both discovery domains then share the stored
public definition; no fragment-level cleanup or extra query-time callback is
needed. The source tool's original call contract remains available internally.

## Native desktop client

The optional `clat-desktop` workspace member lives in `desktop/` and is excluded
from default CLI/core gates. Tauri and system WebView integrations belong only
there. Its installer carries a private CLI backend, while core owns validated
cache staging, leases, discovery, project trust and process startup. The GUI
never constructs Application, writes shared state or spawns business processes.
Native installation/update and window preferences are frontend-local OS concerns.

The application shell has a global toolbar above navigation, content, and an
independent right extension host. Conversation list/detail is nested only inside
Conversations; content-page routes never zero the extension width or remove its
accessibility. Selected review surfaces retain their project/session ownership.
The project host owns bounded in-document navigation history, while the active
project view supplies the toolbar location and applicable conversation actions.
The project tree composes project/session metadata without changing core selection:
expanded mounted groups may read session.list, while unmounted groups remain lazy.
Only an explicit conversation/new action activates a view and waits for its
connection, workbench state, and selection generation before using the existing
switch/new path. A navigation token and active-view check cancel stale waits.
The view exposes scoped navigation metadata/actions rather than application internals;
existing active-session menu handlers and draft/approval ownership stay in that view.
Desktop maps this same toolbar into the macOS overlay title area or a custom
Windows title area. A curated `desktop_window` command handles only frame info,
drag, minimize, maximize/restore, and close, with the existing native guard and
close-to-hide lifecycle. Welcome and older Host pages retain fallback chrome.

The shell loads the verified Host's existing workbench origin and uses a narrowly
scoped native bridge. Permissions still go through the Host. Window close hides;
GUI quit leaves the detached, leased backend alive. One Host owns one storage root
across CLI/TUI/PWA/desktop. See [CLAT desktop client](desktop.md).

`host.describe` advertises native-client compatibility generation 1, build kind
and capabilities. Published compatible builds can share a Host despite different
file fingerprints; development builds stay strict. Version preference is separate
from compatibility: only a strictly newer backend may request the existing idle,
instance-fenced takeover. Readiness is observed through `host.runtime_status`;
the takeover owner rechecks atomically. Root identity/authentication and exact
Host/wire/journal compatibility checks precede build compatibility.

`GET /api/notifications` is an authenticated, bounded, host-wide SSE metadata
projection. It adds no variant to `/api/events` and no journal vocabulary. It
avoids a project connection per subscription, so idle project reclamation stays
possible. An attached desktop can receive pending approval/question notices in
background projects; decisions still require the ordinary permission/answer RPC.
`RunHandle::session_id()` freezes notification routing at admission. Notification
failure never changes execution facts or retries a business operation.

Source map: `src/host_client/backend.rs` owns packaged-runtime validation/cache;
`src/host_client/notification.rs` owns the native notification client port;
`src/serve/notifications.rs` owns the transient Host projection; `desktop/src/`
owns native windows, curated IPC and desktop updates.

The desktop-workbench-v1 capability also covers the workbench composer contract
(`#prompt`) used by the native draft guard. A DOM or behavior change that breaks
that guard requires a compatible adapter or a new native-client generation.

Desktop author tooling lives in `desktop/scripts/` as TypeScript executed by
Bun. It stages the private Rust backend, invokes the pinned Tauri CLI through
Bun, and handles owner-side packaging/signing/publication. The tooling is outside
the Cargo runtime graph, consumes the root version and typed asset inventory,
and adds no runtime dependency to CLI/core or installed desktop applications.

From v2.1.2, `desktop/scripts/release-index.ts` owns the compact 23-file release
inventory, checksum grouping and signed payload projection. One `latest.json`
envelope carries both trust roots' signatures over the exact payload text;
Tauri package signatures are embedded in its four platform entries. The CLI
verifies through `src/upgrade/manifest.rs`; desktop's native policy verifies its
own root before restoring authenticated metadata into the official updater.
No standalone signature assets or legacy updater compatibility are published.
This distribution format does not change Host identity or shared business data.

`desktop/src/update/state.rs` owns exclusive check/download/wait/install phases,
cancellation and retained verified bytes. `update.rs` coordinates authenticated
metadata, the official updater and the read-only Host readiness port. Restored
updates initialize the existing rustls ring provider explicitly and give package
downloads a timeout. Installation runs on a blocking worker; cancellation ends
at the atomic transition into native installation. The bridge only renders state
and guards page-local drafts; menu events reuse the same commands. Waiting or
installing preserves the current page, suppressing notification navigation and
backend reconciliation during the operation; automatic reconciliation resumes
afterwards. CLI and cached Host files are
outside the GUI install target.

The v2.2.0 shared-Host coordinator lives in `src/host_client/reconcile.rs`.
`PackagedBackend::reconcile` owns existing-Host discovery, compatible attachment
and initial idle activation; its `BackendConnection` client-port result keeps
`Missing` and `Connected` distinct from failures. Official builds connect across
versions using the existing native compatibility contract; development builds
retain fingerprint checks. There is no historical protocol adapter or migration.
CLI startup and Desktop share the typed `try_upgrade_takeover` primitive; only a
`busy` error permits continued attachment, while other failures retain their
error. The Host atomically arbitrates readiness for all projects. An attached
Desktop retains its selected project and compatible Host even after it becomes
idle, avoiding page reloads that could lose unsent text. A newer packaged backend
can activate at the next Desktop launch or explicit project open.
Packaged startup, reconciliation and explicit stop share a coordinator lock.
Explicit stop suspends polling until a successful project open; observation of a
missing Host never starts a writer. Desktop's notification loop projects these
results into native connection state; its welcome/about pages display and refresh
that state. A frontend connection lock serializes publication/navigation with
open/stop and notification selection; stale notification targets cannot reattach a
stopped or replaced instance. No lifecycle or upgrade policy is implemented by
the web page.

Session history is owned by `src/session_query/`. A Host-shared lazy SQLite connection maintains the disposable FTS5 cache, while `SessionService::query_reader` exposes only committed header/revision/visitor reads. The query service derives authority from project binding and the control-plane trust table; Agent parameters cannot supply project authority. `HistoryReader` is the cloneable client port. RPC and TUI continuations release the Application lock before indexing or inspection. Search metadata follows catalog replay classifications and title projection rules. UI presentation retains references and never selects a session implicitly.

Native background commands are owned by `background_jobs::JobRegistry`, with a Host-shared capacity budget and a detached `ProcessService`/sandbox per job. The planner, permission gate and process launcher remain shared with normal command tools. The registry records `job/registered` before spawn and `job/settled` only after a real terminal observation; execution, persistence, cleanup and notification failures remain distinct. The catalog owns native job phase classification; the job projection/checkpoint and restart recovery consume it.

`SessionService` keeps independent display/execution facades over a shared weak writer table keyed by the full `SessionKey`. A borrowed target uses the same coordinator, folding journal and projections as any selected facade. Acquiring a target never changes the foreground selection. Acquisition and last explicit close share a lock, including the last strong-reference release, so simultaneous detach cannot leak a writer thread or create a second owner.

`scheduler::SchedulerService` owns one Host control document, frozen user grants,
occurrence reservations and delivery settlement. `application/schedules.rs`
exposes user management; native schedule tools can only propose drafts scoped to
their real bound session. `serve/automatic.rs` is a Host timer/notification pump:
it acquires the ordinary project Run slot after human work, then calls the same
`RunExecutionEngine`. It does not implement another Agent loop. Future timers
survive restart and do not block upgrade; unsettled deliveries and active Jobs do.

`application/target_services.rs` binds session-specific tools, permissions,
processes, Task/Todo/Goal/Plan, history query and project-instruction observation
explicitly for an automatic Run. Its immutable tool/pipeline fork retains the
original definitions, middleware and result transformations. Common context
assembly stays in `run_context.rs`. A target facade never changes foreground
selection or consumes the user's armed skill. Automatic events and interactions
carry their original session in control notices; they cannot enter another
conversation's live Run stream. Durable events keep their existing vocabulary.
Recovery reads committed replay and receipts, never repairs or reruns an unknown
committed occurrence. See [scheduled work](schedules.md).


### Navigation-page data and lifecycle ports

`navigation-pages.js` owns explicit page entry/leave state and unsaved presentation
snapshots. It does not override `showModal`/`close` on navigation pages; temporary
editors and global extension previews retain their distinct overlay behavior.
`task-center.js`, `resource-library.js` and `conversation-reader.js` reuse scoped
project ports. The active conversation pane may be hosted in task details, then
returned on leave; read-only history never feeds the active replay/admission state.

Public `history.conversation` and `history.attachment` ports resolve stable project,
conversation and sequence references through the query service. Pagination checks
source versions, excludes hidden context and verifies attachment descriptors before
reading private blob storage. Directory enumeration uses the existing project fence.
Library persistence is core-owned private metadata, with stable registered project
identity, bounded atomic writes and no duplicate message/file body. A submitted file
reference is reconciled only against its durable client-message identity and exact
submitted snapshot; an unknown receipt is not proof of admission.

Scheduler task title and notification fields are additive and omitted when absent,
preserving existing specification digests. Edits use existing authorization revision
rules. Manual deliveries reuse atomic reservation and settlement, with a durable
request-derived occurrence identity; they do not advance the regular clock.
WeChat user projection exposes authorization sources without credentials; complete
revocation updates paired and manual grants in a single control-store transaction.

## Contextual work area and workflow documents

Web/Desktop use one contextual two-column surface owner, separate from global
navigation. Conversation retains its sole list/chat/composer DOM and event
subscription; one current tool supplies list and detail regions. Enter, leave,
reset and restore invalidate request epochs and exclude hidden regions from focus.
The tools use explicit region lifecycle rather than native dialog `open` state.
This is presentation state and does not select sessions or affect Host execution.

The Application workflow facade exposes typed `WorkflowRequest` / `WorkflowIntent`
through client ports and additive `workflow.action` RPC; `workflow.details`
returns document revisions, review, goal, to-dos and pending-handoff state.
Session identity, expected workflow version, process-local authority generation,
operation ID and goal ID/revision fence mutations. Stop/restart invalidates
unaccepted old workflow requests so a delayed approval cannot restart execution. Mode changes and review/execution handoffs belong to the core.
The serve adapter only supplies the usual asker/approver/event/completion ports
and drives a ready core action after the preceding worker has joined.

PlanModeService owns document mutation, shared review arbitration and exact
revision approval. `clat/plan/change` has one catalog seat, strict admission and
a checkpointable projection. Markdown is core-owned derived session output.
GoalService retains execution, accounting and acceptance; creation plus new-plan
binding use one durable journal batch. A human completion candidate records an
awaiting-user-acceptance blocker without reporting completion or wasting retry
allowance. Normal prompts retain their old disarming contract; typed in-run input
preserves the explicit goal through queueing/interruption unless manually stopped.
No frontend owns a second workflow or persists plan/run policy directly.

### Human development workspace

`DevelopmentService` owns three project-scoped services behind client ports and
`editor.*`, `git.*`, `terminal.*` authenticated RPC methods. It is not a plugin service
and adds no model-callable tools. Human authorization is established by the trusted
client channel and explicit intent, never an `is_human` field supplied to an Agent tool.
The services do not alter the session permission mode, planning mode or Agent RunOwner.
Blocking filesystem/model/Git/PTY I/O occurs after releasing the Application mutex.

The editor uses held directory capabilities, parent write locks, content fingerprints
and conditional atomic publication. Windows no-replace moves use directory-relative
`NtSetInformationFile`, retaining open-directory boundaries for rename and trash
recovery; they never fall back to an overwrite or an ambient-path move. The editor
deliberately does not attribute human writes to an Agent turn's file-recovery ledger. Git serializes CLAT writes by actual common Git
directory, retains native Git locks/configuration and freezes a private index before
model generation. Staged patches use an explicit Git directory and an empty private
worktree plus an empty process working directory, so Git cannot substitute
stat-matching live files for staged blobs. The empty directory lives at the
validated private ancestor rather than inside `repositories/<id>`, keeping nested
repository IDs out of Windows' process-working-directory length limit. No repository
configuration is changed. Windows Git invocations enable command-local long-path support for
private nested-repository indexes. Final HEAD/index checks and the private index prevent late external
staging from entering the approved commit. Native hooks still have their ordinary Git
semantics, including their own explicitly configured side effects. Commit recovery
checks parent, approved tree and message; an uncertain outcome is not blindly replayed.

Terminals share platform facilities, not Agent run lifetime. Unix PTY groups and shell
job descendants are terminated on close; Windows creates ConPTY children suspended,
assigns a kill-on-close Job Object, then resumes them. Live shells and Git operations
pin their project against idle reclamation/upgrade takeover. Output uses an independent
authenticated SSE stream, byte offsets, a bounded 2 MiB ring and a bounded VT screen
projection for resynchronization. Slow readers are disconnected, without blocking the
Host. Resynchronization explicitly marks lost older output; the VT projection is not a
claim of complete historical replay or full fidelity for every terminal extension.

CodeMirror/xterm widgets are pinned, built offline and checked in with notices. Cargo
embeds the generated widget assets without a JavaScript runtime; font faces come
from the viewing device. Desktop backend
builds verify the source/output manifest and rebuild only stale widgets with build-time
Bun. Source and manifest inputs have LF checkout attributes. No font files are bundled.

The project editor separates lazy tree presentation, document/tab ownership and
CodeMirror views. Shared rows also serve the read-only resource browser; optional
editor menu intents do not grant write authority. Search and directory loads reject
stale generations. Existing editor ports own path fences, conditional saves and
private draft persistence. No new RPC or durable event is introduced by the layout.
One offline format registry supplies language identification, filename aliases,
Markdown fences and file-icon metadata to editors and Git diffs. Language overrides
are document presentation state. The widget build regenerates dependency notices;
its source manifest includes the language/theme modules and notice generator.

The Host terminal directory snapshots project-scoped developer handles before
reading private registries outside Application locks. `terminal.create` accepts
optional initial rows/cols, applied to PTY/ConPTY and the screen projection before
Shell execution; omitted values preserve legacy 24x80 behavior. `terminal.remove`
and `terminal.removeEnded` serialize registry lifecycle changes and atomic removal
receipts; `terminal.close` retains its original stop-and-retain contract.
The device-wide appearance service owns font loading and generations; views only
apply resolved options and serialize the resulting geometry updates.


### Shared navigation presentation

Resources, Tasks, History and Extensions use the conversation/terminal two-column
layout. `navigation-view.js` owns the reusable page frame, SVG controls, search,
tabs and list rows; `navigation.css` owns its geometry and scroll boundaries.
Information glyphs use the shared `ICON_PATHS.info` rounded-square outline in
`app.js`, including notices and empty states; pages do not define local variants.
Terminal reuses these primitives without changing PTY ownership. Page-specific
controllers retain their existing scoped requests, operations and permission gates.
Each list and detail scrolls independently; the outer page reserves no scrollbar.
The capture lifecycle saves reading positions before a page is hidden. Switching
pages does not stop Host execution or create another conversation DOM/subscription.

Resources retains current-project files/materials/bookmarks; its file browser
mounts directly into the common list/detail slots. History keeps search scope and
read-only admission separate from explicit conversation continuation. Tasks keeps
project selection and existing schedule/job controls; its borrowed live conversation
is returned on leave. Extensions separates Installed/Discover navigation from the
plugin owner and permission-review renderer: changing selection cancels unsubmitted
review tickets and clears credentials, while late prepare results are cancelled.
No journal vocabulary, persistence format or public RPC changes are introduced.

Default navigation content reuses `navigationOverview` and the shared SVG table.
Recommendations are projections of the existing bounded public catalog, not new
installation authority or synthetic installed records. Task permission interaction
cards mount inside the detail column, outside the two-column grid: an empty
interaction host cannot allocate an implicit row or shorten the sidebar.

### Source-control workspace

`development/git` owns bounded repository discovery, opaque repository identity,
structured commit files and historical comparisons. Nested repository services
retain separate operation receipts and backups; writes continue to serialize on
the canonical Git common directory, including linked worktrees. Cached repository
admission revalidates directory components and rejects symlink substitution.
Legacy Git calls without `repositoryId` retain their original project scope.

`git.repositories` and `git.commit.files` are additive trusted-client RPCs. Git
queries/operations accept optional `repositoryId`; `git.diff` additionally accepts
`commit` and optional `parent`. History responses retain legacy fields and add
parents, reference labels and a continuation cursor anchored to fixed commit IDs.
Branch/reference labels are refreshed views; a fresh traversal observes new refs.
No journal or Agent permission vocabulary changes are introduced.

The Web source-control composition separates layout, repository-bound actions,
comparison tabs and graph rendering. The graph draws supplied parent identities;
it does not infer history or launch Git. The shared CodeMirror widget supports
read-only split/unified views, content-width adaptation, chunk navigation and
paired scrolling. Device layout settings and repository drafts never authorize
writes; all mutations use existing core concurrency/receipt checks.

`WORKSPACE_TOOLS` is the shared presentation catalog for tool regions, trigger
identities, sidebar shortcuts and icons. Trigger owners attach their own action
handlers; mounting a region alone does not make its entry button actionable.
The Git review entry binds to the Git controller's `open` action explicitly.

Repository discovery uses the existing `ignore` walker without following directory
symlinks. Local project exclusions prune generated/temporary trees before directory
counting. Git's registered submodule/worktree paths are admitted separately and
validated against the project boundary, preserving registered ignored locations.
Resource bounds remain explicit; they count searched directories/repositories,
not changed files. No new dependencies or persistent state are introduced.

The sidebar work summary is a presentation of the existing `workflow.details`
response; it neither edits todos nor treats todo completion as goal acceptance.
Project/session identity is owned by the application toolbar. Existing connection
and model DOM nodes remain subscribed inside Session details, while non-live
connection status is also surfaced compactly outside that collapsed section.


### View identity and contextual application chrome

`view-context.js` owns a presentation registry keyed by stable page/tool IDs.
Controllers supply breadcrumb levels, actual sidebar nodes and action descriptors;
`application-toolbar.js` and `application-breadcrumbs.js` render them without reading
DOM copy to infer ownership. Global pages expose their own scopes and actions.
The menu precedes inline actions. Anchored overlays use their actual toolbar
button, not the retired header action group. Conversation Find lives at the
project overlay root outside the clipped content canvas, aligns its right edge
with the canvas gutter, and closes on view changes; action descriptors carry its expanded/control relationship.
Conversation and its five workspace tools share a switching group; only a tool's
last breadcrumb carries the close action. The old workspace tab row and tab ARIA
references are removed. `workspace-tools.js` still owns the single chat DOM and
subscriptions, suspending presentation while preserving drafts, buffers and scroll.
Editor-menu close notifications use the project-local `window` event port, matching
the listener owner. The scoped `document` port does not expose event dispatch.
Tool motion uses native element animations with cancellation epochs. Only the
departing paint is temporarily overlaid and inert; navigation and lifecycle switch
immediately. Global navigation/project deactivation cancels pending motion, and
Reduce Motion disables it without changing lifecycle behavior.
Device-local list preferences are keyed per view, with the legacy preference applied
only to Conversation. Mobile list/detail navigation overrides desktop collapse.
`workspace-tool-view.js` composes the same navigation list/detail primitives for
operations, processes and workflows; editor tabs and Git graph remain specialized.
The workspace host owns outer clipping/corners, and sidebar contents inherit their
sidebar surface instead of painting a separate background. Legacy conversation
controls stay inside one hidden container so they cannot become extra grid rows.

`session_query/operations/` is the shared read-only operation query owner, exposed
through the HistoryReader client port and `operations.list` / `operations.detail`
RPCs. The transport captures a reader and verifies selection under the Application
lock, then releases it before scanning committed journal facts. ReplayAdapter owns
vocabulary adaptation; records use the recorded turn and request sequence, pairing
results within a turn so reused wire call IDs do not cross conversations or rounds.
An unpaired result is unknown and never evidence of a successful invocation.

List cursors bind project, conversation, filters and a hashed committed prefix;
appended facts are available on refresh without destabilizing older pages. They
never create a writer or select an execution target. Queries enforce project trust,
source consistency and cancellation/deadline checks. Detail serialization reuses
history redaction and explicitly bounds previews. No durable vocabulary, checkpoint
or storage format is introduced. Existing `turn.changes` / `turn.restore` retain their
separate aggregate file-capture and permission contracts. Native capture remains
bound at run entry, prepared before atomic file publication and confirmed afterward;
run settlement clears the binding. Reading operation history cannot replay writes.

### Shared settings presentation

`settings-layout.js` owns category grouping, shared SVG decoration, category-name
filtering, content headers and in-memory per-category scroll positions. It also
provides the row/card presentation used by terminal and Web settings. Controls keep
their stable IDs, event owners and enclosing `data-settings-panel`; presentation
does not clone forms, issue configuration RPCs or implement save/permission policy.
Native desktop settings keep their existing dialog/control instance when composed
into the settings page. Repeated mounts preserve category selection and do not
create duplicate category controls.

`settings.css` owns the settings shell, rows, groups, switches and responsive
list/detail treatment; superseded modal and per-page overrides are removed from
the general workbench and development stylesheets. Theme tokens, system fonts,
shared navigation search and existing scrollbar behavior remain common resources.
Catalog labels are rebound without replacing decorated control nodes.

`settings-models.js` groups the existing `model.settings.get` preset catalog and
uses `model.preset.select` for explicit activation; it owns no vendor table, key
store or model policy. Preset selection fields are action arguments, separate from
the unsaved custom-profile editor. Native tray typography lives in the macOS client
adapter; menu actions, recent identity and persisted ordering remain in `tray.rs`.


### Contextual help ownership

`help-topics.js` is the static guide registry (topic, group, shared icon, existing
navigation target and related topics). `help-center.js` maps view IDs and settings
categories to guides, searches localized catalog text locally, and composes the
shared navigation frame. The application toolbar adds a secondary help intent to
each view without adding business actions to the help renderer. Every guide uses
the shared three-language catalogs, with no HTML injection or external content
fetch. New pages should supply a matching topic mapping and guide alongside their
user-facing behavior.

A return target captures page/tool identity, settings category, project and
conversation; it invokes existing navigation only while the owner still matches.
Topic routes reuse the existing page history and existing project workbench
lifecycle. Help creates no run, permission policy, persistence or RPC surface.
Notification permission controls remain owned by navigation settings, independently
of the guide renderer. Guide selection and scroll state live in the mounted
client; changing locale rebinds guide text and refilters the existing search.
