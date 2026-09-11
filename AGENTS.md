# CLAT Project Constitution

CLAT is a rock-solid agent foundation: fast, local-first, open-source, one binary.

All contributors and coding agents working in this repository should preserve these principles unless a deliberate project decision changes them.

1. **Local First** — Prefer local execution and local state when the task can be completed without a remote service.
2. **One Binary** — The core CLI should not require users to install Node.js, Python, or another language runtime.
3. **Model Agnostic** — Model vendors are adapters behind CLAT-owned interfaces; no provider defines the core runtime.
4. **MCP Native** — MCP is a first-class protocol for external capabilities, while native core tools may remain direct Rust implementations.
5. **Project Aware** — CLAT should understand the repository and working context it operates in.
6. **Permission First** — Side-effecting operations must pass through an explicit permission model.
7. **Dogfood Driven** — CLAT is developed against real work first, dogfooding CLAT itself daily; requirements may originate from any dogfood project.
8. **Generalize, Never Special-case** — Product requirements may originate from any dogfood project, but CLAT core must contain reusable abstractions rather than caller-specific behavior.

## Engineering constraints

- Rust is the implementation language for the core runtime and CLI.
- Keep dependencies minimal and justified.
- Prefer standard, interoperable formats and protocols over project-specific equivalents.
- **DSH is the reference implementation** for interoperable surfaces (session journal, event vocabulary, permission gating). Follow it unless there is a recorded reason not to; every deliberate deviation lands in a `docs/research/` mapping doc (see `dsh-permission-gating.md` for the pattern).
- Keep the Agent Runtime, Model Provider, Tool, Permission, Context, Session, Project, and Event concepts separable.
- Favor observable event-driven execution so future CLI, TUI, IDE, desktop, or remote clients can consume the same runtime events.
- The single-agent runtime is the daily-driver baseline. Multi-agent features are now admissible, but only when driven by a concrete dogfood need and compared against DSH's subagent design — no speculative orchestration.
- A new durable-event producer must pass four gates together: catalog known-type, admission payload validation, projection fold (with checkpoint/restore), and live/replay parity. The `src/session/catalog/` seat tables are the single home that makes these one change — see "Code shape and test discipline" below.
- **Byte-exact test data must be pinned in `.gitattributes` in the same change.** Any fixture consumed byte-for-byte — `include_bytes!` targets, signed artifacts (minisig pairs), golden files with length/digest assertions — gets a `-text` (signed/binary goldens) or `text eol=lf` (text goldens) entry when it is added. Otherwise Windows `core.autocrlf` checkouts silently rewrite line endings and CI fails only off-platform (2026-08-22 TUI snapshots, 2026-08-24 market signatures — same failure class twice; see the incident notes inside `.gitattributes`).
- **Never `git checkout <path>` to "undo" an edit in a dirty, uncommitted working tree.** Checkout restores the committed baseline, not the pre-edit state — it silently destroys the entire uncommitted change for that path. This failure class has now hit auditors repeatedly (mutation spot-checks during audits: mutate → test red → `git checkout` to restore → uncommitted VP-3 work wiped, 2026-09-04; earlier researchers hit the same trap). Undo your own mutations with a reverse string replacement or reverse patch only; `git checkout`/`git restore` is reserved for deliberately discarding a path back to HEAD, said out loud in the report when used.
- **Separate edit feedback from delivery gates (2026-09-10, TS-1).** During editing use `scripts/gates.sh [test-filter ...]`: only relevant Rust tests, with no clippy/xwin/rustdoc/npm/E2E repetition. No arguments selects domains from staged, unstaged and untracked paths; unknown/shared inputs fall back to all Rust targets. Zero matches or ignored-only matches fail. Run `scripts/gates.sh --full` once before handoff; rerun failed/affected checks after repairs, not every already-green face. Full Linux CI and `scripts/ci-box.sh` invoke the same `scripts/gates.sh --ci` steps (including gated and adapter tests); Windows CI remains mandatory. Local `--full` additionally checks Windows statically with xwin. Use targeted stress only for a concrete unresolved concurrency concern; use the Linux container for platform-semantics changes. The toolchain remains pinned by `rust-toolchain.toml`. Keep CI/container/gates changes in the same batch. See `docs/testing.md`.
- **Commits and pushes are performed by the 负责人 (repo owner) personally.** Developers and coding agents never run `git commit` or `git push` — not even after audit closure, and approval of the changes is not approval to commit. Deliver a gate-green working tree (tests, fmt, clippy, docs updated) and let the owner review and commit it.

