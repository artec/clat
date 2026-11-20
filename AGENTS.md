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
- **Localize product UI text.** All new or changed CLAT-owned user-facing text in CLI, TUI, Web/PWA, Desktop and user installers must use stable keys in the shared `locales/` catalogs and the existing explicit locale/presentation APIs; never hardcode display text in product code. Update English, Simplified Chinese and Traditional Chinese together, preserve live language switching, and run the catalog checks. Keep protocol identifiers, model/tool content and raw third-party diagnostics unchanged. Follow [the localization specification](docs/localization.md).
- **Human-readable presentation, stable machine identifiers.** Default user-facing tool, MCP, plugin and execution labels must describe the action in ordinary language using the shared three-language catalogs. Never expose raw snake_case names, wire IDs or internal namespace chains as the primary label. Keep original identifiers and diagnostics in expandable technical details. Perform this conversion only in presentation: do not rewrite journals, protocol fields, tool definitions/arguments/results, model content, source documents or Agent context. Unknown external capabilities use an honest generic localized label; never guess their purpose. Collapsed previews may adapt known identifiers while expanded originals and raw copies remain intact. Presentation wording and styling are manually reviewed, not fixed CI contracts.
- Prefer standard, interoperable formats and protocols over project-specific equivalents.
- **DSH is the reference implementation** for interoperable surfaces (session journal, event vocabulary, permission gating). Follow it unless there is a recorded reason not to; every deliberate deviation lands in a `docs/research/` mapping doc (see `dsh-permission-gating.md` for the pattern).
- Keep the Agent Runtime, Model Provider, Tool, Permission, Context, Session, Project, and Event concepts separable.
- **Initial context and no-progress rule.** Publish the complete schema for every frozen-policy-allowed tool in the initial model request: no hidden tool names or deferred discovery. Optimize concise tool descriptions and concise root project instructions without weakening authority or parameter constraints. Never turn an observed incident count into a tool-turn quota; protect against repeated evidence, allow model-visible correction, and preserve useful long-running work. See docs/architecture.md, Initial context principles.
- Favor observable event-driven execution so future CLI, TUI, IDE, desktop, or remote clients can consume the same runtime events.
- The single-agent runtime is the daily-driver baseline. Multi-agent features are now admissible, but only when driven by a concrete dogfood need and compared against DSH's subagent design — no speculative orchestration.
- A new durable-event producer must pass four gates together: catalog known-type, admission payload validation, projection fold (with checkpoint/restore), and live/replay parity. The `src/session/catalog/` seat tables are the single home that makes these one change — see "Code shape and test discipline" below.
- **Byte-exact test data must be pinned in `.gitattributes` in the same change.** Any fixture consumed byte-for-byte — `include_bytes!` targets, signed artifacts (minisig pairs), golden files with length/digest assertions — gets a `-text` (signed/binary goldens) or `text eol=lf` (text goldens) entry when it is added. Otherwise Windows `core.autocrlf` checkouts silently rewrite line endings and CI fails only off-platform (2026-08-22 TUI snapshots, 2026-08-24 market signatures — same failure class twice; see the incident notes inside `.gitattributes`).
- **Never `git checkout <path>` to "undo" an edit in a dirty, uncommitted working tree.** Checkout restores the committed baseline, not the pre-edit state — it silently destroys the entire uncommitted change for that path. This failure class has now hit auditors repeatedly (mutation spot-checks during audits: mutate → test red → `git checkout` to restore → uncommitted VP-3 work wiped, 2026-09-04; earlier researchers hit the same trap). Undo your own mutations with a reverse string replacement or reverse patch only; `git checkout`/`git restore` is reserved for deliberately discarding a path back to HEAD, said out loud in the report when used.
- **Separate edit feedback from delivery gates (2026-09-10, TS-1).** During editing use `scripts/gates.sh [test-filter ...]`: only relevant Rust tests, with no clippy/xwin/rustdoc/npm/E2E repetition. No arguments selects domains from staged, unstaged and untracked paths; unknown/shared inputs may broaden automatic selection to all Rust targets: inspect `--plan` and supply explicit narrow filters during development instead of routinely running that fallback. Zero matches or ignored-only matches fail. Development, review and ordinary handoff use only the smallest relevant checks. Run `scripts/gates.sh --full` once only when the owner starts remote release preparation or explicitly requests full verification; handoff alone is not that trigger. After repairs rerun failed/affected checks, not every already-green face. Full Linux CI and `scripts/ci-box.sh` invoke the same `scripts/gates.sh --ci` steps (including gated and adapter tests); Windows CI remains mandatory. Local `--full` additionally checks Windows statically with xwin. Use targeted stress only for a concrete unresolved concurrency concern. During development, verify platform semantics with affected tests; the complete Linux container and native Windows faces belong to remote release verification. The toolchain remains pinned by `rust-toolchain.toml`. Keep CI/container/gates changes in the same batch. See `docs/testing.md`.
- **Commits and pushes are performed by the 负责人 (repo owner) personally.** Developers and coding agents never run `git commit` or `git push` — not even after audit closure, and approval of the changes is not approval to commit. Deliver a working tree with the smallest relevant checks passed and docs updated; the owner controls remote release gates and personally reviews and commits.
- **DSH version upgrades and session or protocol compatibility work start from** `.agents/skills/dsh-session-upgrade/SKILL.md`. For explicit DSH design or architecture comparisons, use its lightweight source comparison workflow. Treat UI and interaction references as ordinary product design work. Claude Code users can read the repository skill directly or use the `.claude/skills` symlink to `.agents/skills`.

