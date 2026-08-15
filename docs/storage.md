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
| `model_state` | model configuration and provider runtime values (API key) |
| `sessions` | conversations, keyed by project root |
| `messages` | display text (user/assistant), per session |
| `message_items` | the full conversation context: user/assistant text, tool calls, tool results, provider state, reasoning — persisted in order |
| `input_history` | previously submitted inputs, per project |
| `trusted_projects` | directories the user has explicitly trusted |

The display `messages` table and the context `message_items` table are
kept in sync at the same lifecycle points; tool activity stays in the
status line rather than the chat panel.

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
