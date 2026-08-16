# Permissions

Every side-effecting operation must pass through the permission model.

## Classification

Each tool declares a `ToolEffect`:

| Effect | SafeByDefault decision |
|---|---|
| `Pure`, `Read` | allow automatically |
| `Write`, `Execute`, `Network`, `ExternalRead`, `Destructive` | require approval |

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
trust gate), and every call still asks. Environment variables are
inherited from the CLAT process — a command sees what you would see.

The edit lock coordinates CLAT writers that follow this protocol. It is not
a transaction against arbitrary external editors: filesystem locks are
advisory on some platforms, so a non-cooperating process can still change the
same entry. Capability-relative I/O still prevents such a process from using
path replacement to redirect CLAT's write outside the project.

## Design

The runtime core stays client-neutral. A built-in Project plugin provides the
permission-policy factory; each Run Scope receives a `PermissionApprover` port
from its client and creates an `InteractivePermissionPolicy` around the
safe-by-default classifier. The TUI implementation is only a dialog adapter
over a channel. Tests, future headless clients, and desktop clients can inject
their own approver without importing terminal code or implementing permission
semantics.

An approver must not retain its own response sender while waiting. The TUI
creates one response channel per request; dropping the dialog disconnects the
receiver and resolves to `Deny`, so application shutdown cannot deadlock on an
orphaned permission prompt.

In non-interactive contexts (no approver), an unresolved `Ask` still
fails the run — there is nobody to answer it.

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