## Code shape and test discipline (2026-09-11 consolidation)

The 2026-09 refactor campaign paid down the structural debt below
(giant match arms, root-directory sprawl, 24%-of-code root files,
un-tiered verification). These rules keep it paid down. Every rule
below was paid for by a real incident; the dates name them.

- **Vocabulary has exactly one home.** Adding or changing a durable
  journal event or a `RunEvent` variant means one row in the
  `src/session/catalog/` seat tables (validation + surface + replay
  kind + recorder/wire projections) plus its golden — nothing else.
  A new per-event/per-variant string-match arm anywhere else is a
  layering violation; the architecture tests pin the catalog as the
  single home and the macro-generated exhaustive matches make a
  missing variant a compile error. Before the consolidation the same
  vocabulary change touched four files (the four-times-recurred
  "vocabulary race" tax).
- **Layering is crate physics, not convention.** Terminal-independent
  code lives in the `clat-core` crate (`src/core.rs` entry,
  `crates/core/Cargo.toml`); frontends (TUI, dsh terminal integration,
  the binary) stay in the root `clat` package and consume the core
  only through `src/client_ports.rs`. The core cannot depend on the
  frontend — not as discipline but as an impossible edge in the cargo
  graph, enforced by the workspace metadata test together with the
  single-binary rule. **The `src/` root accepts no new production
  files** — new domains open a directory (2026-09: root production
  share cut from 24% to 19.7% by moving every multi-thousand-line
  root file into its domain directory; do not grow it back).
- **Shape budget (the anti-decay ratchet).** Do not add new
  production functions ≥80 lines, and do not extend an existing one —
  split it while you are there (the proven shapes are the seat table,
  the priority chain, and the per-family file; see
  `docs/architecture.md` for the precedents). At every round close,
  run `scripts/code-health.py` and record the result: giant-function
  count and root share may improve, never regress. What is not
  measured drifts; the 2026-09 baseline was 91 giant functions and
  24% root share at worst.
- **Flake discipline.** A red test is rerun once; a second occurrence
  opens a case with a captured panic message. A "known flaky" label
  without a filed case is forbidden — the 2026-09-10 typert case was
  a real bug (silent POST loss on pooled connections) wearing a flake
  label for three occurrences, and the label cost the owner two hours
  before someone chased the reply instead of the assertion.
- **Test quality.** Every fix ships a test that is red on the pre-fix
  code (constitution rule above); never transcribe implementation
  behavior — assert the invariant. When touching duplicated test
  scaffolding, merge it (the providers adapters carried 97 duplicated
  normalized blocks in their test sections while their production
  code was only 5% alike). Harness-heavy tests live behind the
  `runtime-tests` feature so pure-UI edit loops stay fast. Test
  counts are reconciled at delivery: a refactor must not lose tests
  silently (the 2026-09 workspace split was audited as
  328 root + 970 core = the exact pre-split 1,298).
- **Measurement discipline.** Performance and latency claims need
  repeated samples in a clean machine state; a single timed run on a
  developer machine is not evidence (both sides of the 2026-09-11
  inner-loop closure paid this tuition — one 59–70s sample was an
  environmental outlier; the clean-state acceptance measured 8.2s).
  Acceptance numbers are measured independently by the reviewer, not
  taken from the implementer's report. Source-scanning tests must
  normalize `\r\n` on read — Windows autocrlf checkouts made an exact
  needle split silently scan the whole file (2026-09-11 CI incident).

## Documentation map

- **Committed docs** (`docs/`, public): `usage.md` (TUI + headless
  `clat exec`), `model-editor.md`, `permissions.md`, `mcp.md`,
  `plugins.md` (runtime/package/market model), `wasm.md` (plugin authoring),
  `dsh-plugins.md` (DSH adapter porting guide), `dsh-compat.md`
  (runtime-oracle compatibility matrix), `architecture.md`,
  `providers.md`, `storage.md`, `testing.md` (verification tiers and
  standing test rules), `releasing.md`, `live-validation.md`. They are
  indexed by `README.md` / `README.zh.md` (English/Chinese mirrors —
  update both indexes together); `sdk/dsh-adapter` carries its own
  npm-facing README pair.
- **Local-only docs** (gitignored; present in development workspaces —
  never hyperlink them from committed docs or README, plain-text
  references are acceptable): `docs/research/` (decision archives and
  DSH mapping docs), `docs/todo/` (implementation plans with
  invariants), `docs/audit/` (adversarial reviews), `docs/tui/`.
