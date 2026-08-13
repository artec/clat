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
│ arguments:                                        │
│   {                                                │
│     "path": "notes.txt",                           │
│   }                                                │
│                                                   │
│ Enter / y — allow      ·      Esc / n — deny      │
└───────────────────────────────────────────────────┘
```

- `Enter` or `y` — allow this single call and resume the run
- `Esc` or `n` — deny

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
