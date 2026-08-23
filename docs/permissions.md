# Permissions

Every side-effecting operation must pass through the permission model.

## Permission modes (interactive)

The TUI exposes three user-switchable modes (DSH `sandbox/mode` adapted to
CLAT's tool-effect vocabulary). The active mode is shown at the **top
right of the input box**, symmetric with `Message` at the top left;
`Full Access` renders in warning yellow as its only risk cue.

| Effect | Read Only | Project Write (default) | Full Access |
|---|---|---|---|
| `Pure`, `Read`, `SessionWrite` | allow | allow | allow |
| `Write` | approval | **allow** | allow |
| `Network`, `ExternalRead` | approval | **allow** | allow |
| `Execute`, `Destructive` | approval | approval | allow |

- The project-write story is one sentence: **file edits, reads, and
  network/search tools run free; commands and destructive tools still
  ask** — exactly the two operation classes CLAT cannot contain (it has
  no OS-level sandbox; see the deviations in
  `docs/research/dsh-permission-gating.md`). `Read Only` re-adds the
  prompt on every side effect; its decisions are identical to the
  headless `SafeByDefault` column.
- `Network` / `ExternalRead` auto-allow under `Project Write` follows
  DSH: network access is explicitly outside its sandbox vocabulary and
  MCP tools are ungated there. The only remaining ask surfaces for MCP
  under `Project Write` are destructive-annotated tools.
- No mode produces a table-level deny — a refusal always comes from a
  human (approver), never from the mode itself.
- Switching takes effect at the **next permission check**; an in-flight
  approval is unaffected.

**The mode is a session property** (DSH `sandbox/mode` journal events,
latest-wins): every switch appends a `sandbox/mode` event
(`{"mode": "read-only" | "workspace-write" | "danger-full-access"}` —
the DSH vocabulary, so CLAT and DSH session logs stay interchangeable)
to the active session's journal; a session records its birth mode as
its very first event. Resuming a session restores **its own** mode;
`/new` starts at the default (`Project Write`); sessions created before
the mode system existed (no mode events) fall back to the default — a
mode never leaks across sessions. Restarting restores the mode because
the workspace auto-resumes the pinned session and its journal. A failed
journal write never rolls back the in-process switch (it works until
exit, with a notice). `clat exec` sessions never journal mode events.
v0.7.0's per-project `permission_modes.json` is retired — the leftover
file is no longer read and can be deleted by hand.

Two switch paths:

1. **`/perm`** (alias `/permission`) — a mode picker popup (arrows +
   Enter, `Esc` cancels). This is the only way to *downgrade* (under
   `Full Access` no dialog ever appears). Selecting `Full Access` from
   another mode requires a confirmation step (risk text + a second
   `Enter`) — a cold switch has no pending call as context.
2. **Escalation keys in the permission dialog** — when a dialog is
   open, the action line additionally offers exactly the wider modes
   that would let **this specific call** run: `w` (switch to Project
   Write) for a `Write`, `Network`, or `ExternalRead` call under
   `Read Only`, and `f` (Full Access) for any gated call. Switching
   there still requires the full argument review (same gate as
   `Enter`/`y`), then switches the mode and allows the pending call in
   one action. The approver contract is unchanged — the frontend sets
   the shared mode cell and answers `Allow`. Modes that would still ask
   for this call are never offered (e.g. `Execute` under `Read Only`
   does not offer `Project Write`).

The active mode is also injected as a one-line note into the system
instructions at run start (the model knows the approval boundary
before it tries), and every `Ask` reason names the current mode.

## Classification

Each tool declares a `ToolEffect`:

| Effect | SafeByDefault decision (headless / `clat exec`) |
|---|---|
| `Pure`, `Read`, `SessionWrite` | allow automatically |
| `Write`, `Execute`, `Network`, `ExternalRead`, `Destructive` | require approval |

`SafeByDefault` (ask for every side effect) remains the delegate for
`clat exec` and other headless clients — the three-mode table is the
interactive frontend's system and its decisions are identical to the
`Read Only` column.

`SessionWrite` is for tools that only mutate CLAT-local session metadata
(currently `todo_write`). The exemption comes from the effect
classification — a tool that also touches files, processes, or the
network must not declare it.

CLAT deliberately does **not** classify shell commands (no read-only
command allowlist). DSH solved auto-running `bash` with OS-level
sandboxes (Seatbelt / bwrap / Landlock); without a sandbox, a
command allowlist would be a lie — `rm` hiding behind `ls &&` is one
character away. Under `Project Write`, `run_command` keeps asking;
automation-heavy work should switch to `Full Access` knowingly.

## Interactive approval

When a run requests a side-effecting tool, CLAT pauses the model loop and
shows a permission dialog instead of aborting:

```text
┌─ Permission ──────────────────────────────────────┐
│ Permission required                               │
│                                                   │
│ tool:      write_file                             │
│ effect:    writes files                           │
│ reason:    tool `write_file` can cause side effects│
│ fields:    path, content                          │
│ arguments:                                        │
│   {                                                │
│     "path": "notes.txt",                          │
│     "content": "…"                                │
│   }                                                │
│ arguments lines 1–4 of 4 · ↑/↓ PgUp/PgDn Home/End │
│                                                   │
│ Enter / y — allow      ·      Esc / n — deny      │
└───────────────────────────────────────────────────┘
```

- The `fields` line lists every top-level argument key, so dangerous
  targets (a `command`, a `path`, a URL) cannot hide at the bottom of a
  long JSON payload.
- `write_file`, `edit_file`, and `run_command` render a readable
  preview instead of raw JSON: `edit_file` shows the `- old_str` /
  `+ new_str` diff, `write_file` shows the target path and full
  content, `run_command` shows the command line and its working
  directory / timeout. The preview is the reviewed content — the same
  scroll-and-unlock rules apply to it.
- Long arguments scroll (`↑`/`↓` line by line, `PageUp`/`PageDown` by
  page, `Home`/`End` to the ends). **Allow is disabled until the final
  line has actually been visible** — jumping to `End` alone does not
  count; you must page through the content. `Esc`/`n` (deny) is always
  available.
- If the terminal is too small to show any argument lines, Allow stays
  disabled until the window is enlarged.
- Preview wrapping uses the dialog's actual rendered width (84% of the
  current terminal), so a narrow terminal cannot horizontally clip a long
  command tail while still counting that line as reviewed.

