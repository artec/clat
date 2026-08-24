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
- A new durable-event producer must pass four gates together: catalog known-type, admission payload validation, projection fold (with checkpoint/restore), and live/replay parity.
- **Commits and pushes are performed by the 负责人 (repo owner) personally.** Developers and coding agents never run `git commit` or `git push` — not even after audit closure, and approval of the changes is not approval to commit. Deliver a gate-green working tree (tests, fmt, clippy, docs updated) and let the owner review and commit it.

## Documentation map

- **Committed docs** (`docs/`, public): `usage.md` (TUI + headless
  `clat exec`), `model-editor.md`, `permissions.md`, `mcp.md`,
  `plugins.md` (runtime/package/market model), `wasm.md` (plugin authoring),
  `dsh-plugins.md` (DSH adapter porting guide), `architecture.md`,
  `providers.md`, `storage.md`,
  `releasing.md`, `live-validation.md`. They are indexed by
  `README.md` / `README.zh.md` (English/Chinese mirrors — update both
  indexes together); `sdk/dsh-adapter` carries its own npm-facing
  README pair.
- **Local-only docs** (gitignored; present in development workspaces —
  never hyperlink them from committed docs or README, plain-text
  references are acceptable): `docs/research/` (decision archives and
  DSH mapping docs), `docs/todo/` (implementation plans with
  invariants), `docs/audit/` (adversarial reviews), `docs/tui/`.
- Technical depth belongs in the matching doc above; the READMEs stay
  summaries and indexes, with the two language versions in sync.

## Layering rules (hard boundaries)

The codebase is deliberately split into a UI-independent core and thin
frontends. A future desktop app (and IDE/remote clients) will reuse the
core as-is; every violation of these rules is migration debt for that
day.

- **The core never depends on the frontend.** Modules under `run`,
  `model`, `providers`, `tool`, `native_tools`, `permission`, `project`,
  `storage`, `mcp`, `presets`, and `event` must not
  reference `tui*` modules, ratatui, or crossterm. Dependencies flow
  one way: `tui*` → core, never the reverse.
- **No business logic in UI modules.** Anything in `tui.rs` /
  `tui_*.rs` must be presentation and input handling only: rendering,
  key/mouse handling, dialog state machines, view-model mapping. If a
  function would be needed by any non-terminal client (run lifecycle,
  persistence policy, balance/quota logic, permission semantics), it
  belongs in core, with the UI calling it.
- **Interact with runs only through `EventSink` / `RunEvent`.** Never
  poll or reach into `Run` internals from a frontend. The event stream
  is the future RPC message set — treat its shape as an interface, and
  think twice before making breaking changes to it.
- **Permissions flow through `InteractivePermissionPolicy` + injected
  approver.** Frontends supply an approver closure; they never
  implement permission semantics themselves.
- **Frontend-specific I/O stays frontend-local.** Terminal escape
  sequences, raw-mode handling, and `~/.clat`-external UI state never
  leak into core. Conversely, core owns all persistence and spawning
  (models, MCP subprocesses) — frontends never spawn or store directly.

### Practical checklist before merging

1. Does any new `use` in a core module mention `tui`, `ratatui`, or
   `crossterm`? Move the logic to core or push the call out to the UI.
2. Did you add a method to `App`/`tui_*` that another client (desktop,
   headless) would also need? Extract it into core first.
3. Did you change a `RunEvent` variant or `EventSink` signature? That
   is a protocol change — say so explicitly in the commit message.
4. New background threads or channels? They must belong to one layer:
   runtime workers belong to core, render/input plumbing to the UI.
5. Did user-visible behavior change? Update the matching docs in the same
   change (usage / permissions / README / architecture, plus
   `docs/research/` mappings for cross-project decisions).

Current boundary note: the DeepSeek/GLM monitor and run workers live in core
plugins/Application. `UiEvent` in `tui/worker.rs` is frontend-local channel
multiplexing only; core lifecycle and persistence must not move back into it.

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
