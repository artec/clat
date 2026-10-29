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
├── web.json                     # secret-free native Web provider/key-reference selection
├── schedules.json               # authorized rules, budgets and delivery control state
├── history-index/               # rebuildable SQLite FTS5 cache; never session authority
├── memory.json                  # explicit user/project knowledge (0600)
├── im.json                      # atomic WeChat binding, authorization and pairing authority (0600)
├── trust.json                   # canonical project path -> trusted timestamp
├── web-token                    # clat serve Bearer credential (0600)
├── host-endpoint.json            # private host discovery hint, not proof of liveness
├── dsh-last-session             # last session id opened by clat dsh
├── drafts/<process-random>/web/  # private raw drafts; 128 MiB aggregate bound / 1 h TTL
├── file-review/                 # native file-change and recovery evidence
├── vision-probe-log.jsonl       # vision-probe diagnostics
├── utility-log.jsonl            # bounded naming/suggestion failure evidence (256 KiB rotate to .1)
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
            ├── session.v4.jsonl.zstd # current authoritative DSH-compatible log
            ├── session.lock          # POSIX-only stable DSH writer-lock inode
            ├── clat-checkpoint.json # bounded derived projection cache
            ├── clat-utility-budget.json # private same-turn naming deduplication state (legacy filename)
            └── attachments/
                ├── .orphan-sweep-cursor-v1 # private bounded-GC progress
                ├── blobs/<sha256>   # immutable normalized PNG/JPEG bytes
                └── staging/         # unpublished admission transactions

~/.clat-wasm-cache-<storage-id>/     # outside ~/.clat; rebuildable compiled WASM cache
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
| `clat-utility-budget.json` | per-session naming scheduling state, retaining legacy counters | bounded regular-file read and atomic private publication; corruption pauses naming; counters no longer impose call quotas; suggestions do not access this file |
| `web-token` | local API credential | validate regular 0600 file; create/rotate atomically |
| `host-endpoint.json` | host port and instance hint | host publishes atomically under root lease; authenticated discovery validates instance/root/versions; stale hints survive shutdown |
| `memory.json` | authoritative explicit knowledge | version/CAS/path validation fail closed; never inferred from model output |
| extension manifests | user input | isolate configuration/plugin failure where possible |
| plugin registry | authoritative activation pointers | version/digest/signature mismatch fails closed; never reset or guess |
| plugin artifacts/staging | immutable code / uncommitted transaction bytes | verify complete tree before activation; stale staging is never executable |
| market cache/staging | public signed metadata / uncommitted downloaded code | signature+expiry checks on every read; only a committed package registry can activate code |
| `dsh-last-session` | decorative client preference | missing/corrupt/oversized/symlink → ignore |
| `utility-log.jsonl` | bounded utility failure evidence | one JSON line per failed naming/suggestion attempt carrying CLAT-owned category codes (never provider error bodies); append is serialized across writers; rotate to `.1` past 256 KiB, dropping the line if rotation fails; write failures only drop the evidence line |