A denial is returned to the model as a structured tool error
(`ToolResult` with `is_error`), so the agent can adapt its approach
instead of the run aborting. The tool itself never executes — the
permission check happens before invocation.

## Sandbox scope (reads free, writes fenced)

The permission table decides *whether* a call asks; the path fence
decides *where* I/O may land. Two layers, aligned with DSH's scope:

- **Reads are never confined** (`read_file`, `list_files`, `search`
  accept absolute paths anywhere on disk, in every mode — DSH: "every
  mode permits reading"). Project-relative paths keep their stricter
  in-process discipline (no `..` traversal, symlink-aware handle
  resolution).
- **Writes are fenced by `WriteScope`**, resolved from the same shared
  mode cell the permission check reads: under `Read Only` /
  `Project Write` (and always for `clat exec`) only project-root
  relative paths are writable — absolute paths fail with a clear
  "only writable under Full Access" error; under `Full Access`,
  absolute paths anywhere are writable, and the atomic-write
  discipline (temp file + rename bound to the target parent's opened
  directory handle) is preserved unchanged.

## Native write and execute tools

The side-effecting native tools and their guarantees:

| Tool | Effect | Guarantees |
|---|---|---|
| `write_file` | `Write` | project-relative by default, absolute paths only under Full Access (see Sandbox scope); parent creation, reads, temporary-file creation, and rename are all capability-relative to opened directory handles (the project root, or the canonicalized target parent under Full Access), so symlink or directory-replacement races cannot redirect I/O outside the intended root; final symlink targets are rejected; atomic temp-file+rename — a failed write never leaves partial content; existing file permissions (mode bits) are preserved; content capped at 1 MiB |
| `edit_file` | `Write` | same fence as `write_file`; exact unique match required (zero or multiple matches error out, file untouched); a parent-directory lock serializes cooperating CLAT writers (flock on Unix; a named mutex keyed by the directory's kernel identity on Windows), and the commit re-verifies the read snapshot inside the same lock before rename — a competing CLAT modification surfaces as a conflict error instead of a silent overwrite |
| `run_command` | `Execute` | runs only in the canonical project root; always terminates (exit, bounded timeout, or `Esc` cancellation); when the shell exits but background descendants keep holding the output pipes, a short drain grace follows and the whole group is then terminated so the call cannot hang; Unix uses a dedicated process group and sends TERM followed by an unconditional group KILL after the grace period, even if the leader shell already exited; Windows uses a Job Object so termination covers `cmd.exe` and descendants; stdout/stderr each capped at 32 KiB **without changing command semantics** — output past the cap is drained and discarded, never closing the pipe on the command |

Write and execute tools are registered only for trusted projects (the
trust gate); whether a call asks is decided by the permission mode
(see the table above). Environment variables are
inherited from the CLAT process — a command sees what you would see.

The edit lock coordinates CLAT writers that follow this protocol. It is not
a transaction against arbitrary external editors: filesystem locks are
advisory on some platforms, so a non-cooperating process can still change the
same entry. Capability-relative I/O still prevents such a process from using
path replacement to redirect CLAT's write outside the project.

## Design

The runtime core stays client-neutral. A built-in Project plugin provides the
permission-policy factory; each Run Scope receives a `PermissionApprover` port
from its client and creates an `InteractivePermissionPolicy` around a
classifier delegate. Which delegate depends on the bootstrap mode:

- `ModeSource::Classic` → `SafeByDefault` (headless `clat exec`; behavior
  byte-identical to before the mode system existed),
- `ModeSource::Shared(cell)` → `ModePolicy`, which reads a shared
  `Arc<RwLock<PermissionMode>>` cell on every check (mode switches are
  effective immediately, `escalation_targets(mode, effect)` is the single
  source for which escalation keys a dialog offers).

The write tools' path fence resolves from the same cell through
`WriteScopeSource` (`mode_write_scope`: Full Access → `Unrestricted`,
otherwise `ProjectRoot`; exec is pinned to `ProjectRoot`) — the
permission check and the path fence read the mode at the same call
boundary.

The TUI implementation is only a dialog adapter over a channel. Tests,
future headless clients, and desktop clients can inject their own approver
without importing terminal code or implementing permission semantics.

An approver must not retain its own response sender while waiting. The TUI
creates one response channel per request; dropping the dialog disconnects the
receiver and resolves to `Deny`, so application shutdown cannot deadlock on an
orphaned permission prompt.

In non-interactive contexts (no approver), an unresolved `Ask` still
fails the run — there is nobody to answer it.

`clat exec` is the first non-TUI approver and follows that rule: when
stdin is a pipe it resolves every `Ask` to `Deny` with a reason the model
can read, so scripts and CI fail closed without failing the run. `--yes`
replaces the approver with allow-everything and is the only
non-interactive way to approve side effects.

With a terminal stdin, the runner asks through a **request-scoped input
port**: the answer is only read while a concrete permission request is
displayed, keys typed before the prompt appeared are discarded (a stale
`y` cannot approve a future call), `y`+`Enter` allows, `Esc` or anything
else denies, and a single Ctrl-C interrupts the wait (resolving to deny
and unwinding the run). The port itself is frontend-neutral; the raw-mode
terminal adapter lives at the process boundary in `main.rs`, and desktop
or IDE clients can supply their own dialog implementation without
touching permission semantics.

The execution boundary is deliberately ordered:

```text
final Tool arguments
  → PermissionPolicy decision
    → ordered Tool middleware
      → Tool::invoke
    → ordered post observers
```

Middleware cannot see or execute an `Ask`/`Deny` call before approval. A denial
therefore produces the existing structured `ToolResult` without entering
middleware or the Tool. Middleware and observers are static plugin
contributions with scope-owned leases; they are frozen before runs and revoked
during reverse teardown.

## MCP tools

Remote MCP tool annotations are used only to improve the permission
classification (`ExternalRead`, `Network`, `Write`, or `Destructive`).
They are untrusted server hints and never produce the auto-allowed
native `Read` classification. Under `Read Only` every MCP call asks,
with the full argument review described above; under `Project Write`,
readOnly-annotated tools (`ExternalRead` / `Network`) run without
prompting (DSH: MCP is ungated), while destructive-annotated tools —
including servers that omit annotations, which default to destructive —
still ask.

## WASM write grants

Permission modes express how much you trust the *agent*; WASM
components are third-party code, so their filesystem write access has a
separate, explicit approval bound to the component hash and the exact
directory set — a mode like Full Access no longer hands write access to
globally installed components silently. Rejections downgrade that run
to read-only; headless runs fail closed without a recorded grant. See
`docs/wasm.md` › *Filesystem write grants*.
