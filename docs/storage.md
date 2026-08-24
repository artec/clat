# Persistent state

CLAT is local-first: model configuration, trust, session journals, projections,
extension configuration, and web pairing state live under `~/.clat` by
default. Frontends never write these files directly; the Application core owns
all durable state except explicitly user-managed extension manifests.

## File map

Files appear lazily, so a fresh installation may contain only a subset.

```text
~/.clat/
├── config.json                  # control-plane version sentinel
├── settings.json                # active model state and named profiles
├── credentials.json             # remembered per-vendor API keys (0600)
├── trust.json                   # canonical project path -> trusted timestamp
├── web-token                    # clat serve Bearer credential (0600)
├── dsh-last-session             # last session id opened by clat dsh
├── mcp.json                     # optional, user-managed MCP servers
├── plugins.json                 # optional, user-managed WASM components
├── plugin-grants.json           # hash- and directory-bound WASM write grants
├── plugin-store/
│   ├── registry.json            # atomic active/enabled package pointers (0600)
│   ├── staging/                 # inert, recoverable install transactions
│   └── artifacts/<id>/<digest>/ # immutable complete package trees
├── storages/
│   ├── workspace.json           # multi-project workspace registry
│   └── session_projcache.json   # rebuildable session-list cache
└── sessions/
    └── --<project-key>--/
        └── <encoded-session-id>/
            ├── session.jsonl.zstd   # authoritative DSH-compatible log
            ├── clat-checkpoint.json # bounded derived projection cache
            └── attachments/         # copied local image attachments
```

`mcp.json` and `plugins.json` are legacy declarative inputs written by the
user and override a same-id installed package. `plugin-store` is written only
by `clat plugin` while holding the same storage-root lease as a running CLAT
application. The other files are written through core storage helpers or, for
`dsh-last-session`, a small fail-soft client preference writer.

## Ownership and sensitivity

| File family | Authority | Failure policy |
|---|---|---|
| session log | authoritative conversation facts | fail closed; recover only a torn tail with explicit synthetic closure |
| settings, credentials, trust, workspace tables | control-plane facts | preserve torn remnant and start an empty replacement with a diagnostic |
| projection checkpoint/cache | derived | drop and rebuild from facts |
| `web-token` | local API credential | validate regular 0600 file; create/rotate atomically |
| extension manifests | user input | isolate configuration/plugin failure where possible |
| plugin registry | authoritative activation pointers | version/digest/signature mismatch fails closed; never reset or guess |
| plugin artifacts/staging | immutable code / uncommitted transaction bytes | verify complete tree before activation; stale staging is never executable |
| `dsh-last-session` | decorative client preference | missing/corrupt/oversized/symlink → ignore |

Back up `~/.clat` as a unit when conversation history and configuration both
matter. Copying only `storages/workspace.json` does not copy conversations;
copying only `sessions/` preserves conversation facts but not provider keys,
trust, or current selections.

## Session journals

Everything a conversation *is* is represented by one append-only event log.
CLAT's event types, headers, paths, and encoding are compatible with the DSH
session format.

`/new` may update the workspace's active-session pointer to Fresh, but it does
not create a session directory or log. The first prompt materializes the new
session.

### Physical encoding

The normal file is `session.jsonl.zstd`. Each committed batch is an independent
zstd frame with a content checksum. Independent frames make appending cheap and
limit crash damage to the final incomplete frame.

Every frame is decoded under a 64 MiB budget. Streamed record reads carry the
same bound, so a compressed bomb fails with a named error instead of consuming
unbounded memory.

The session root may contain uncompressed `session.jsonl` from a compatible
source, but one root cannot mix raw and zstd session encodings. Startup rejects
an encoding conflict before mounting storage.

### Event admission

Known durable event types are validated before append and again when read.
Malformed payloads, retired types, unsupported required headers, and unknown
required events fail closed. Unknown events are skipped only when explicitly
marked `ignorable: true`.

Adding a durable event is a four-part contract: catalog it, validate its
payload, fold it into projections/checkpoints, and prove live/replay parity.

### Crash recovery

On open, CLAT scans frames and events in order. A torn final frame is truncated
to the last durable boundary. If the crash left an open tool/step/turn, recovery
appends synthetic `tool/result`, `step/end`, and `turn/end` events so later
folds see a complete state machine.

Recovery does not guess through corruption in an earlier committed frame. A
corrupt or unsupported session fails before the active-session pointer moves.

## Projections and checkpoints

The journal folds into independent projections:

- model-context surface;
- human transcript;
- title;
- todo state;
- permission mode;
- usage/statistics;
- compaction state.

Committed batches fold directly from the append path; CLAT does not reread the
whole log after every write. The transcript remains complete after compaction,
while the surface shadows compacted ranges for the next model request.

`clat-checkpoint.json` caches bounded projection rows and has an 8 MiB final
size cap. Unbounded units are omitted and rebuilt from the log. Deleting a
checkpoint changes performance only, never semantics.

Cold resume is a single physical log pass. The recovery stream feeds
projection folding, transcript replay, and usage restoration together. Session
listing can use checkpoint rows and the bounded first log record instead of
decoding the conversation body.

`session_projcache.json` is a higher-level list cache. It is disposable and
silently rebuilt when stale or torn.

## Attachments

Before the first journal batch of a prompt, CLAT validates each local image and
copies it into that session's `attachments/` directory. The journal contains an
absolute attachment reference, not image bytes.

Because the copy happens before the durable user event, a failed import cannot
leave a journal entry referring to an attachment CLAT never stored. Deleting an
attachment later degrades replay/model input to a visible missing-file note.

## Control plane

Core JSON files carry a `unit` name/version header. A different version is
refused rather than guess-migrated.

