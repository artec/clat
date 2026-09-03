# Permissions

CLAT separates three questions that are easy to conflate:

1. **May this project activate agent capabilities?** — project trust.
2. **May this tool call run without asking?** — permission mode and tool
   effect.
3. **Where may the operation reach?** — path/write scope and extension
   capabilities.

Every side-effecting tool call passes through the same core permission pipeline.
The TUI and PWA present decisions; they do not own the policy.

## Permission modes

Interactive local clients use three session-scoped modes:

| Tool effect | Read Only | Project Write (default) | Full Access |
|---|---|---|---|
| `Pure`, `Read`, `SessionWrite` | allow | allow | allow |
| `Write` | ask | allow | allow |
| `Network`, `ExternalRead` | ask | allow | allow |
| `Execute`, `Destructive` | ask | ask | allow |

Project Write is the daily-driver mode: file edits and network/search tools run
without interruption; commands and destructive operations still ask. Full
Access removes all tool prompts and unlocks absolute-path writes, so cold
switches require a separate warning confirmation.

Read Only does not create a hard deny. It asks before every side effect and
still confines approved writes to the project. A deny always comes from the
approver or from an unavailable approver, not from a table-level mode rule.

CLAT deliberately has no "read-only shell command" approval allowlist. An
apparently harmless command can contain a write, network call, or destructive
suffix, and Linux/Windows do not yet have a graduated sandbox provider.
`run_command`, `exec_command`, and `write_stdin` therefore remain `Execute` and
ask in Project Write even where macOS Seatbelt adds a second OS boundary.

### Session persistence

The active mode is a session property. Interactive switches append a DSH
`sandbox/mode` event:

```json
{"mode":"read-only"}
{"mode":"workspace-write"}
{"mode":"danger-full-access"}
```

The latest event wins. New sessions begin in Project Write, resuming restores
that session's own value, and mode never leaks between sessions. Sessions from
before the mode vocabulary existed also default to Project Write.

If journaling a mode switch fails, the in-process switch remains effective for
the current process and a notice reports the persistence failure. The retired
`permission_modes.json` file is not read.

Headless `clat exec` does not use this session-mode system. It always uses the
SafeByDefault classifier described below and does not append mode events.

### Switching modes

There are three interactive paths:

- **TUI `/perm` or `/permission`** — choose any mode. A cold switch to Full
  Access requires a second confirmation. This is also the normal downgrade
  path.
- **TUI approval escalation** — while reviewing one call, `w` switches to
  Project Write only if that mode would allow this effect; `f` switches to Full
  Access. The same action approves the pending call after its arguments have
  been reviewed.
- **PWA Workbench settings** — calls the core setter through
  `permission.set`. `danger-full-access` additionally requires
  `confirm: "danger-full-access"`; a checked browser control alone is not
  authority.

A switch affects the next permission check. It does not retroactively change
an approval already waiting on screen.

## Tool-effect classification

Each tool declares one `ToolEffect`:

| Effect | Meaning |
|---|---|
| `Pure` | deterministic computation without external reads or writes |
| `Read` | trusted native read capability |
| `SessionWrite` | CLAT-local session metadata only, such as `todo_write` |
| `ExternalRead` | extension-provided read capability |
| `Network` | read-oriented network access |
| `Write` | file or external-state mutation |
| `Execute` | process or command execution |
| `Destructive` | mutation with destructive or insufficiently declared semantics |

`SessionWrite` is narrow: a tool that also touches project files, processes,
or the network must not use it.

For `clat exec` and other classic headless clients, `SafeByDefault` allows only
`Pure`, `Read`, and `SessionWrite`; every other effect asks. This is identical
to the Read Only decision column, but it is not a journaled interactive mode.

## Interactive approval

When policy returns `Ask`, the run pauses before middleware or tool invocation
and emits a `PermissionRequest` containing the final tool name, effect, reason,
and complete arguments.

The TUI renders dangerous native calls as readable previews:

- `edit_file` shows the old and new snippets as a diff;
- `apply_patch` shows the complete patch JSON, including its single target and
  every hunk; v1 rejects add/delete/rename and multi-file forms;
- `write_file` shows the target and full content;
- `run_command` shows the command, working directory, timeout, sandbox request,
  and whether network is requested;
- other tools show formatted JSON plus all top-level field names.

