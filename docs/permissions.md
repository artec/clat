# Permissions

Every side-effecting operation must pass through the permission model.

## Classification

Each tool declares a `ToolEffect`:

| Effect | SafeByDefault decision |
|---|---|
| `Pure`, `Read` | allow automatically |
| `Write`, `Execute`, `Network` | require approval |

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
- Long arguments scroll (`↑`/`↓` line by line, `PageUp`/`PageDown` by
  page, `Home`/`End` to the ends). **Allow is disabled until the final
  line has actually been visible** — jumping to `End` alone does not
  count; you must page through the content. `Esc`/`n` (deny) is always
  available.
- If the terminal is too small to show any argument lines, Allow stays
  disabled until the window is enlarged.

A denial is returned to the model as a structured tool error
(`ToolResult` with `is_error`), so the agent can adapt its approach
instead of the run aborting. The tool itself never executes — the
permission check happens before invocation.

## Design

The runtime core stays client-neutral: an `InteractivePermissionPolicy`
wraps the safe-by-default classification and resolves `Ask` decisions
through an injected approver closure. The TUI is one approver (a dialog
over a channel); headless clients can supply their own.

In non-interactive contexts (no approver), an unresolved `Ask` still
fails the run — there is nobody to answer it.

## MCP tools

Remote MCP tools cannot be statically classified, so they are always
treated as `Execute` — every call asks, with the full argument review
described above.
