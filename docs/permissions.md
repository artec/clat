# Permissions

Every side-effecting operation must pass through the permission model.

## Permission modes (interactive)

The TUI exposes three user-switchable modes (DSH permission-presets adapted
to CLAT's tool-effect vocabulary). The active mode is shown at the **top
right of the input box**, symmetric with `Message` at the top left;
`Full Access` renders in warning yellow as its only risk cue.

| Effect | Read Only | Project Write (default) | Full Access |
|---|---|---|---|
| `Pure`, `Read`, `SessionWrite` | allow | allow | allow |
| `Write` | approval | **allow** | allow |
| `Execute`, `Network`, `ExternalRead`, `Destructive` | approval | approval | allow |

- The `Write` row is the point of `Project Write`: file edits (already
  capability-confined to the project root by cap-std) stop prompting;
  commands, network, and destructive tools still ask. `Read Only`
  differs from `Project Write` only in that file edits ask again.
- No mode produces a table-level deny — a refusal always comes from a
  human (approver), never from the mode itself.
- Switching takes effect at the **next permission check**; an in-flight
  approval is unaffected. The mode is **persisted per project** in
  `<storage root>/permission_modes.json` (a plain control-plane JSON
  file, kept out of the version-locked `clat.db` schema) and reloaded on
  startup — each project remembers its own mode. A missing or corrupted
  file degrades to the default (`Project Write`); a failed save never
  rolls back the in-process switch (it works until exit, with a notice).

Two switch paths:

1. **`/perm`** (alias `/permission`) — a mode picker popup (arrows +
   Enter, `Esc` cancels). This is the only way to *downgrade* (under
   `Full Access` no dialog ever appears). Selecting `Full Access` from
   another mode requires a confirmation step (risk text + a second
   `Enter`) — a cold switch has no pending call as context.
2. **Escalation keys in the permission dialog** — when a dialog is
   open, the action line additionally offers exactly the wider modes
   that would let **this specific call** run: `w` (switch to Project
   Write) for a `Write`-effect call under `Read Only`, and `f`
   (Full Access) for any gated call. Switching there still requires
   the full argument review (same gate as `Enter`/`y`), then switches
   the mode and allows the pending call in one action. The approver
   contract is unchanged — the frontend sets the shared mode cell and
   answers `Allow`. Modes that would still ask for this call are never
   offered (e.g. `Execute` under `Read Only` does not offer
   `Project Write`).

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

## Native write and execute tools

The side-effecting native tools and their guarantees:

| Tool | Effect | Guarantees |
|---|---|---|
| `write_file` | `Write` | project-relative only; parent creation, reads, temporary-file creation, and rename are all capability-relative to opened project directory handles, so symlink or directory-replacement races cannot redirect I/O outside the project; final symlink targets are rejected; atomic temp-file+rename — a failed write never leaves partial content; existing file permissions (mode bits) are preserved; content capped at 1 MiB |
| `edit_file` | `Write` | exact unique match required (zero or multiple matches error out, file untouched); a parent-directory lock serializes cooperating CLAT writers, and the commit re-verifies the read snapshot inside the same lock before rename — a competing CLAT modification surfaces as a conflict error instead of a silent overwrite |
| `run_command` | `Execute` | runs only in the canonical project root; always terminates (exit, bounded timeout, or `Esc` cancellation); Unix uses a dedicated process group and sends TERM followed by an unconditional group KILL after the grace period, even if the leader shell already exited; Windows uses a Job Object so termination covers `cmd.exe` and descendants; stdout/stderr each capped at 32 KiB **without changing command semantics** — output past the cap is drained and discarded, never closing the pipe on the command |

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
description (`ExternalRead`, `Network`, `Write`, or `Destructive`). They
are untrusted server hints and never produce the auto-allowed native
`Read` classification. Every MCP call therefore asks, with the full
argument review described above.