Long previews scroll with arrows, PageUp/PageDown, Home, and End. Allow remains
disabled until the final line has actually been visible; jumping directly to
End does not count. If the terminal cannot display any argument line, the user
must enlarge it. Deny is always available.

An allow proceeds into middleware and invocation. A deny never invokes the
tool and returns a structured error result to the model, allowing the run to
adapt. Dropping a dialog disconnects its one-shot response channel and resolves
fail-closed instead of deadlocking shutdown.

### Headless approval

With terminal stdin, `clat exec` displays the request only while it is pending.
Input typed before the request is discarded. `y` + Enter allows; `Esc` or any
other answer denies. Ctrl-C resolves the wait and cancels the run.

With piped stdin, no person can answer. Every `Ask` becomes unavailable/denied
and the model receives the tool error. `--yes` installs an allow-all approver;
it does not replace sandbox policy. On macOS classic headless commands still
use the workspace-write Seatbelt profile by default. On Linux/Windows `auto`
is an explicitly reported no-enforcement fallback, so external containment is
still required for unattended `--yes` execution.

### Server approval

`clat serve` publishes an `approval.requested` SSE frame. An authenticated
client answers through `approval.respond`; the first answer wins and late
answers receive `not-pending`. Cancellation, a ten-minute timeout, or losing
all event subscribers while a request is pending resolves it as deny.

### WeChat approval

When `clat serve --im wechat` is enabled, an explicitly paired WeChat user can
answer permission requests for that user's mapped session. The chat projection
contains a random request ID, tool name, effect, and a bounded redacted reason.
It intentionally does not send full tool arguments, command lines, or sensitive
paths through the IM service. If that summary is insufficient, deny the request
and review the complete arguments in the TUI or Workbench.

Only one of these exact standalone messages is an answer:

```text
/allow <requestId>
/deny <requestId>
/always <requestId>
```

The request ID is scoped to the paired user; the first matching answer wins.
An ID belonging to another user, an unknown or expired ID, extra words, an
embedded command, or natural language such as “allow it” has no approval
authority. Run cancellation and the ten-minute deadline both resolve the wait
as deny.

`/allow` affects one pending call. `/always` is an explicit escalation: before
that call proceeds, CLAT switches the current session to Full Access through
the same core session-mode setter and journal path used by local clients. It
does not create a global allowlist. Removing the paired user or unbinding the
bot prevents later messages from approving or starting work; unbinding also
clears the old credential's replay and chat-mapping state.

## Project trust

Trust controls whether a repository may activate project-aware capabilities at
all. Until trust is accepted:

- no project file is read;
- no session or model credential is loaded into a trusted scope;
- no MCP subprocess or remote connection starts;
- no model request or native project tool can run.

Trust is remembered by canonical project path. The type boundary between
`BootstrapApplication` and `TrustedProjectApplication` prevents pre-trust code
from acquiring trusted services accidentally.

Trust does not mean every future tool call is approved. After mount, the active
permission mode and path scope still apply.

## Workflow and language-intelligence boundaries

Plan Mode is stricter than a prompt convention but narrower than an OS
sandbox. One frozen `ToolAccessPolicy` removes side-effecting schemas from the
model and also rejects forged/direct/plugin-host calls. It does not terminate or
re-sandbox already mounted extension code; those components keep their own
runtime boundaries.

The native `skill` loader is `Read`. Loading a `SKILL.md` body or referenced
resource never inherits Execute authority. A `requires-execution: true` skill is
model-visible only after the platform's required sandbox capability has
graduated, and its script still runs only through normal `exec_command` or
`run_command` as `Execute`, with the ordinary approval and ProcessService path.

The optional `lsp` tool is always `ExternalRead`. It is absent from Plan Mode and
may spawn only the executable chosen in the user-level `lsp.json`; the spawn
requires the project-read/temp-write OS sandbox with network disabled. If that
required provider is unavailable, CLAT fails before spawning the server.

Memory, goals, and subagents narrow authority independently of the current
permission mode:

- only direct user commands/Application APIs write `memory.json`; the model's
  `memory_search` surface is `Read`, bounded, and cannot promote project memory
  into user scope;
