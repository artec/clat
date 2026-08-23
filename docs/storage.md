# Persistent state

CLAT creates:

```text
~/.clat/
├── config.json      # control-plane version sentinel (the only Fresh-commit file)
├── settings.json    # model state, named profiles, active-profile pointer (0600)
├── credentials.json # per-vendor remembered API keys (0600)
├── trust.json       # project trust decisions (canonical path → trustedAt)
├── mcp.json         (optional, user-managed MCP server definitions)
├── storages/
│   ├── workspace.json          # multi-workspace registry (DSH-isomorphic)
│   └── session_projcache.json  # session-list projection cache (pure cache)
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
Every frame is decompressed under a 64 MiB decoded budget, and streamed
record reads carry the same bound — a compressed bomb fails at the cap
with a budget-named error instead of exhausting memory.

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

## Control plane (the `~/.clat/` JSON family)

The control plane is a small family of JSON files, each wrapped in a
`unit` header (`{"name": …, "version": …}`) that gates loading
fail-closed: a file carrying a different unit version is refused, never
guess-read (no-migration policy — a version mismatch is an anomaly, not
a compatibility case).

| File | Content | Identity |
|---|---|---|
| `config.json` | the five-field version sentinel (control format v5) | publish marker |
| `settings.json` | model state + named profiles + the active-profile pointer | fact (salvage on tear) |
| `credentials.json` | per-vendor remembered API keys (INV-VK1..3) | fact (salvage on tear) |
| `trust.json` | canonical project path → trust timestamp | fact (salvage on tear) |
| `storages/workspace.json` | the multi-workspace registry | tables are facts; `global`/`sessionIds` are projections |
| `storages/session_projcache.json` | session-list rows for fast listing | pure cache (silently rebuilt) |

**Fact/projection split (the load-bearing wall).** A torn *fact* file is
never silently rebuilt: the remnant is renamed to
`<name>.torn-<date>`, a fresh empty state starts, and a mount diagnostic
says so loudly — workspace identity loss is unacceptable, so the remnant
stays for human salvage (sessions themselves re-adopt from their logs).
A torn or stale *projection* rebuilds from the fact source quietly: the
sessions directory always wins. On every mount the registry is
reconciled against it: session ids whose logs vanished are pruned
(order-preserving), unregistered sessions in a workspace's bucket are
adopted at the tail (deterministic order, header cwd must match the
workspace path), and `global.workspaceIds` is synced with the table
keys. Array order is display order — repair never reorders.

`storages/workspace.json` is field-for-field isomorphic to DSH's
(`unit{name:"workspace",version:2}` / `global` / `tables.workspaces`
with camelCase `path`/`title`/`sessionIds`/`createdAt`/`updatedAt`);
reading a real DSH file needs zero adaptation. Two deliberate CLAT
extensions ride along as additive fields: a per-workspace
`activeSessionId` (each project remembers its own current session —
switching between projects restores each one independently) and
`global.activeWorkspaceId`/`activeSessionId` (the restore-scene
groundwork for future desktop/PWA panels). CLAT never writes DSH's
storages. `path` is stamped as the `fs.realpath` canonical form, and
the session bucket derives from the same spelling by forward encoding.

The `settings.json` active-profile pointer keeps its B9 semantics: it
always names the row the single-slot state was installed from — profile
activation sets it, and any direct save (a preset switch, the classic
editor, or a `Shift+Tab` thinking change) clears it, so the `●` in
`/model` never claims a profile the running configuration no longer
matches. Vendor key memory lives in `credentials.json` (0600), no longer
as `vendor:` rows inside the profile table; profile names still reject
the reserved prefix.

All control-plane writes go through capability handles
(`cap-std`) with tmp+rename+fsync discipline and 0600 permissions; a
failed write rolls the in-memory state back so memory and disk never
diverge. One writer at a time: the kernel-level storage-root lease
covers processes, a facade-level mutex covers threads — the old
per-project revision CAS is structurally subsumed and gone.

## Startup state machine

Bootstrap performs a **zero-write** preflight, classifying `config.json`
plus the presence of the legacy database and new-family files into
Fresh / LegacySQLite / LegacyConfigOnly / Ready / Unsupported /
Inconsistent.

`authorize_and_mount` is the only path that persists trust: it acquires a
kernel-level storage-root lease (`flock` on Unix, taken on the deepest
existing ancestor directory with no lock files; a named mutex keyed by
the root's canonical identity on Windows — either way the kernel
releases it when the process dies), validates the session-root layout
read-only, then commits: a Fresh root publishes exactly one file (the
sentinel — everything else is born lazily), and a legacy v4 root has its
`clat.db` (plus `-wal`/`-shm` sidecars) renamed to
`clat.db.bak-<date>` before the new sentinel is written. **Zero
migration, zero deletion**: the old database is preserved byte-for-byte
as a corpse; the new control plane starts empty (re-approve trust once,
re-enter the model config once), while session logs — the fact source —
survive untouched and are adopted into the new registry as each project
is opened. An interrupted upgrade (db renamed, sentinel still v4)
re-runs idempotently.

The session-root preflight is a **strict full walk** (root → project
bucket → session directory → log file): any symlink anywhere on the path,
a regular file where a directory belongs, an unreadable directory, an
unknown entry (except a regular `.DS_Store`), or any mix of
`session.jsonl` and `session.jsonl.zstd` anywhere in the same session root
is a hard rejection — before any control-plane commit, Fresh or upgrade
alike. The same SessionId in different project buckets is legal; every
listing instead verifies that the physical bucket, encoded id directory,
and Header cwd/id are one consistent SessionKey.

Crash windows are covered by the fact/projection split rather than a
state matrix: a first durable batch that landed without its registry
update is adopted by mount-time reconciliation; a workspace pointer
naming a session that cannot load falls back to Fresh in memory with a
diagnostic, and the next successful command replaces it.

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
that cannot prove its directory sync returns Unknown (directory fsync
is a Unix discipline — on Windows the step is a no-op, since directory
handles cannot be flushed and NTFS metadata journaling covers dirent
durability), and an append whose
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
persisted (registering the workspace first if this is its first durable
session); the old session then quiesces and the in-memory target swap releases
the withheld resume seed without another fallible storage step — install
first performs a best-effort flush so the seed is durable before it
returns (a read barrier: a mount-time full-log stream must not mistake
our own seed for a foreign writer; a failed flush stays on the normal
retry lane and install remains infallible). A missing,
corrupt, or unsupported target fails before the pointer moves. A failed
pointer persistence leaves the old session untouched and closes the
unpublished writer;
an idempotent repair already completed while arming may remain, but no
resume seed or selection change is published. Detaching a session flushes, checkpoints, and
**joins** its writer thread — session switching does not leak threads
even when writes keep failing. Dropping a coordinator without an explicit
close is a safety-net retirement, not a detach: the last `Arc` falling
runs a best-effort close, so a forgotten quiesce can never leave an
immortal writer thread (a leaked writer held every parallel
`wait_for_writer_baseline` check red on slow CI, 2026-08-19).

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
