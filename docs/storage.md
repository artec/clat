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
├── memory.json                  # explicit user/project knowledge (0600)
├── trust.json                   # canonical project path -> trusted timestamp
├── web-token                    # clat serve Bearer credential (0600)
├── dsh-last-session             # last session id opened by clat dsh
├── mcp.json                     # optional, user-managed MCP servers
├── lsp.json                     # optional, user-managed read-only LSP servers
├── skills/<name>/SKILL.md       # optional user-layer instruction bundles
├── plugins.json                 # optional, user-managed WASM components
├── plugin-grants.json           # hash- and directory-bound WASM write grants
├── plugin-store/
│   ├── registry.json            # atomic active/enabled package pointers (0600)
│   ├── staging/                 # inert, recoverable install transactions
│   └── artifacts/<id>/<digest>/ # immutable complete package trees
├── market-cache/<origin-hash>/  # signed index + signature; reverified on use
├── plugin-market-staging/       # inert remote download/unpack transactions
├── storages/
│   ├── workspace.json           # multi-project workspace registry
│   └── session_projcache.json   # rebuildable session-list cache
└── sessions/
    └── --<project-key>--/
        └── <encoded-session-id>/
            ├── session.jsonl.zstd   # authoritative DSH-compatible log
            ├── clat-checkpoint.json # bounded derived projection cache
            └── attachments/
                ├── .orphan-sweep-cursor-v1 # private bounded-GC progress
                ├── blobs/<sha256>   # immutable normalized PNG/JPEG bytes
                └── staging/         # unpublished admission transactions