- `update_goal` is `SessionWrite`, uses expected-revision CAS, and cannot bypass
  a registered acceptance verifier. Arming continuation is user-only and
  process-local; each continued tool call still passes through the original
  run permission policy. The verifier constrains only model completion
  candidates submitted through `update_goal`. `/goal complete` is the user's
  explicit final authority: it may complete any non-complete current goal
  without satisfying the verifier, while still using the current revision/CAS
  transition and durable `goal/change` commit;
- `delegate_readonly` is `SessionWrite` because it spends model tokens and
  records parent-session provenance. It is absent until `/subagents on`.
  Children use a separate fixed access snapshot containing only
  project-relative `list_files`, `read_file`, and `search`; absolute paths,
  third-party `Read` tools, host sampling, approval, write, execute, network,
  interaction, LSP, memory, and recursive delegation are structurally absent.

The ordinary parent read tools still accept absolute paths as described below.
That ambient-read behavior is deliberately **not** inherited by a child.

## Read and write scope

Permission decides whether a call asks. Path scope independently decides what
the operation may address.

### Reads

Native read tools accept absolute paths in every mode. This follows the current
DSH-aligned rule that reading is not project-confined. Project-relative paths
still reject `..` traversal and use symlink-aware capability resolution.

Image authority is deliberately narrower than this ambient read rule.
`view_image` is a `Read` tool, but it is registered in a run's model-facing
catalog only when the frozen model route has a vision-capable image-input and
tool-result policy (officially-declared built-in presets, or a custom route
unlocked by a passing `/vision-probe`). Its argument must resolve through
exactly one of three
authorities: an attachment id reachable from the active session, a
project-relative no-follow path, or an unforgeable current-run scratch ref
minted by core. Absolute host paths are rejected even in Full Access, and Full
Access does not widen the visual catalog. A successful invocation returns a
typed image tool result to the provider; journal and client events retain only
the fenced reference and metadata, never the host path or image bytes.

PWA draft upload ids follow a different boundary. They are random, short-lived,
Bearer- and selection-generation-bound capabilities for pre-admission raw
files. `prompt.send` and `steer.send` may reference those ids, but the browser
cannot name a server path or turn an upload id into a durable attachment
without core validation and the journal commit point. Session/project switch,
token rotation, expiry, rollback, and unclaimed steering invalidate or release
the corresponding authority.

The consequence is intentional and important: Read Only means "ask before
side effects," not "the agent can read only this repository." Do not start CLAT
with a model or extension you would not trust with local readable data.

### Writes

`WriteScope` is derived from the same mode cell used by permission checks for
the native `write_file`, `edit_file`, and `apply_patch` tools:

| Client/mode | Writable paths |
|---|---|
| TUI/PWA Read Only | project-relative only, after approval |
| TUI/PWA Project Write | project-relative only |
| TUI/PWA Full Access | project-relative and absolute |
| `clat exec` | project-relative only, even with `--yes` |

This table governs CLAT's native write/edit/patch path resolver; command
confinement is a separate boundary. On macOS, classic headless, Read Only and
Project Write commands default to a functionally probed Seatbelt profile.
Classic/Project Write can write only the canonical project, `/tmp`, and the
process temporary directory; Read Only cannot write the project. Network is
denied unless the tool call explicitly sets `network: true`. Full Access is
deliberately unconfined.
The current profile intentionally permits account-readable filesystem reads,
inherits CLAT's environment, and permits Workspace Write commands to use those
temporary roots. It is graduated write/network confinement, not a
container or a secret filter.

Linux and Windows currently have no graduated command sandbox provider:
`sandbox: "auto"` reports `provider=none, enforcement=none`, while
`sandbox: "required"` fails closed. Therefore `clat exec --yes` on those
platforms still gives approved commands the operating-system account's normal
filesystem, environment and network authority.

Project-relative writes bind all parent creation, inspection, temporary-file
creation, and rename operations to opened directory capabilities. Final
symlinks are rejected. Under Full Access, CLAT canonicalizes and opens the
absolute target parent before applying the same atomic discipline.

`run_command` always runs in the canonical project root. `exec_command` accepts
only a project-relative existing directory. Both use the same ProcessService
and sandbox plan. `sandbox: "off"` is accepted only under interactive Full
Access; `required` refuses when the current platform/policy cannot provide the
claimed confinement.

## Native side-effecting tools

