# Persistent state

CLAT creates:

```text
~/.clat/
├── config.json
└── clat.db
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

The display `messages` table and the context `message_items` table are
kept in sync at the same lifecycle points; tool activity stays in the
status line rather than the chat panel.

On Unix, CLAT attempts to create `~/.clat` with mode `0700` and the
bootstrap/database files with mode `0600`. Provider credentials are
currently persisted inside the local SQLite database so `/model`
survives restarts; the database is not application-level encrypted yet.
