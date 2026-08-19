# Persistent state

CLAT creates:

```text
~/.clat/
├── config.json      # control-plane version sentinel (Ready commit marker)
├── clat.db          # control plane: model state, profiles, trust, workspace pointers
├── mcp.json         (optional, user-managed MCP server definitions)
└── sessions/        # DSH-compatible session logs (zstd-framed JSONL)
    └── --<project-key>--/<session-id>/session.jsonl.zstd
```

## Session facts

Everything a conversation **is** lives in one append-only log per session
(`docs/research/dsh-session-compatibility.md` pins the format; CLAT logs
are readable by DSH tooling and vice versa). Events are appended in
contiguous batches, every batch an independent zstd frame with a content
checksum; a torn tail frame is truncated and the open turn closed with
synthetic `tool/result` / `step/end` / `turn/end` events on recovery.

Reads never reconstruct state from raw events on the hot path: each
session folds into projections (surface, transcript, title, todo, stats,
compaction). Committed batches fold directly from the append path (no
per-flush re-read of the whole log); listing reads only the bounded first
frame/line of each log, whatever the body size, plus the bounded
checkpoint rows for titles/stats. Checkpoint files have an
8 MiB final-size cap; oversized derived units are omitted and rebuilt
from the authoritative log. Cold resume is single-pass (invariant R-1,
2026-08-19): the recovery scan's one physical stream feeds projection
folding, the transcript replay, and usage stats together — the resume
path itself never reads checkpoints, because the surface/transcript
units are deliberately unbounded and would force the checkpoint floor
back to zero anyway; only the `/resume` listing consumes checkpoint
rows. The model context is
derived from the *surface* projection (compaction replaces shadowed
ranges); the human-visible transcript is deliberately not shadowed, so
history stays readable after compaction. Reading a log is fail-closed:
unknown required events, retired event types, malformed payloads of the
vocabulary CLAT folds, and subagent/delegated/agent-preset headers are
rejected instead of folded with defaults; only `ignorable: true` unknown
events are skipped.

Sessions materialize lazily: `/new` writes nothing until the first prompt
reaches the journal. Input recall comes from the transcript projection.

## Control plane (`clat.db`)

| Table | Content |
|---|---|
| `clat_storage_meta` | control schema version + init id, matched against `config.json` |
| `model_state` | model configuration and provider-neutral credentials |
| `model_profiles` | named configuration snapshots + the active pointer |
| `trusted_projects` | directories the user has explicitly trusted |
| `project_workspace_state` | per-project current-session pointer (Fresh / Materializing / Session) with a revision for compare-and-set |

No session content, titles, or surface state is stored here.

## Startup state machine

Bootstrap performs a **zero-write** preflight: `config.json` and `clat.db`
are classified against the sentinel matrix (Fresh / PendingCommit / Ready /
Unsupported / Inconsistent). Old pre-release layouts — a `version: 3`
config, or a database with `sessions`/`messages`/`message_items`/
`input_history` tables — are refused with removal instructions; there is
no migration and no legacy reader.

`authorize_and_mount` is the only path that persists trust: it acquires a
kernel-level storage-root lease (flock on the deepest existing ancestor
directory — no lock files), validates the session-root layout read-only,
commits the control plane (temp DB + no-clobber link publish + config
sentinel), and mounts the Trusted Project scope holding the lease for its
lifetime. One CLAT process owns the storage root at a time; a second
process fails fast with a clear error.

The session-root preflight is a **strict full walk** (root → project
bucket → session directory → log file): any symlink anywhere on the path,
a regular file where a directory belongs, an unreadable directory, an
unknown entry (except a regular `.DS_Store`), or any mix of
`session.jsonl` and `session.jsonl.zstd` anywhere in the same session root
is a hard rejection — before any control-plane commit, Fresh
or PendingCommit alike. The same SessionId in different project buckets
is legal; every listing instead verifies that the physical bucket,
encoded id directory, and Header cwd/id are one consistent SessionKey.
The database half of a PendingCommit is additionally verified against
this build's complete schema-object DDL (including indexes, views, and
triggers), not just its table names.

Crash windows are covered by the state matrix: a database published
without its config re-runs the read-only preflight and idempotently
completes the config; a workspace pointer stuck in `Materializing` is
normalized to `Session` (log exists) or `Fresh` (it does not) on mount.

## Run-time persistence

A run produces two streams from one loop: `RunEvent`s for frontends and
`SessionEvent`s into the journal. The first batch (`turn/start` +
`user/message`) is flushed **before** the model is called; side-effecting
tools flush `approval/asked` before the human answers and the
`approval/decided` + `tool/call` group before execution; a successful
terminal `RunEvent` is published only after the closing `turn/end` is
durable — if that final flush fails, the frontend receives `RunFailed`
carrying the journal error instead, so the event stream and the
completion channel can never disagree. Writes go through a 200 ms
write-behind window whose flush drains to silence; commit outcomes are
three-state (NotCommitted / Committed / Unknown); a first-batch publish
that cannot prove its directory sync returns Unknown, and an append whose
file identity drifted from the prepared handle (external writer) returns
Unknown — both poison the session until a cold repair reopens it.
In-run steering journals as plain mid-turn `user/message` rows,
durable before the model request that consumes them. Each assistant
message carries the step's token accounting in the DSH `usage` field
(TokenUsage: `inputTokens` / `outputTokens` / optional
`cacheReadTokens` / `reasoningTokens`) when the adapter reported it —
the status bar's Cache/Context restore from these rows at startup in
the same streaming pass as the replay.

Session switching is two-phase: the target is first staged read-only
(admission + cold restore), then armed but kept unpublished while pending
torn-tail recovery, projection catch-up, and view construction finish.
Only after those fallible operations succeed is the workspace pointer
CASed; the old session then quiesces and the in-memory target swap releases
the withheld resume seed without another fallible storage step — install
first performs a best-effort flush so the seed is durable before it
returns (a read barrier: a mount-time full-log stream must not mistake
our own seed for a foreign writer; a failed flush stays on the normal
retry lane and install remains infallible). A missing,
corrupt, or unsupported target fails before the pointer moves. A lost CAS
race leaves the old session untouched and closes the unpublished writer;
an idempotent repair already completed while arming may remain, but no
resume seed or selection change is published. Detaching a session flushes, checkpoints, and
**joins** its writer thread — session switching does not leak threads
even when writes keep failing.

Full-log streams (replay) assume quiescent callers, which same-process
late writers can violate; the stream's stat→read→stat mismatch is
retried up to three times with locally rebuilt state before the error
surfaces (mirroring `read_stable`), and the mount-time `snapshot()`
reuses the replay produced while arming instead of streaming the log a
second time.

## Layering

Storage is assembled behind `SessionService` (use-case facade) and
`ControlStorage`. Frontends never see raw persistence: they use
`BootstrapApplication` (read-only preflight, `authorize_and_mount`) and
`TrustedProjectApplication` (sessions, model state, profiles, runs).