| File | Content | Kind |
|---|---|---|
| `config.json` | five-field control-format sentinel | publication marker |
| `settings.json` | active model, named profiles, active-profile pointer | fact |
| `credentials.json` | per-vendor remembered API keys | fact |
| `trust.json` | canonical project path and trust time | fact |
| `storages/workspace.json` | workspace tables and current session pointers | facts plus projections |
| `storages/session_projcache.json` | session-list rows | pure cache |

### Fact versus projection

A torn fact file is never silently reconstructed from partial data. The remnant
is renamed to `<name>.torn-<date>`, a fresh empty unit is created when needed,
and mount diagnostics explain what was lost. The remnant remains for manual
salvage.

A torn projection cache is deleted or replaced from authoritative sources.
The `sessions/` directory wins over list caches and derived session-id arrays.

### Workspace registry

`storages/workspace.json` is field-for-field compatible with DSH's workspace
unit: `unit`, `global`, and `tables.workspaces` with camelCase path/title/
session/time fields. CLAT adds optional active-workspace and active-session
pointers.

Project paths are canonical real paths. On mount, reconciliation:

- prunes registry session ids whose physical logs vanished;
- adopts unregistered logs whose header cwd matches the workspace;
- preserves existing display order and appends adopted sessions
  deterministically;
- synchronizes global workspace ids with the table.

Each project remembers its own active session. Switching projects therefore
restores the conversation last selected in that project.

### Model settings and credentials

`settings.json` stores the effective single model slot plus named profiles. The
active-profile pointer is cleared whenever the effective configuration no
longer exactly matches the saved profile—preset switch, direct save, or runtime
reasoning change—so the model picker's `●` remains truthful.

Vendor preset keys live in `credentials.json` with 0600 permissions rather
than fake profile rows. Custom profile credentials remain part of their profile
configuration.

### Atomic writes and rollback

Control-plane writes use opened directory capabilities, temporary files,
rename, fsync, and 0600 mode where secrets are involved. A failed fact write
rolls the corresponding in-memory mutation back, keeping memory and disk
aligned.

A facade mutex serializes threads. A kernel storage-root lease serializes
processes.

## Web token and DSH preference

`clat serve` creates `~/.clat/web-token` only after the requested loopback port
binds successfully. Creation and `--rotate-token` use atomic replacement and
0600 permissions. `--token` bypasses this file for one process and never
modifies it. The token does not enter manifests, URLs, logs, or session events.

`dsh-last-session` contains one opaque session id, capped at 4 KiB. Reads reject
symlinks, non-files, invalid UTF-8, and oversized content. Writes are atomic
and fail-soft. This file is a presentation preference only; deleting it does
not affect the DSH host or any session.

## Startup state machine

`BootstrapApplication::open` performs a zero-write preflight and classifies the
storage root:

| State | Meaning | Result |
|---|---|---|
| Fresh | no supported control plane or legacy database | wait for authorization, then publish current sentinel |
| Ready | current sentinel and compatible layout | continue to trust decision |
| Legacy SQLite | supported old `clat.db` layout | preserve database and start current control plane on authorized mount |
| Legacy config only | old supported sentinel without database | start current control plane on authorized mount |
| Unsupported | known but unsupported older format | refuse with manual instructions |
| Inconsistent | mixed or contradictory files | refuse before writes |

Session-root preflight walks root → project bucket → session directory. It
rejects symlinks at every level, invalid bucket/session shapes, unreadable
directories, non-directory bucket members, non-regular CLAT-owned files, and a
raw/zstd encoding mix. Unknown non-symlink content *inside* a session directory
is left opaque for compatible DSH attachments, spill, or delegated state.

Only `authorize_and_mount` can acquire the root lease, persist trust, publish a
fresh sentinel, or perform a supported legacy cutover.

## Legacy SQLite cutover

CLAT does not translate the old SQLite control plane into the current JSON
family. On the supported v0.4–v0.8 cutover, `clat.db` and its WAL/SHM sidecars
are renamed to `clat.db.bak-<date>` and kept byte-for-byte. Then the current
sentinel is published.

The user re-enters model configuration and re-approves project trust. Existing
DSH-format session logs survive and are adopted when each project opens.

An interrupted rename-before-sentinel window is idempotent on the next mount.
Older SQLite formats that stored conversations in `sessions`/`messages` tables
are refused rather than silently losing or partially converting data.

## Runtime commit ordering

A run writes and publishes in a deliberate order:

1. `turn/start` + `user/message` are durable before the model request.
2. `approval/asked` is durable before waiting for a human.
3. `approval/decided` + `tool/call` are durable before side-effect execution.
4. Steering is durable before the model request that consumes it.
5. Closing events are durable before the frontend receives success.

Writes use a 200 ms write-behind window and drain to silence on explicit
flush. Commit outcomes distinguish NotCommitted, Committed, and Unknown. An
unknown directory-sync or file-identity result poisons further writes until a
cold reopen can establish a safe boundary.

## Session switching

Switching is two-phase:

1. Stage and validate the target read-only, including recovery, projection
   folding, and replay construction.
2. Persist the workspace pointer, quiesce the old session, install the prepared
   target, and publish its withheld resume seed.

Any target or pointer failure before commit leaves the old session active. A
detached session flushes, checkpoints, stops, and joins its writer thread.
Dropping a coordinator has a best-effort safety close, but normal Application
shutdown performs explicit closure and reports errors.

## Layering

`SessionService` owns session use cases and `ControlStorage` owns the JSON
control plane. `BootstrapApplication` exposes preflight/trust transition;
`TrustedProjectApplication` exposes session/model/run use cases. TUI, exec, and
serve clients never open these paths or infer state from file contents.
