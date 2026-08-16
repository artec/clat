# Persistent state

CLAT creates:

```text
~/.clat/
├── config.json
├── clat.db
└── mcp.json        (optional, user-managed MCP server definitions)
```

`config.json` is intentionally only a small bootstrap file containing
the storage version and the SQLite database filename. The SQLite
database is the source of truth:

| Table | Content |
|---|---|
| `model_state` | model configuration and provider-neutral credential values (API key) |
| `sessions` | conversations, keyed by the canonicalized project root (symlinked and real paths share a session; legacy keys are migrated in place on open) |
| `messages` | display text (user/assistant), per session |
| `message_items` | the full conversation context: user/assistant text, tool calls, tool results, provider state, reasoning — persisted in order |
| `input_history` | previously submitted inputs, isolated per session |
| `trusted_projects` | directories the user has explicitly trusted |

The display `messages` table and the context `message_items` table are
kept in sync at the same lifecycle points; tool activity stays in the
status line rather than the chat panel.

Storage is assembled once, behind a shared core backend. Bootstrap Scope
registers only the narrow `TrustStore`; Session and Config stores are not
registered until a successful transition to Trusted Project Scope. Frontends
never open the database or receive a raw store. They use
`BootstrapApplication` for trust and `TrustedProjectApplication` for session,
history, profile, and model-state use cases.

The static-plugin migration does not change the schema or persisted JSON
contracts. `ProviderCredentials` remains encoded as the legacy JSON string
array, and `ModelProtocol` values retain their previous representation. The
storage layer no longer depends on concrete Provider runtime implementations.

On Unix, CLAT attempts to create `~/.clat` with mode `0700` and the
bootstrap/database files with mode `0600`. Provider credentials are
currently persisted inside the local SQLite database so `/model`
survives restarts; the database is not application-level encrypted yet.

## Integrity guarantees

- The `database` field in `config.json` only accepts a bare file name —
  absolute paths, separators, `..`, and Windows drive prefixes are
  rejected, so a tampered bootstrap cannot point SQLite (and its chmod)
  outside `~/.clat`.
- The storage root and the database file must not be symbolic links;
  SQLite is opened with `SQLITE_OPEN_NOFOLLOW` so the final open itself
  refuses to follow a link.

## Session lifecycle

A conversation is in one of three states:

```text
absent      no database row (fresh `/new`, or never opened)
live        row exists, zero chat content
persistent  row exists, ≥1 chat item (message or message_items row)
```

Invariants (tests derive from these, not from the code):

- **INV1 — content survives everything.** A persistent session is never
  automatically deleted or archived. Exit, `/resume` away, restart: it
  stays resumable. `archived = 1` is reserved for an explicit user
  action (no automatic path may set it).
- **INV2 — emptiness never persists.** "Empty" means **chat history**
  is empty (no `messages`, no `message_items`). Input history is recall
  convenience and never counts toward content. Leaving or exiting a
  live session deletes the row (and its input history) physically.
- **INV3 — lazy creation.** `/new` writes nothing. The row appears only
  when the first model-bound input is submitted; command input
  (`/help`, `/model`, …) never creates a session.
- **INV4 — history is session-scoped.** Input history belongs to one
  session and is deleted with it; sessions never see each other's
  inputs.
- **INV5 — reopening follows the last *opened* session.** Resuming a
  session read-only counts as opening it (the resumed session is
  touched and becomes the startup conversation, and sorts first in
  `/resume`). Known boundary: "last opened" is derived from a
  second-granularity timestamp — a resume in the same second as
  another session's write loses to it, and an explicit `/new` followed
  by exit restores the previously opened session rather than a fresh
  start. The exact model would be a persisted per-project
  last-opened pointer; adopt it only if this boundary ever bites.

## Run persistence boundary

Application owns run persistence. It appends the user display message and
`ModelItem` before starting the core worker. On success it persists assistant
display text and every newly produced item before publishing completion. On
failure or cancellation it performs the same best-effort persistence for
partial text/items and reports any persistence failure alongside the run
failure instead of silently discarding it. `RunEvent` remains observational;
the TUI never writes completion state.

Usage stays in the existing run result/event contract; this migration does not
add a usage column or otherwise change the database schema.
