# CLAT extended engineering policy

The detailed constraints below remain binding. AGENTS.md carries the concise
always-on rules. Before editing runtime, persistent state, permissions, tests
or architecture, consult the relevant section here. This is progressive
disclosure of engineering detail, not a relaxation of any requirement.

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
- **UI tests protect behavior, not presentation details (2026-10-26).**
  Ordinary copy, colors, icon shapes, exact spacing, menu counts and DOM
  arrangement must not become fixed CI contracts or visual goldens.
  Copy/style/layout-only edits do not require new automated tests. Review
  screenshots for visual quality; use stable action/test identifiers and
  assert user-visible outcomes for interaction tests. Catalog checks protect
  keys, parameters, references and format while allowing wording changes.
  Preserve checks for actual user/file/tool data, protocol fields, permissions,
  persistence and signed/byte-exact artifacts. When migrating Web/Desktop
  tests, remove or merge brittle presentation assertions with a recorded
  behavior-coverage mapping; do not silently lose business coverage or keep
  low-value tests alive by repeatedly updating strings and screenshots.
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

## Layering rules (hard boundaries)

The codebase is a UI-independent core (the `clat-core` crate) and thin
frontends (TUI, dsh terminal integration, the binary — the root `clat`
package), plus the optional `desktop/` Tauri client. Desktop consumes the
same core through client ports; default CLI builds do not include native GUI
dependencies. IDE/remote clients must preserve this boundary too.

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