## Code shape and test discipline

For runtime, test, architectural and release-gate work, READ the relevant
extended rules in docs/engineering-policy.md before editing.

- Journal events and RunEvent variants have a single seat-table vocabulary
  with validation, projections, checkpoint/restore and live/replay parity.
- The core/frontend boundary is enforced by the Cargo graph and client ports.
  No new top-level production files in src/; split functions rather than
  adding/extending functions of 80 lines or more.
- During development use narrow scripts/gates.sh checks. Full gates are for
  owner-initiated release preparation, not ordinary handoff.
- Each fix needs an invariant-based pre-fix-red, mutation-discriminating test.
  Preserve behavioral tests, avoid UI copy/layout goldens and file flakes.
- Run scripts/code-health.py at round close. Counts and root code share must
  not regress. Record independent, repeated performance measurements.

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

Before changing architecture, READ docs/engineering-policy.md. The core owns
lifecycle, persistence, spawning, execution and permission semantics.
Frontends only render and collect input; they use EventSink / RunEvent and
client ports. Permissions use InteractivePermissionPolicy with injected
approvers. No UI-owned policy or core-to-frontend dependency. Update
public documentation for all user-visible behavior changes.

## State discipline (invariants before code)

Before persistent-state changes, enumerate invariants and every reader/writer.
Test full real sequences (resume, exit, reopen). Every bug fix gets a test
that fails before the fix and if that specific fix is deleted. Adversarially
review permission, path, concurrency, persistence and cross-session failures.
Build/tests passing alone does not prove correctness. Additional binding
requirements: docs/engineering-policy.md.

## Code Exploration & Analysis (Codegraph)

- **Codegraph Indexing**: This project supports CodeGraph indexing (identified by the `.codegraph/` directory).
- **Usage**: Always prioritize using `codegraph` to analyze code architecture, query symbol definitions, understand call flows, or determine the blast radius before making code edits.
- **Agent Tools**: AI agents should use the `codegraph_explore` MCP tool as the primary way to read and understand codebase context instead of relying on traditional `grep` or manual file reading.
- **Query Discipline**: `explore` is semantic retrieval, not exact file reading. For generic names, provide path plus call-chain anchors and avoid `maxFiles: 1`; use `codegraph node --file` for exact reads. Never mistake ranking noise for an indexing/parser failure. For files or custom configs that Tree-sitter doesn't support, fall back to grep or read_file.