Back up `~/.clat` as a unit when conversation history and configuration both
matter. Copying only `storages/workspace.json` does not copy conversations;
copying only `sessions/` preserves conversation facts but not provider keys,
trust, or current selections. The compiled WASM cache
`~/.clat-wasm-cache-<storage-id>` is outside this backup unit; it is rebuildable
and can be deleted while CLAT is stopped. See [compiled-code cache](plugins.md#startup-verification-and-compiled-code-cache).

## Session journals

Everything a conversation *is* is represented by one append-only event log.
CLAT's event types, headers, paths, and encoding are compatible with the DSH
session format.

`/new` may update the workspace's active-session pointer to Fresh, but it does
not create a session directory or log. The first prompt or an explicit `/task create` materializes the new
session. Task creation saves its own durable objective without inventing a user
chat message.

### Physical encoding

New sessions use `session.v4.jsonl.zstd`. Discovery recognizes the canonical
`session.vN.jsonl[.zstd]` generation family and reads the highest generation in
a session directory. Each committed batch is an independent zstd frame with a
content checksum. Independent frames make appending cheap and limit crash
damage to the final incomplete frame.

Every frame is decoded under a 64 MiB budget. Streamed record reads carry the
same bound, so a compressed bomb fails with a named error instead of consuming
unbounded memory.

The session root may contain uncompressed generation files from a compatible
source, but one root cannot mix raw and zstd session encodings. Startup rejects
an encoding conflict before mounting storage. Released-v0
`session.jsonl[.zstd]` remains readable, including beside newer generations, but
CLAT opens older supported formats (v0/v2/v3) read-only through normal startup and `/resume`. Reading
and closing do not repair the source, append a seed, write a checkpoint, or
clean attachments. Runs and persisted session edits are rejected.

Only while a legacy local session is selected, `/update` appears in the command
menu. It ensures the complete history is at the current format (V4), preserving
the original generation and attachment paths, and reopens the same session for
normal read/write use. The command disappears after upgrading and is absent
and unavailable in new/current-format sessions. The migration chain runs in
memory and publishes only `session.v4.jsonl[.zstd]`, with no intermediate
generation files. Ensuring an already-current generation is a no-op; retired
v1 is rejected with guidance to use `/new`. It is not a binary updater and does not
migrate a remote DSH host's files.

Pre-attachment-ID image records retain their original fenced paths and receive
the same deterministic legacy IDs used by replay. Upgrade does not relocate
their images; missing image files are not reconstructed from the journal.

Upgrade holds the session writer lease and atomically publishes the validated
new log without overwriting an existing generation. Before-publication failure
leaves the original generation selected; a reported post-publication failure can be recovered by
retrying or reopening. Torn logs, unsupported lineage, unknown extensions, and
unresolvable references are refused instead of silently discarding history.
Close old CLAT processes first. The retained source is a pre-upgrade backup, not a
synchronized copy of later messages. Alternatively, use `/new` to leave the
old conversation untouched.

If the highest canonical filename names a generation newer than V4, CLAT
refuses inspection/resume with an upgrade-or-new-session diagnostic. It never
falls back to the older sibling, because doing so would split one conversation
into two invisible histories.

Every writable current-format handle also owns DSH's per-session kernel lease for its full
lifetime. POSIX uses non-blocking `flock` on the stable `session.lock` inode;
Windows uses DSH's case-folded path-derived named semaphore and creates no lock
file. Readers stay lock-free, while first materialization, append, and repair
all require the lease. A crashed process releases it through the kernel.

The read-side revision remains the DSH-compatible stat token. A leased writer
also keeps a SHA-256 witness of the physical log bytes, collected during its
existing sequential read and advanced after each successful batch. If only
`ctime` changes, CLAT rechecks the full bytes and the handle/path identity before
continuing. Metadata changes alone do not invalidate unchanged content; a
replacement, append, truncation, or same-length rewrite still stops the writer.
Ordinary appends do not rescan the full history. Rollback preserves the previous
byte witness and cursor; an indeterminate outcome still requires cold recovery.

Compaction's `compact-checkpoint` user-role carrier remains in the durable log
and model-facing surface. Human transcript, message outline, input history and
public search exclude this internal carrier, retaining the compaction marker
and original human conversation. Derived checkpoint/search versions invalidate
older presentation caches; this does not rewrite session journals.

V4 retains model deltas inside `assistant/message.stream`; failed model requests
are retained as `assistant/attempt` with the same embedded stream format.
`sourceEventSeqs` ranges are expanded before projections see an event.

New turn/step coordinates are 1-based. Migration preserves source coordinates,
including older CLAT logs' 0-based coordinates. Request headers no longer persist
`system`; the prompt is represented by a protected `system/message` head,
initialized immediately after `step/start` and replaced when the prompt changes.
V4 stores tool results as `role:"tool"` messages with message-level `toolCallId`
and optional `isError`; the V3 `tool-result` content wrapper is retired.
Producer attribution uses direct source kinds (for example `plugin:clat`)
instead of V3's `{kind:"plugin",plugin:...}` wrapper. CLAT accepts but does
not produce V4 `developer/message`, `image/offload`, or `workspace/changes`.
Sessions containing developer messages or a nonempty `agentPreset` open
read-only: CLAT cannot continue their model/deferred-tool context safely.
`/update` is only for older generations and is unavailable in those V4 sessions.

DSH 0.2.1-rc.2 adds required `working-directory/change`, `job/registered`, and
`job/settled` records within V4. CLAT admits their declared payload shapes and
preserves their bytes during append-only continuation, including an unsettled
registration. These are non-surface metadata with no CLAT cwd/job projection or
producer. Continuing uses CLAT's existing project/run directory; it does not
adopt DSH's recorded effective directory or take over a background job. See the
revision-specific boundary in the [compatibility matrix](dsh-compat.md).

### Event admission

Known durable event types are validated before append and again when read.
Malformed payloads, retired types, unsupported required headers, and unknown
required events fail closed. Unknown events are skipped only when explicitly
marked `ignorable: true`.

Adding a durable event is a four-part contract: catalog it, validate its
payload, fold it into projections/checkpoints, and prove live/replay parity.

### Crash recovery

On open, CLAT scans frames and events in order. For writable current-format sessions, a torn final frame is truncated
to the last durable boundary. If the crash left an open tool/step/turn, recovery
appends synthetic `tool/result`, `step/end`, and `turn/end` events so later
folds see a complete state machine.

Legacy v0/v2 sessions are inspected without these recovery writes. Recovery does
not guess through corruption in an earlier committed frame. A
corrupt or unsupported session fails before the active-session pointer moves.

## Projections and checkpoints

The journal folds into independent projections:

- model-context surface;
- bounded current task progress, operation evidence, budget reservations and
  optional versioned acceptance/verification/completion provenance;
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

Client delivery recovery scans durable journals without changing the selected
session. If a concurrent title or write-behind commit invalidates a scan, owner
lookup retries at most three times and discards each failed attempt's result.
Duplicate admissions, invalid records, and persistent I/O errors still fail
closed; pausing or deleting a scheduled task does not bypass receipt recovery.

Cold resume is a single physical log pass. The recovery stream feeds
projection folding, transcript replay, and usage restoration together. Session
listing can use checkpoint rows and the bounded first log record instead of
decoding the conversation body.

Incremental history reads publish their replay cursor, messages, pairing state,
and usage only after the complete stream passes its revision check. A transient
I/O conflict gets at most three attempts with provisional suffix state discarded
between attempts; persistent I/O failures and malformed logs remain visible
errors. The existing transcript prefix is reused rather than cloned per attempt.

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
| `settings.json` | active model, named profiles, active-profile pointer, host-wide companion utility policy | fact |
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

The optional `settings.json.utility` row contains only `naming_enabled`,
`suggestions_enabled`, and an optional saved-profile name. It is a full
replacement under the same control-storage lock and atomic publication path as
model settings. A saved-profile reference must resolve to a regular profile;
deleting that profile clears the reference in the same commit. No API key or
provider secret is projected into the row. The per-session
`clat-utility-budget.json` sidecar is separate from the journal: its bounded
reader rejects links, special files, malformed/unknown data, and oversized
content, while each reservation is atomically published before any provider
request. Legacy counters remain readable, including exhausted budgets, but no
longer limit calls. Naming deduplicates only the same or older ended turn;
the timestamp is informational, not a cooldown. Suggestions bypass this file
entirely, avoiding per-click disk writes and leaving old counters untouched.
The `sessions/` directory wins over list caches and derived session-id arrays.

### Workspace registry

`storages/workspace.json` is field-for-field compatible with DSH's workspace
unit: `unit`, `global`, and `tables.workspaces` with camelCase path/title/
session/time fields. CLAT adds optional active-workspace and active-session
pointers, and per-workspace `sessionFlags` keyed by session id with `pinned` /
`archived` booleans. Missing flags mean false; explicit organization is shared
frontend state, not deletion authority or journal vocabulary. CLAT writes its
own registry only, not a DSH installation's files.

Project paths are canonical real paths. On mount, reconciliation:

- prunes registry session ids whose physical logs vanished;
- adopts unregistered logs whose header cwd matches the workspace;
- preserves existing display order and appends adopted sessions
  deterministically;
- synchronizes global workspace ids with the table.

Each project remembers its own active session. Switching projects therefore
restores the conversation last selected in that project.

### Native file recovery evidence

`file-review/<sha256(canonical-project)>/<sha256(session)>-<turn>.json` stores
CLAT-private version-1 native commit evidence, separate from interoperable
session JSONL. It retains pre-operation text (including existing dirty edits),
captured after text, file identity/metadata/hash, mixed-writer indication and
prepared/captured/restoring/restored status. A prepared follow-up also retains
the last confirmed result separately from the unconfirmed intent.
Capabilities/no-follow protect the
private store; records are bounded and atomically persisted before/after native
publish. Crash uncertainty fails closed rather than claiming recovery safety.
Backup this directory along with sessions to retain recovery evidence.

Recovery directories `.clat-restore-<uuid>` inside the affected file's parent
hold detached `after` and, for existing files, reconstructed `before` material.
They are private and retained even after success. Reported partial/conflict
states may require manual reconciliation; remove retained material yourself only
after inspecting it and keeping any needed backup. No automatic cleanup policy
is introduced. Shell/MCP, absolute-path writes, directory creation and external
effects are outside this ledger. It neither rewrites history nor stores a whole
workspace or Git snapshot.

### Workflow and intelligence state

Plan Mode is session state, not a separate control-plane file. DSH-compatible
`plan/mode` events record active/inactive state; an approved plan is also bound
to its durable event sequence and digest so the next run can consume it.

Versioned plan documents additionally use the ignorable `clat/plan/change`
extension. It records document identity/session, revision, title, full Markdown,
digest, approval and goal linkage, plus idempotent workflow receipts. Its catalog
validator and `plan-documents` projection admit the same transitions during live
folding, replay and checkpoint restoration. Existing `plan/mode` and
`goal/change` meanings do not change; there is no format-generation bump.

`<session>/plans/<plan-id>-r<revision>.md` is a private, reconstructable projection
of the recorded text, not a second fact store. Missing files can be rebuilt;
external edits or symlinks are preserved and refused for approval. Reads/writes
use held session/directory capabilities, no-follow file opens, bounded reads and
private atomic publication. New goals and their plan binding commit in one
journal batch. Execution progress never rewrites approved document text.
Old approved `plan/mode` text remains viewable without rewriting its history;
explicit workflow approval imports its document identity into the new projection.

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

For a Fresh root, the initializer leases the newly created directory before
publishing `config.json`, while retaining its ancestor lease. If another
initializer has acquired the new directory, the loser publishes no control
facts; the directory's owner can still complete initialization. This avoids
stranding concurrent CLI/desktop launchers without a ready Host.

## Web token and DSH preference

`clat serve` creates `~/.clat/web-token` only after the requested loopback port
binds successfully. Creation and `--rotate-token` use atomic replacement and
0600 permissions. `--token` bypasses this file for one process and never
modifies it. The token does not enter manifests, URLs, logs, or session events.

`dsh-last-session` contains one opaque session id, capped at 4 KiB. Reads reject
final-component symlinks, including relative links to files in the same directory,
through the capability resolver's no-follow option. Non-files, invalid UTF-8,
and oversized content are also ignored. Writes are atomic
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
The legacy database file is never opened or interpreted — the SQLite
dependency is gone entirely — so any pre-cutover variant is preserved
byte-for-byte rather than partially converted or refused on content.

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

## Disposable run scratch

Each active core run owns a freshly created `clat-run-<random>` directory beneath
the operating-system temporary directory, outside the workspace. Unix creates
it with mode 0700. The same root authority feeds native write/edit/patch fences
and command sandbox providers. Windows adds no shared system-temp write grant.
Run teardown reaps child processes before revoking and deleting scratch;
creation or explicit cleanup failure is reported instead of silently falling
back. An abrupt host kill may leave a private temporary directory for manual
cleanup.

Scratch holds disposable model scripts and files only. Journals, checkpoints,
attachments, recovery ledgers, and other durable session state keep their
normal storage paths. Scratch writes do not enter FileReview and do not appear
in `workspace.changes` because they are outside the repository. Scratch paths
are injected at execution time and are never relied on to resume a session.


## Versioned Task acceptance compatibility

Task acceptance adds optional `acceptance_state` and operation `command_fact`
and `execution_identity` fields to the existing `clat/task` payload. Missing fields deserialize to `None`
and are omitted on serialization: GA-1 snapshots/goldens keep their original
byte shape, and old text acceptance/completed Tasks retain human semantics.
Contracts carry their own positive version; checker identity includes its
version. No new journal event type or alternate Task store is introduced.
Admission and Task projection share snapshot/transition validation, including
full-fold and checkpoint/tail parity. A Task snapshot remains capped at 128 KiB.

The acceptance object stores the current user contract, current human
attestations, up to eight immutable reports, one candidate, explicit completion
provenance and at most 32 one-use recheck grants. Contract replacement requires
version+1, clears current attestations/candidate/grants, and preserves prior
reports. The journal retains prior contract/confirmation transitions. Reports
bind Task/project/session, contract/attestation digests, checker versions,
observed file SHA-256 and explicit producer operation/result identities. Operation
command facts retain bounded receipts/lifecycle metadata and before/after input
digests, not process output. See [usage](usage.md#confirmed-acceptance-and-evidence-backed-completion)
for rule/resource limits and observation boundaries.

Read-only verification and applicability views do not append Task facts or
rewrite checkpoint files. Explicit report save is a durable CAS. Completion is
a later durable CAS referencing an already saved report; a model proposal stores
only pending state until successful Run disposal/accounting/journal finalization.
For Task Runs the final Task transition and `turn/end` commit as one atomic
durable group after final step flush, accounting and actual scope disposal.
Terminal publication follows that commit, so a late gate failure cannot leave
an earlier successful live/replay terminal. Native command operation identities
come from frozen argv/shell and resolved cwd; wrapper aliases and output/polling
options do not form a new execution identity. Legacy missing fields stay omitted.
Failures never publish the proposed Task state. A crash can leave a report or
pending candidate, or a charged unsettled Run reservation; reopening does not
execute it. Current file changes mark an old report stale without changing its
historical result or completed Task phase. Metadata revision and content
freshness are separate identities.

Compatibility is forward reading, not downgrade writing: GA-1 CLAT uses strict
Task payload decoding and cannot interpret these new fields. Older readers
without the CLAT extension may ignore the ignorable event, but cannot enforce
acceptance. Do not resume/write structured Tasks with a pre-acceptance binary or
strip fields to make them readable. Keep the journal and use a compatible binary;
no database deletion or migration of original text into machine checks is needed.

`history-index/index.sqlite` (and SQLite WAL/SHM companions) is a private derived history-search cache. It is not a session fact or control-plane rule store. Source revisions reconcile changes; corruption can be rebuilt without repairing or altering journals. No export/backup correctness depends on retaining this directory. Cache directories and database files reject symlinks; new files use private permissions.

Native job facts live in their original conversation journals. Retained output and live process handles are bounded Host memory, never reconstructed as running processes after a restart. Missing terminal records are conservatively settled as interrupted/unknown, without command replay. The `jobs` projection is derived and checkpointable; an unsaved terminal fact remains a separate visible error and blocks takeover until an explicit retry succeeds.
`schedules.json` is Host-owned control state (unit `clat.scheduler`, version 1),
independent of session journal vocabulary. It retains explicit instruction/model/
time/budget authorization, next occurrence, bounded reservations and delivery
records (64 rules, 1,024 deliveries, 2 MiB maximum). Credentials are resolved from
the existing store at dispatch and never copied into this file. A bounded private
no-follow reader and atomic writer own the file. Corruption, external revision
changes or uncertain publication stop scheduling until explicit reload. They
never default to an empty ruleset. Session receipts and terminal replay reconcile
committed work; absent terminal facts retain unknown usage conservatively.

Search's SQLite cache is disposable; schedules are not. Do not remove scheduler
state during cache cleanup. Pausing/deleting future work does not falsify already
admitted execution facts. Original-session journal ownership continues through
the shared SessionService writer pool. See [scheduled work](schedules.md).

### Human development state

Project developer state lives under `<storage-root>/development/<canonical-root-hash>/`:
editor operation receipts, client-isolated draft checkpoints, private recovery copies,
Git preferences/operation receipts/discard backups, temporary frozen indexes and terminal
metadata. These private JSON files are not session facts, DSH events or model context.
Human editing does not rewrite journals or add repository files for UI recovery.

Receipts are written before side effects. Pending/uncertain operations are reconciled or
reported as unknown after restart, never automatically re-executed. Temporary Git indexes
and commit-message files are removed after normal completion/cancellation. A crash may
leave an unreferenced private temporary file, which is not executable authority. Drafts
remain drafts on recovery, and external edits require explicit resolution. File/folder
trash uses no-clobber moves and preserves the source if the filesystem cannot perform the
move; cross-filesystem folder deletion may require the terminal. Discard backups preserve
bytes, including binaries, and restore only against the captured post-discard version.

Terminal metadata survives restart as ended records; input streams, credentials and live
process handles do not. Output history is bounded Host memory and is not a durable shell
transcript. Font/cursor preferences are client-device presentation state. Commit-language
preferences are project state. Third-party widget notices are embedded at
`/development-licenses.txt` and linked from Terminal settings.

The terminal registry also retains compact removal tombstones and idempotent
removal receipts in the same atomic JSON document. Removed identifiers are never
reused by a retried create request. Listing all projects does not create shells
or mount projects. Ended records remain until explicit removal; output is still
bounded Host memory, not restored as a transcript after restart. Device font
preferences use version 2 with system/bundled/custom selection and a family name.

Nested repositories use a project-private `repositories/<repository-id>` namespace
for existing Git operation and backup records. The main project's original records
remain in place, so old operation IDs can still be reconciled. This adds no session
journal events. The browser retains per-project/repository commit drafts and pending
operation IDs; operation receipts in core remain authoritative. Comparison tabs and
graph selection survive view switches in memory. Sidebar proportions and collapsed
sections are device preferences, independent of repository execution state.