- Technical depth belongs in the matching doc above; the READMEs stay
  summaries and indexes, with the two language versions in sync.

## Layering rules (hard boundaries)

The codebase is a UI-independent core (the `clat-core` crate) and thin
frontends (TUI, dsh terminal integration, the binary — the root `clat`
package). A future desktop app (and IDE/remote clients) will reuse the
core as-is; every violation of these rules is migration debt for that
day.

- **The core never depends on the frontend — enforced physically.**
  The core→frontend edge does not exist in the cargo graph, and
  `tests/architecture_boundaries.rs` additionally rejects frontend
  imports of core internals (storage/session/composition/execution
  owners), frontend-owned assembly, process spawning, and
  slash-command dispatch. Frontends consume the core only through
  `src/client_ports.rs`.
- **No business logic in UI modules.** Rendering, key/mouse handling,
  dialog state machines, and view-model mapping only. Anything a
  non-terminal client would also need (run lifecycle, persistence
  policy, balance/quota logic, permission semantics) belongs in core,
  with the UI calling it. The crate boundary cannot catch this class —
  it stays a review duty.
- **Interact with runs only through `EventSink` / `RunEvent`.** Never
  poll or reach into `Run` internals from a frontend. The event stream
  is the future RPC message set; changing a variant or the sink
  signature is a protocol change — declare it in the commit message.
- **Permissions flow through `InteractivePermissionPolicy` + injected
  approver.** Frontends supply an approver closure; they never
  implement permission semantics themselves.
- **Frontend-specific I/O stays frontend-local.** Terminal escape
  sequences, raw-mode handling, and `~/.clat`-external UI state never
  leak into core. Conversely, core owns all persistence and spawning
  (models, MCP subprocesses) — frontends never spawn or store directly.

User-visible behavior change ⇒ update the matching public doc in the
same change (usage / permissions / architecture / README).

Boundary note: the DeepSeek/GLM monitor and run workers live in core
plugins/Application. `UiEvent` in `tui/worker.rs` is frontend-local
channel multiplexing only; core lifecycle and persistence must not
move back into it.

## State discipline (invariants before code, tests from invariants)

Every bug shipped in the v0.3.x session-lifecycle work shared one root:
code written against a freshly imagined scenario instead of the state
space, verified by tests transcribed from the implementation. These
rules exist to break that pattern.

- Before changing persistent state (schema, lifecycle, files), write
  down the invariants that must hold and audit **every reader and every
  writer** of that state. An unspoken invariant will be violated by the
  next cleanup. (The vanishing-session bug was exactly an unspoken
  invariant: *automatic code may never archive or delete a session that
  has chat content*.)
- Behavior tests are derived from invariants or written specs — never
  from reading the implementation. A test that asserts what the code
  just did is transcription, not verification; it cannot fail when the
  design is wrong because it shares the author's mental model with the
  bug.
- Every bug fix ships with a test that **fails on the pre-fix code**.
  If such a test cannot be written, the bug is not understood yet.
- A test can be red pre-fix yet still fail to discriminate: each
  assertion that claims to lock a fix must go red when **that specific**
  fix is deleted. Spot-check this during review — the `/new`-reset leg
  of the permission-mode test once passed for the wrong reason until an
  audit caught it.
- Permission, path-fence, and persistence changes get an adversarial
  self-review before handoff: attack the new code as an outsider
  (failure paths, concurrency, cross-session leaks, leftover
  artifacts), fix what it finds, and report what was checked.
- Stateful features are verified by walking real user operation
  sequences through the code path by path (resume → exit → reopen),
  not only by the test suite. fmt / clippy / cargo test green is
  hygiene, not evidence of correctness.

## Code Exploration & Analysis (Codegraph)

- **Codegraph Indexing**: This project supports CodeGraph indexing (identified by the `.codegraph/` directory).
- **Usage**: Always prioritize using `codegraph` to analyze code architecture, query symbol definitions, understand call flows, or determine the blast radius before making code edits.
- **Agent Tools**: AI agents should use the `codegraph_explore` MCP tool as the primary way to read and understand codebase context instead of relying on traditional `grep` or manual file reading.
- **Query Discipline**: `explore` is semantic retrieval, not exact file reading. For generic names, provide path plus call-chain anchors and avoid `maxFiles: 1`; use `codegraph node --file` for exact reads. Never mistake ranking noise for an indexing/parser failure. For files or custom configs that Tree-sitter doesn't support, fall back to grep or read_file.
