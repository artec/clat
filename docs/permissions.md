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

CLAT deliberately has no "read-only shell command" allowlist. Without an
OS-level process sandbox, an apparently harmless command can contain a write,
network call, or destructive suffix. `run_command` therefore remains `Execute`
and asks in Project Write.

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
- `run_command` shows the command, working directory, and timeout;
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
it is the caller's responsibility to provide an external containment boundary.

### Server approval

`clat serve` publishes an `approval.requested` SSE frame. An authenticated
client answers through `approval.respond`; the first answer wins and late
answers receive `not-pending`. Cancellation, a ten-minute timeout, or losing
all event subscribers while a request is pending resolves it as deny.

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

## Read and write scope

Permission decides whether a call asks. Path scope independently decides what
the operation may address.

### Reads

Native read tools accept absolute paths in every mode. This follows the current
DSH-aligned rule that reading is not project-confined. Project-relative paths
still reject `..` traversal and use symlink-aware capability resolution.

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

This table is **not an OS process sandbox**. `clat exec --yes` also approves
`run_command`; that subprocess can use the operating-system account's normal
authority to read or write absolute paths, access the network, and inspect
inherited environment variables. The project-relative row constrains CLAT's
native write/edit path resolver only.

Project-relative writes bind all parent creation, inspection, temporary-file
creation, and rename operations to opened directory capabilities. Final
symlinks are rejected. Under Full Access, CLAT canonicalizes and opens the
absolute target parent before applying the same atomic discipline.

`run_command` always runs in the canonical project root. Full Access removes
its prompt but does not turn it into an arbitrary-working-directory tool. The
command can still name paths outside that cwd because no kernel sandbox is
installed.

## Native side-effecting tools

| Tool | Effect | Core guarantees |
|---|---|---|
| `write_file` | `Write` | content ≤1 MiB; atomic temp+rename; existing mode bits preserved; failed writes leave no partial target; capability-relative path operations |
| `edit_file` | `Write` | target/result ≤1 MiB; exactly one text match; cooperating writers serialized; source snapshot revalidated before atomic replace |
| `apply_patch` | `Write` | patch/target/result ≤1 MiB; one existing UTF-8 target; all exact hunks prevalidated; one snapshot-checked atomic commit; no add/delete/rename/multi-file v1 |
| `run_command` | `Execute` | canonical project cwd; bounded timeout; cancellation; whole process-tree termination; stdout and stderr each retained up to 32 KiB while excess is drained |

The edit lock coordinates CLAT writers, not arbitrary external editors.
Snapshot revalidation reports a conflict when a cooperating or visible
concurrent change occurs; no advisory filesystem lock can make unrelated
processes transactional.

Commands inherit the CLAT process environment. That is useful for normal
tooling and also means secrets exported in the launching shell are visible to
approved commands.

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