| Tool | Effect | Core guarantees |
|---|---|---|
| `write_file` | `Write` | content ≤1 MiB; atomic temp+rename; existing mode bits preserved; failed writes leave no partial target; capability-relative path operations |
| `edit_file` | `Write` | target/result ≤1 MiB; exactly one text match; cooperating writers serialized; source snapshot revalidated before atomic replace |
| `apply_patch` | `Write` | patch/target/result ≤1 MiB; one existing UTF-8 target; all exact hunks prevalidated; one snapshot-checked atomic commit; no add/delete/rename/multi-file v1 |
| `exec_command` | `Execute` | run-owned session; project-relative cwd; incremental stdout/stderr or real PTY; 256 KiB transient rings; sandbox facts; max 8 active jobs |
| `write_stdin` | `Execute` | poll/input/close/terminate one same-run session; ≤256 KiB per write with a 5 s backpressure deadline; characters redacted from durable `tool/call`; sensitive model input rejected |
| `run_command` | `Execute` | one-shot ProcessService wrapper; canonical project cwd; ≤600 s call timeout; owned process-group termination; 32 KiB stdout/stderr compatibility result |

The edit lock coordinates CLAT writers, not arbitrary external editors.
Snapshot revalidation reports a conflict when a cooperating or visible
concurrent change occurs; no advisory filesystem lock can make unrelated
processes transactional.

Commands inherit the CLAT process environment. That is useful for normal
tooling and also means secrets exported in the launching shell are visible to
approved commands even under a filesystem/network sandbox. Seatbelt is not an
environment-secret filter.

## MCP effects

MCP annotations are untrusted hints used only to choose a conservative effect:

| Annotation shape | CLAT effect |
|---|---|
| read-only, closed world | `ExternalRead` |
| read-only, open world | `Network` |
| non-read-only, non-destructive | `Write` |
| destructive or missing/ambiguous | `Destructive` |

An MCP server cannot claim the auto-allowed native `Read` effect. Under Read
Only every MCP call asks. Under Project Write, ExternalRead, Network, and Write
follow the mode table; Destructive still asks. Full Access skips the approval
dialog, but protocol resource limits and the per-run sampling budget remain.

For the subprocess environment, secret storage, and network threat model, see
[MCP security posture](mcp.md#security-posture).

## WASM capabilities and write grants

Permission mode expresses trust in the agent's requested operation. A WASM
component is third-party code and receives a separate capability boundary.

The project root is preopened read-only. A component declaring write-capable
tools can receive project write access in Project Write or Full Access, and
configured extra directories only in Full Access. Before those preopens become
read-write, the user approves a grant bound to:

- plugin name;
- component SHA-256;
- exact directory set.

Changing any member requires a new approval. Rejection keeps that run
read-only; a non-interactive run without a stored grant fails closed. Grant
records live in `~/.clat/plugin-grants.json` and can be revoked by deleting the
matching record. See [WASM plugins](wasm.md#filesystem-write-grants).

## Package capability review

`clat plugin install/update --accept-capabilities` is an install-time review,
not a replacement for runtime permission checks. It accepts the manifest's
static ceiling (tools, prompts, sampling, elicitation, host context and named
host tools). A later update that adds any ceiling entry stops until it is
accepted explicitly; same/subset updates and rollback to an already accepted
artifact do not widen authority.

Runtime calls still pass the ordinary permission policy, project fence,
cancellation and tool pipeline. WASM write preopens still require their
separate hash/directory-bound grant. For MCP packages, capability review cannot
sandbox arbitrary executable code; the package trust label and MCP security
posture remain visible distinctions. See [CLAT plugins](plugins.md).

## Core design

Each Run Scope creates `InteractivePermissionPolicy` around one classifier:

- `SafeByDefault` for classic/headless operation;
- `ModePolicy` for TUI and serve/PWA, reading a shared
  `Arc<RwLock<PermissionMode>>` on every check.

The same mode cell feeds `WriteScopeSource`, so prompt decisions and path
permissions change at the same call boundary. `escalation_targets` is the
single source of truth for which wider modes can allow a pending effect.

The execution order is fixed:

```text
final tool arguments
  → permission decision
  → ordered tool middleware
  → tool invocation
  → ordered post observers
```

This ordering is an architecture contract. Moving permission behind middleware
would let extension code observe or act on a call before the user authorizes
it. Frontend code may change presentation, but it must continue to inject the
approver and delegate all semantics to core.