```

`mcp.json` and `plugins.json` are legacy declarative inputs written by the
user and override a same-id installed package. `plugin-store` is written only
by `clat plugin` while holding the same storage-root lease as a running CLAT
application. The other files are written through core storage helpers or, for
`dsh-last-session`, a small fail-soft client preference writer.
Market cache bytes are not trusted merely because they are local: every online
or offline use rechecks the embedded public-key signature and index expiry.
Market staging is protected by the package-store storage-root lease during
installation and never participates in runtime discovery.

## Ownership and sensitivity

| File family | Authority | Failure policy |
|---|---|---|
| session log | authoritative conversation facts | fail closed; recover only a torn tail with explicit synthetic closure |
| settings, credentials, trust, workspace tables | control-plane facts | preserve torn remnant and start an empty replacement with a diagnostic |
| projection checkpoint/cache | derived | drop and rebuild from facts |
| `web-token` | local API credential | validate regular 0600 file; create/rotate atomically |
| `memory.json` | authoritative explicit knowledge | version/CAS/path validation fail closed; never inferred from model output |
| extension manifests | user input | isolate configuration/plugin failure where possible |
| plugin registry | authoritative activation pointers | version/digest/signature mismatch fails closed; never reset or guess |
| plugin artifacts/staging | immutable code / uncommitted transaction bytes | verify complete tree before activation; stale staging is never executable |
| market cache/staging | public signed metadata / uncommitted downloaded code | signature+expiry checks on every read; only a committed package registry can activate code |
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

`request/header` is also the durable model-input witness. Besides provider,
model, system text and tool schemas, CLAT records
`clatInstructionContext` with the active project-instruction digest and bounded
source path/scope/digest rows. Its `imageProjection` section freezes the model
route, image policy, and estimator/calibration/encoder versions used for the
run. A successful file tool can cause a `change` header before the next model
request; resume restores those scopes and rereads the current files instead of
trusting cached repository text.

Process stdout/stderr/PTY rings and stdin are intentionally not durable state.
Process `tool/result` events retain only byte counts, terminal/truncation and
sandbox metadata plus an explicit output-omitted marker; raw output remains in
the bounded live model result. `write_stdin` replaces `chars` in durable
`tool/call` arguments with `chars_bytes` and a redaction marker; permission
review and the live invocation still receive the complete arguments. Command
text is limited to 64 KiB; an oversized invalid call is journaled as byte count
plus an omitted marker rather than copying the rejected body. Process
completion notices contain terminal metadata only and are not session facts.
Consequently, a run never resumes a live process: run or Application teardown
owns and reaps it.

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

Before the first journal batch of an image-bearing prompt, core admission
validates magic bytes and decoded dimensions, fully decodes the source, applies
orientation, strips metadata, bounds the long edge, and deterministically
normalizes it to PNG or JPEG. Every admission source is opened final-component
no-follow, proved to be a single-link regular file from that descriptor, and
held open from batch preflight through bounded read and normalization; path
replacement cannot change which inode is committed. Browser raw-staging
prevalidation uses that same descriptor for metadata, header policy, and full
decode instead of splitting a path check from a later decoder open. Inputs
already read through a project/scratch capability enter the shared
normalize/publish transaction directly; they are never written out and then
reopened through an ambient display path. New bytes are published under
`attachments/blobs/<sha256>` through a 0600 staging file, file sync, atomic
rename, and directory sync. Existing digest targets are accepted only after a
no-follow descriptor check of regular-file type, single-link ownership,
length, and bytes; a symlink, multiply-linked file, or conflicting target is
never followed or overwritten. Production opens the attachment domain relative
to the already-held session-directory capability, then retains handles for the
attachment, blob, and staging directories. Publication, rollback, GC, and
cursor updates are relative to those handles; replacing an ambient
session/root spelling cannot redirect a write. The bridge-era absolute
provider path is checked again before and after admission, so a namespace
replacement fails the batch and capability-relative rollback removes its
unpublished artifacts. Opening a store likewise rejects a pre-existing
symlink/non-directory at the attachment root or either core-owned child before
creating later namespace entries through it.

The durable content block stores an opaque attachment id plus MIME, dimensions,
byte count, and display metadata. It stores neither image bytes nor a host
path. Provider projection resolves that id through the current session's store
and uses no-follow reads; session-blob paths open the platform/storage prefix
once, then walk every session-owned component from `sessions` downward with
held no-follow directory handles, closing subtree replacement between the
surface fence and provider projection. The PWA download path opens the blob by
id through the active session/store capabilities. Deleting or corrupting the
blob therefore fails closed instead of falling back to the original user path.
Reads also require a single-link file so a second hardlink name cannot mutate
the inode behind a durable attachment id. For a new `blobs/<sha256>` entry,
provider projection hashes the bounded bytes before constructing a data URL.
The PWA reader copies
the no-follow source once into a bounded immutable snapshot and authenticates
that exact snapshot before emitting headers or body bytes; mutation of the
store inode after verification cannot alter the response. It still writes the
snapshot to the socket in 64 KiB chunks, and the four-download permit caps
concurrent snapshot ownership. Both paths also compare the durable MIME claim
with image magic before constructing a Data URL or emitting `Content-Type`; a
valid digest can never relabel PNG bytes as JPEG (or vice versa). A same-length
in-place mutation therefore fails closed. Before the PWA reader is exposed,
new content-addressed descriptors also bind their durable byte count to the
verified file length and their width/height to dimensions parsed from that same
bounded authenticated snapshot. Tool-result metadata therefore cannot drift
from the blob that the browser receives. Pre-MM-2 flat
filenames do not claim a digest and retain their fenced compatibility behavior.
Those flat files remain readable only through the fenced legacy bridge and are
never new-write targets.

The whole batch is validated before publication. A failure removes this
batch's staging files and only blobs newly published by this batch, so the
journal cannot refer to a half-admitted image and deduplicated historical blobs
survive rollback. All attachment-store handles for one session share a single
admission publication lane: a successful concurrent batch cannot observe a
blob that another batch still owns and may remove during rollback. Session open
performs a bounded sweep: unreferenced staging or blob entries older than 24
hours may be reclaimed, while referenced blobs and legacy flat files are
preserved. The mark pass covers direct user/assistant image blocks and images
nested in durable tool results, so `view_image` output
does not age into an apparent orphan after cold reopen. Each sweep inspects at
most 256 directory entries; fresh or referenced entries consume that work
budget too, so cold open cannot scan an unbounded directory merely because
nothing is eligible for deletion. A private atomic cursor records phase and
logical offset, survives a newly constructed store handle, advances from
staging to blobs, and resets after a complete cycle; retained entries at the
front of one directory therefore cannot permanently starve later expired
orphans. Cursor corruption or a failed cursor write only repeats bounded work
and never authorizes deletion. One message is limited independently to
8 images, 32 MiB of raw source bytes, and 16 MiB of normalized bytes.

PWA browser files first enter a separate short-lived raw draft scope. These
uploads are not session attachment identity and are not journal facts. Only a
successful core admission can publish normalized blobs and commit their opaque
ids with the user message; abandoned draft uploads expire independently. The
process-random draft tree and its fixed `drafts`/`web` components are created
one level at a time and opened without following links; a pre-existing symlink
is rejected before clipboard or browser bytes can be written outside the
core-owned staging domain.

TUI clipboard PNGs use a separate process-local registry in that same private
draft tree. The registry has a 128 MiB aggregate bound and a one-hour TTL.
Removing or clearing a composer entry, completing initial-message admission, or
receiving the durable steering claim releases the exact core-minted raw file.
Physical deletion failures remain registered for a later sweep, while arbitrary
user-selected `/attach` paths are never deletion authority.

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

### Workflow and intelligence state

Plan Mode is session state, not a separate control-plane file. DSH-compatible
`plan/mode` events record active/inactive state; an approved plan is also bound
to its durable event sequence and digest so the next run can consume it.

A run's selected skill catalog is recorded in `request/header.skills` for
explainable live/replay parity. User skills live under `~/.clat/skills`; project
skills live under `<project>/.clat/skills` and override the user layer. Their
bodies/resources remain ordinary user/project files rather than session state.

`lsp.json` is a user-managed configuration input and never enters a session
journal. LSP processes and protocol buffers are transient project-owned state.

`/context` is wholly derived: it writes no file or journal event and keeps no
background monitor after returning its snapshot. Its image count, normalized
bytes, visual estimate, safety factor, and older-image omission count come from
the same detached request projection used by the agent run.

`memory.json` is a separate versioned unit because user memory spans sessions.
Project records carry the canonical project key; user records do not. Every
record has a stable id, revision, source, timestamps, digest, and optional
source-file digest. The core writer uses a regular-file/no-symlink check,
0600 temporary file, rename, and parent-directory fsync. Add/edit/delete are
explicit user operations with revision CAS. Searches and run injection are
read-only and never append a session event.

Goal state is session-local. A complete `goal/change` snapshot records each
revision, transition, budget counter, acceptance result, and terminal/blocked
reason. The goal projection participates in admission, checkpoint restore, and
tail replay. The armed bit is intentionally absent from the durable snapshot:
restart and session switch require the user to arm again.

Subagent activation is likewise process-local and defaults off. Each actual
child writes a DSH v2 `subagent/descriptor` plus CLAT `clat/subagent` start/end
facts to the parent journal before/after execution. The latter is ignorable to
older consumers but still has strict admission and a checkpointed lifecycle
projection. Child prompts, transient tool traces, and independent histories do
not create resumable session directories; bounded output returns only as the
parent tool result.

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
