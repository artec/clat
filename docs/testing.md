# Development verification

Agent-driven Rust verification may use `exec_command` with literal `argv` and
`verification: {"format":"rust-test-summary","minimum_executed":1}`. Follow the
returned run-owned receipt through polls until terminal; zero matches,
ignored-only runs and incomplete scans are inconclusive. The direct process
exit and cumulative output signatures survive bounded diagnostic display.
They do not certify assertion relevance or source/test-binary identity. Keep
pre-fix and deliberate-defect checks independent, and retain the actual build
and executable identity when reporting discrimination. See [usage](usage.md)
for the execution and receipt limits.

The edit loop and the remote release gate have different jobs.
Development, review and ordinary handoff use the smallest relevant checks.
Do not run a full gate merely because a candidate is ready for review.
After a local edit:

```bash
scripts/gates.sh session::admission::   # one Rust area
scripts/gates.sh model:: tui::         # OR filters, one test harness build
scripts/gates.sh --plan                # inspect automatic selection; no build
scripts/gates.sh                       # choose from changed paths
python3 scripts/measure-inner-loop.py --package clat tui::model_editor::
```

Every gate invocation first checks Rust test code for unregistered blocking
`.output()`, `wait_with_output()`, `recv()` and `wait()` calls, including inline
test modules and test helpers. New waits must have deadlines. Existing exceptions
are recorded individually in `scripts/test-waits.allowlist.tsv`, with a source
fingerprint and review reason; this inventory is not evidence that legacy waits
are bounded. There are no file-wide exemptions. Removing or changing a registered
call requires updating its entry. Run `python3 scripts/check-test-waits.py` to
check the inventory without compiling.

The quick path runs only matching library tests. It does not also build the
CLI/integration binaries or run clippy, Windows cross compilation, rustdoc,
npm or browser E2E. Test output is concise. No matching tests, ignored-only
matches and command failures are errors, never a green result.
The quick path invokes Cargo once and checks actual passing-test counts;
it does not start a second harness just to list test names.

Explicit filters select the owning product harness: `tui::` and `dsh::`
build `clat`; runtime filters build `clat-core`. Mixed or unknown filters
retain both. Cargo default workspace members also contain both packages, so
full gates and native Windows CI do not omit the extracted core tests.
An initial workspace build is not a representative incremental-edit result.

Pure presentation filters (`tui::input::`, `markdown`, `model_editor`, `logo`,
`theme`, `popup`, `session_picker`, and `permission_picker`) additionally
select the lightweight `--no-default-features` harness. Runtime-backed TUI
snapshots, conversation/DSH integration tests and model persistence tests
remain in the default `runtime-tests` feature. An explicit persistence-test
filter selects the full harness. Ordinary Cargo tests, mixed filters and
delivery/CI gates retain these tests; the quick tier does not delete coverage.

Snapshot harnesses inject a fixed version before layout, shared by the header
and welcome view (including the narrow fallback), then mask that version in
the textual projection. Replacing the release version only after rendering
leaves length-dependent alignment and style ranges behind; version bumps
must not require refreshing otherwise unchanged snapshots. Normal rendering
continues to use the build version.

Runtime-backed snapshot fixtures authorize through the real TUI confirmation
gate and retain that single mounted application. Do not pre-authorize by
mounting, closing and immediately reopening the same storage root: that
introduces a lease-release visibility window under parallel macOS load.
Production lease acquisition remains one-shot and fail-closed; fixture fixes
must not add sleeps or silently retry a busy root. The fixture structure and
retained cross-thread exclusion have a regression guard.

Attached HTTP/SSE fixtures assign a distinct settlement identity to each
accepted prompt, matching the host contract. The input-history roundtrip checks
both terminal cards as well as recalled text: reusing one `prompt_rpc_id` can
otherwise make the second settlement look like a replay and turn event timing
into an intermittent timeout. Command-description changes must also check the
command-picker snapshots, since wrapping changes the popup height.

Task acceptance fixtures compile real Rust test harnesses with `-C strip=symbols`.
The evidence still hashes the executed binary and declared inputs before and after
execution; stripping irrelevant standard-library symbols keeps tiny fixtures
within the public 250-ms verification budget on the two-CPU Linux CI box. Do not
raise that production deadline or accept `inconclusive` as a passing result to
compensate for fixture size. The checker deadline, mutation and cancellation
counterexamples remain enabled. The test profile also optimizes the established
SHA-256 software fallback, keeping debug-only hashing overhead from consuming
that budget on ARM CI containers. Release settings and checker limits are
unchanged.

Process cleanup fixtures distinguish readiness from cancellation: wait for the
complete child-ready marker with a bounded deadline and report early child
exit, then keep the child waiting until owner cleanup. The cleanup assertion
must run before that child's fallback deadline, and the control arm must never
start. Capture-limit fixtures first establish executable readiness with a
bounded ready request, then keep the original byte and timeout budgets for the
measured calls. Controlled slow-start cases ensure first-launch delays cannot
masquerade as cancellation or byte-limit failures.

Process-recovery fixtures must finish children without consuming the output
whose retention they assert. The AG5 recovery test holds a child on stdin,
releases it explicitly, then waits with a zero-byte read budget; repeated
recovery listings must leave every child's output readable. Do not use a
draining poll helper to establish an unread completed-session fixture.
MCP cancellation fixtures wait for the marker's complete `cancelled` content,
not merely its existence: opening a file for writing makes an empty file
visible before the write completes. A controlled empty-then-complete reader
guards that distinction without changing the notification or latency assertions.

The measurement command reports build time, startup plus tests, artifact
size, cache freshness and total time. Run it immediately after an actual
edit. `--rebuild src/tui/model_editor.rs` explicitly requests a synthetic
mtime-only rebuild; `--budget 30` turns latency regression into failure.
It never signs artifacts, changes security settings or removes metadata.

Automatic selection includes staged, unstaged and untracked files. Known
domains include their principal consumers; shared contracts, fixtures, build
inputs and unknown domains can broaden selection to all Rust targets. Inspect
`--plan` and supply explicit relevant filters during development rather than
running that broad fallback as routine feedback. Documentation-only
changes skip Rust builds after static checks. This is a feedback heuristic, not a proof
of the complete dependency graph. CodeGraph `affected` is useful when choosing
an explicit filter, but missing graph edges must not waive delivery checks.
For adapter or browser edits run that component's targeted tests directly.

CLAT's test harness omits full debug information to reduce compile/link work;
dependency settings and normal development/release builds are unchanged. To
debug the harness with source-level symbols, temporarily set
the relevant `profile.test.package.clat.debug` or
`profile.test.package.clat-core.debug` to 2 in Cargo.toml.

Timing is end-to-end, not just compilation. On the 2026-09-10 development
Mac, a cached admission filter took 0.3 s, but a timestamp-triggered library
rebuild took 80.8 s overall despite compiling in 18.53 s. The new executable
waited before producing test output, with Gatekeeper evaluations present in
the system log. The tens-of-seconds edit-loop goal is therefore not yet
verified on this host. Do not disable system security checks to meet it;
cold executable startup needs separate host-side diagnosis. This measurement
is not a before/after benchmark of a substantive logic change or of CI.
A later unused-function removal with the final single-invocation entry took
65.0 s end-to-end (16.94 s compilation, 26 passing tests), so that startup
limit remains even after removing the redundant invocation.

The 2026-09-11 workspace experiment reduced a representative editor logic-edit
loop to 40.083 s (12.188 s build, 27.895 s startup/tests). The pure presentation
harness is 28.5 MB and its first measured build/run took 38.596 s (9.313 s
build, 29.283 s startup/tests, 33 passing editor tests). These are different
workloads, not a controlled speedup ratio; neither meets the 30 s budget.
The old harness was already linker-signed; re-signing and stripping disposable
build artifacts did not consistently remove the wait and are not part of
the workflow. No system security settings, provenance or quarantine
attributes were changed.

The second signing experiment on 2026-09-11 did not justify an automatic
signing hook. On six disposable arm64 copies of the same 28.5 MB harness,
linker-signed first launches took 0.415/0.573 s; re-signed launches took
0.522/0.311 s, plus 0.066/0.058 s for signing. Subsequent launches took
0.006–0.007 s. Unsigned copies were all killed (exit -9), with no tests run;
their termination time is not a performance result. Copies can share system
caches, so this is not an independent cold-start benchmark or proof of
Gatekeeper causality. Reproduce with:

```bash
# Obtain the executable from Cargo's compiler-artifact JSON.
cargo test -p clat --lib --no-default-features --no-run --message-format=json
python3 scripts/measure-signing.py /absolute/path/to/harness tui::model_editor::
python3 -m unittest discover -s scripts -p test_measure_signing.py
```

Two subsequent real expression edits (preallocating the truncation buffer,
then restoring the original allocation) took 70.182 s and 59.163 s inside
the measurement script: build 9.570/17.877 s, startup plus 33 passing tests
60.612/41.286 s. Both artifacts were freshly built, not mtime-only edits.
Both exceeded the 30 s budget. The temporary source edits were reversed;
no runtime change, security exemption, signing hook or background daemon
was adopted. Reusing a resident statically linked test process would test
old code after an edit; a watcher alone cannot remove that constraint.
This investigation is closed with the latency target explicitly unmet.

Only when the owner starts remote release preparation, or explicitly
requests full verification, run once:

```bash
scripts/gates.sh --full
```

This includes wait-inventory and selection-script tests, fmt, clippy, local xwin static checks,
rustdoc, all Rust targets, adapter build/tests and the ignored test face.
The adapter face also installs the exact-version official DSH web recipe and
runs its mocked MCP tests before the ignored Application tests. These use a
loopback DeepSeek-shaped server, temporary storage and fake credentials;
they never read the user's MCP configuration or call a paid provider.
For compiled-artifact acceptance and the additional Node-free/ambient-file/EOF
check, see [Official web tools](web.md#verification). Bun is required only
when building that artifact, not by the default CI fixture.
After a failure, repair and rerun the failed/affected checks; do not restart
every already-green check without a reason. Test failures are evidence to
investigate, not permission to retry until green.

Linux CI and the Linux container call `scripts/gates.sh --ci`, the same full
sequence without xwin. After the Linux CI gates, an always-run step removes
generated `target/doc` before Rust cache cleanup; rustdoc validation still
runs in the shared gates. The cache prefix excludes older caches containing
those docs, which rust-cache 2.9.2 can misidentify as test build directories.
This cleanup preserves compilation artifacts and does not run locally or in
the container, where documentation remains available for inspection.
Windows CI still runs native clippy and tests on every
PR/main push. Superseded CI runs for the same ref are cancelled. CI has no
path-based omissions; it remains the safety net for quick-selection misses.
The Linux gate step and each Windows test step have a 15-minute CI timeout.
Windows product and WASM backend tests run in separate steps so a later
successful native command cannot mask the product suite exit code.
`scripts/test_ci_windows.py` pins this workflow contract in the shared gates.

Windows native tests include `tests/windows_sandbox.rs`: the same binary's
restricted-token runner must allow workspace writes and reject outside writes,
deletes, Read Only overwrites and sibling scratch writes. These acceptance
cases cannot self-skip when the provider is unavailable. The suite also checks
private TMP/TEMP, Job cleanup and the explicitly open network boundary.
Use `scripts/windows-vm.sh sandbox` for a separate local Parallels checkout or
`scripts/windows-vm.sh full` for native clippy and tests. The script uses the
logged-in guest user; a SYSTEM/service token without a logon SID fails closed.
It does not modify guest security settings. Local VM results do not replace
the native windows-latest CI result after the owner's commit and push.

`scripts/ci-box.sh` runs the full Linux gate on a two-CPU container during
remote release verification (its only flag is `--cpus N`). During
development, select only the affected platform tests; do not launch the full
container gate for every platform-related edit.
The container source export uses Git's tracked files and visible untracked
files, preserving current modifications and deletions. Ignored historical
worktree copies and build outputs are excluded before the Git-less container
runs source scanners; tracked fixtures remain included even if an ignore rule
matches them. A NUL-delimited manifest preserves unusual filenames and symlinks.
The exporter requires Git rather than silently falling back to an unfiltered
workspace copy. `python3 -m unittest discover -s scripts -p test_ci_box_sources.py`
checks this contract; both full and CI gates run it through their shared script.
`scripts/gates.sh --stress N` remains available for a
specific diagnosed timing concern, rather than routine repeated full runs.
DSH checkout oracles remain opt-in with `DSH_CHECKOUT`; real-model/device
checks follow [Live validation](live-validation.md).

## Standing test rules (2026-09-11 consolidation)

The workbench tool suite covers native commit capture through real tools,
dirty baselines, failure/cancellation/restart, later-user conflicts, created-file
takeover, partial recovery/retry and no-follow/no-clobber races. File-browser
tests cover sensitive/traversal/symlink/binary/size/range/staleness limits;
organization tests exercise durable reload without deleting or deselecting chat.
Workflow action tests distinguish recreated goal identity from equal revision.
Process review tests pin owner/generation, bounded UTF-8-lossy tails and unchanged
output cursors/activity. Run relevant filters with `scripts/gates.sh` while editing.
`web/e2e/tests/workbench-tools.spec.js` exercises real-host PWA recovery, file
quotes, archive/restore and workflow controls, plus controlled notification and
task-rendering fixtures. Those fixtures do not prove native OS notification
delivery or mobile background execution; native ProcessService tests own log
semantics. Delivery still requires the full gate and native Windows CI.

Test volume is over half the codebase; these rules keep it an asset
instead of a tax. Each rule was paid for by a real incident.

- **Flake discipline.** Rerun a red test once; a second occurrence of
  the same signature opens a work case with the captured panic
  message and a reproduction attempt. A "known flaky" label without
  a filed case is forbidden. The `typert_era_round_trips…` test was
  red three times across weeks while carrying that label; the actual
  cause was silent POST loss on pooled connections (a real product
  bug), and the label cost two hours of CI ping-pong before someone
  traced the reply instead of the assertion.
- **Tests assert invariants, never implementations.** A test
  transcribed from the code just written shares the author's blind
  spots and cannot fail when the design is wrong. Every bug fix
  ships a test that is red on the pre-fix code; during review,
  spot-check that deleting the specific fix turns its test red
  (mutation spot-check).
- **Mutation builds must be isolated too.** An isolated source copy must
  use its own `CARGO_TARGET_DIR`, never the working tree's target directory.
  Identical local package identities and relative dependency paths can reuse
  a binary built from a different copy; reverting the source does not prove
  that binary was rebuilt. Require a successful compilation followed by the
  intended behavioral failure, then a rebuilt green restoration. If legacy
  shared-target experiments have contaminated the cache, invalidate the
  affected package's build artifacts and rebuild the authoritative tree
  before delivery, including the default-feature test face.
- **Serialize fixed-port lifecycle suites.** Do not launch two complete
  Cargo test faces concurrently: their separate test processes share port
  2691, but an in-process test mutex cannot serialize them. Run these faces
  sequentially; otherwise an address-in-use panic can also poison the
  remaining lifecycle tests in that process.
  Local `scripts/gates.sh --full` and all-target fast feedback probe the
  stable port before their Rust test face. If it is occupied, they enumerate
  the selected tests and print `LOCAL COVERAGE GAP`, the reason, exact count,
  and each skipped fixed-port test. These are filtered out, never counted as
  passed; the other lifecycle tests still run. `--ci` (or a set `CI` variable)
  refuses to skip and fails on an occupied port. A free port runs the original
  selection with zero skips. The probe does not stop or authenticate to the
  developer's host; a subsequent competing bind remains a real test failure.
  To supplement Linux fixed-port coverage, run `scripts/ci-box.sh` in its
  isolated network. The macOS-only browser handoff test still needs a free
  macOS port; have the host owner run `clat host stop` before repeating
  `--full` to close all native coverage gaps.
  Direct `cargo test` retains its port requirements and does not self-skip.
  Port-independent equivalents (`EQUIVALENT_TESTS` in the same script) cover
  trust refusal, occupied-port fail-not-hop, auth boundaries, a synthetic
  model roundtrip through a real `clat serve --port 0` child, cold restart
  with journal replay and dead-route survival on random loopback ports; they
  run even while 2691 is occupied and are never skipped — the script refuses
  a skip filter that would substring-drop them (`tests/host_random_port_lifecycle.rs`).
  The default-background-host-stays-2691 and two-launcher attach convergence
  semantics remain fixed-port-only (`NOT_RUN` locally while the host is up).
- **Isolated model tests arm the outbound guard.** Tests that claim a local
  synthetic HTTP provider should arm `clat_core::test_support::netiso::
  arm_outbound_guard` with the exact listener sockets: armed, the production
  HTTP adapters refuse any effective route that is not a literal loopback IP
  (IPv4 or IPv6) in the authorized set, ignore `ALL_PROXY`/`HTTPS_PROXY`/
  `HTTP_PROXY`, and do not follow redirects — enforced at `send()` time so
  adapters created before arming are constrained too. Arming is process-wide
  with scoped RAII tokens (releasing one token never disables a still-live
  scope), so those tests live in their own test binary
  (`tests/netiso_outbound_guard.rs`), never in the shared lib test process.
  All guard rejections are desensitized: scheme/host/port only, never
  path/query/credentials, and unparseable URLs report only their byte length.
  The guard is inert in production and unarmed builds; offline scripted models
  never pass through it. Real `clat serve` child processes arm the same guard
  through the strictly test-only `CLAT_NETISO_ALLOWLIST` environment entrance
  (`literal-loopback ip:port` entries). The entrance is fail-closed: unset
  keeps ordinary production behavior, while a set-but-unusable value (empty,
  malformed, or non-loopback entries only) arms a deny-all scope that refuses
  every provider HTTP request instead of silently continuing unprotected. It
  constrains route/redirect/proxy outbound across cold restarts — see
  `tests/host_random_port_lifecycle.rs`.
- **Scan visible test sources, not evidence archives.** The wait guard uses
  Git's tracked and nonignored untracked Rust paths, including new test files;
  tracked paths remain covered even if an ignore rule matches them. Exported
  source copies without Git metadata use the directory walk with `output/`
  and build/dependency directories excluded. The 96-site registration rule
  still rejects new waits and stale entries; historical evidence is preserved.
- **Do not accumulate duplicated scaffolding.** When two test suites
  share setup/assert sequences (the providers adapters' test sections
  carried 97 duplicated normalized blocks while the production code
  was only 5% alike), merge on touch. Helper duplication has already
  caused one test-infrastructure incident (two copies of a
  temp-root helper caused cross-test collisions).
- **Test placement.** Harness-heavy or runtime-backed tests live
  behind the `runtime-tests` feature so pure-presentation edit loops
  keep the lightweight harness; delivery gates always enable it. A
  refactor that moves or splits tests must reconcile counts at
  delivery — the 2026-09 workspace split was accepted only after
  328 (root) + 970 (core) was shown to equal the exact pre-split
  1,298-library-test face.
- **Source-scanning tests must normalize `\r\n` on read.** Windows
  autocrlf checkouts rewrite line endings; an exact-string split
  needle silently matched nothing and the scan covered the whole
  file including test code, failing CI only on Windows (2026-09-11).
  Every `fs::read_to_string` feeding a string-shape scan normalizes
  line endings before matching.
- **Measurement protocol.** Latency and performance claims need
  repeated samples in a clean machine state; a single timed run is
  anecdote, not evidence — one 59–70 s inner-loop sample on 2026-09-11
  was an environmental outlier while the clean-state acceptance
  measured 8.2 s for the same edit path. Acceptance numbers are
  re-measured independently by the reviewer, never copied from the
  implementer's report.

### Signed plugin-market acceptance

The PLG-2 browser runner uses ephemeral fixture keys, a loopback market/provider,
a compiled native package and fresh host storage. Production binaries have no
market trust override. After building CLAT, the adapter and recipe, run:

```bash
node web/e2e/plugin-market-staging.mjs \
  --package /absolute/compiled-package-directory \
  --clat /absolute/path/clat --target aarch64-apple-darwin
```

Author-side Node/Bun, Minisign and Playwright dependencies are required. The
runner copies the package, signs that private copy, and removes test keys and
publication files on completion. It exercises consent refusal, private local
configuration, a real MCP process/search fixture, disable/enable and clean
uninstall after an earlier run froze tool registration. It also tests optional
configuration omission versus `false`. This establishes local staging behavior
on the package's platform; production signing, HTTP headers, FTP deployment,
other platforms and a paid-provider call remain separate acceptance steps.

### PLG-3 author experiment

`scripts/gates.sh --full` / `--ci` install the locked `sdk/dsh-wasm-spike`
compiler toolchain and run `npm test` there after the official web recipe. The
suite preserves the original inputs, captures strict original bundling and
componentization failures, and creates an isolated JS engine component. It arms
`plg3_component_engine_probe` with an absolute temporary component path and the
core's `test-support` feature; exactly one Rust test must run. That test uses the
existing production WIT/WASI linker, memory/fuel limits and cancel watcher.
An unarmed ignored Rust invocation is not evidence of engine acceptance.

The engine probe checks Promise/generator/EventBus primitives, private config
states, host write denial, a bounded fuel-burning computation, epoch cancellation
and the disabled HTTP trap. It does not certify the complete Shim lifecycle or
official search. Optional repeated measurements and retained failure artifacts
are documented in [the experiment README](../sdk/dsh-wasm-spike/README.md).
No experimental component or test publisher key is shipped in production market
metadata. Windows's Rust suite still covers compilation; the JS experiment's
author-toolchain execution runs on the Linux gates job and local author machine.

The author suite also compares both lowered XID classes against native Bun over
all Unicode code points, including surrogates. Locked property tables and actual
membership digests detect drift; runtime version metadata alone is insufficient.
The guest executes representative original-pattern compositions, and a separate
unlowered build must fail at initialization. This syntax acceptance does not
certify HTTP/DNS or async-context compatibility.

The shared `clat-wasm-net` library runs its ordinary backend invariants in
Linux/local delivery gates and Windows CI. Generated-component probes require
explicit local artifacts and are run separately; ignored or unarmed probes are
not component acceptance. `PLG4_TOOLS_COMPONENT` must be an absolute path for
`plg4_original_component_installs_mounts_rejects_undeclared_and_uninstalls`.

The network search staging runner is web/e2e/plugin-network-staging.mjs. It
checks generated-v2-catalog routing, signed install consent, private config,
three actual approvals, original search results and uninstall. The numeric
route is limited to the test host, not product network permission. Explicit
live and fixture sampling commands are in the
[second-flavor README](../sdk/dsh-wasm-flavor/README.md).

Local/Linux full gates also run the flavor and v2 release JavaScript contracts
(`node --test market/scripts/*.test.mjs sdk/dsh-wasm-flavor/*.test.mjs`). Network
staging runs the signed installation/update/rollback/restart/uninstall matrix
before its browser test; a compressed original search response is included.
The explicitly armed formal HTTP component shares the six wire encodings with
the unchanged original provider under Node. Backend ordinary tests cover
encoded/decoded ceilings, allocation pressure, empty-member cancellation and
physical socket closure. Node encoding success does not certify a particular
Bun runtime's undici implementation.


### Plugin startup performance

PERF-1 has a six-target compile matrix in CI and repeated SHA comparisons on
native macOS, Windows and Linux runners. Run the same CPU comparison locally
with `cargo test -p clat-wasm-net --lib perf_sha_samples -- --ignored --nocapture`
(and again with `--release`). It compares five 61 MiB inputs with identical
SHA-256 results. x86 deliberately retains the existing backend. Compilation
checks and CPU comparisons do not establish host startup or device acceptance.

For an idle local host, with explicit permission to disconnect its frontends,
`scripts/measure-plugin-startup.py --allow-stop-host --binary before=/absolute/old/clat
--binary after=/absolute/new/clat --output output/perf1/startup.json` measures five
actual `host start --trust` process restarts per binary against the existing
local plugin configuration. Stop competing builds and tests first. This leaves
the host stopped; restart it with the chosen binary after measurement. OS disk
caches are retained, so the report is process startup rather than cold disk I/O.
The command never changes the plugin configuration.

To compare a compiler-cache miss and authenticated cache loading for an actual
component, set `CLAT_PERF_COMPONENT` to its local path and run
`cargo test -p clat-wasm-net --lib perf_component_samples -- --ignored --nocapture`
(and again with `--release`). The fixture must be trusted and supplied explicitly.
Three samples use fresh engines and the production network engine settings.
The miss includes compilation, serialization and atomic cache publication; the
hit includes content hashing, authentication and loading. This is not complete
host startup. The reviewer independently repeats final acceptance.

On Windows, capability directory handles deny delete sharing to protect path
lookups. Cache tests must release directory capabilities before removing their
fixture trees. The cache ownership regression keeps both compiled and loaded
components alive while replacing and deleting cache artifacts, then instantiates
both components to verify that production code owns no cache file handles.

### Local browser authentication and handoff

The web E2E helper opens a fresh local workbench without pairing or seeded
credentials. Browser authentication tests cover silent session exchange,
localhost canonicalization, legacy storage migration/unavailability, and
credential-error classification. Intercepted Chromium requests omit Fetch
Metadata: `fetchBrowserRequest` forwards full headers and restores same-origin
metadata only when the real frame, Origin, and target agree. It never changes
the production fence. Socket tests independently reject absent browser metadata,
cross-site requests, and unknown uncredentialed authorities before body reads.
The macOS lifecycle probe records the system opener instead of opening windows;
ordinary lifecycle children suppress browser handoff. Manual GUI validation
checks the real default browser separately. A stop response precedes complete
teardown, so fixed-port tests wait for both listener and storage lease release.

## Runtime refactor regression checks

Behavior-preserving runtime changes reuse the existing Application and Agent-loop
contracts. During editing, select the affected filters explicitly:

```bash
scripts/gates.sh plugins::process process::
scripts/gates.sh context_tests goal_tests plan_mode_tests skills_tests memory_tests
scripts/gates.sh run:: application:: goal:: subagent:: session::recorder
scripts/gates.sh plugins::conversation plugins::services utility_tests todo_
cargo test -p clat --test architecture_boundaries
```

Test-support conditional-compilation changes also need a normal production check
(`cargo check -p clat`). All-features checks compile the test-only consumers and
can therefore hide unused imports or fields in the ordinary build.

These are per-batch choices, not a requirement to repeat every command after each
edit. Confirm that the relevant core tests actually execute; a frontend target
with zero matching tests contributes no behavioral evidence. Reconcile test
identities/counts across module moves instead of weakening assertions or
refreshing golden files for an internal-only change. Recovery checks distinguish
classified errors from lookalike text, and lifecycle checks exercise permission
denial, cancellation, panic, durable failure, cross-run process fencing and cold
reopen. Targeted mechanism-removal controls must fail for their intended
behavior; they do not constitute model-effectiveness evidence.

## Agent effectiveness evidence

The scripted Agent Scenario Harness exercises the real Application, permissions,
tools and journal. `scripts/gates.sh agent_eval` includes its control export and,
on Unix, independent ordinary-process-group/scratch teardown witnesses. These
prove the tested mechanism; they do not establish real model effectiveness or
containment of an adversarial candidate.

The developer-only [Agent campaigns](../tests/agent-campaigns/README.md) directory
provides preregistered historical-function microtasks, external behavioral oracles
and raw-record recomputation. Run its Python contracts explicitly with
`python3 -m unittest discover -s tests/agent-campaigns -p test_eval.py`. Paid runs
require separate owner authorization and verified candidate isolation. A scripted
control or historical reference-fix result must not be reported as a model
success rate or capability improvement.

## In-run context maintenance

Run the deterministic context-maintenance contracts with:

```bash
scripts/gates.sh application::tests::active_compaction plugins::conversation::compaction application::compaction::alignment::tests
```

The Application cases drive the production provider registry, retry wrapper,
Agent loop, native file tools, compactor, recorder, durable admission and cold
session replay. They cover a single user turn with repeated parallel tool
batches and multiple checkpoints; steering arriving during a summary; no
summary below pressure; summary refusal, cancellation through the actual run
handle, shared cumulative budget, and an injected persistence failure.
Original tool observations stay in the log and untouched live tool results
remain structured JSON. The fixtures use temporary project/storage roots and
a scripted provider; they do not call a real model or the resident Host.

The conversation/alignment cases cover complete call/result batches, protected
users and owned checkpoint identities, duplicate/missing anchors, unchanged
image/provider-state tails, UTF-8 chunk reconstruction including tail evidence,
and actual/fallback accounting for repeated summary attempts. A cancellation
regression requires normal cancellation to settle before projecting an unused,
still-oversized request, while a maintenance persistence error keeps precedence.
These contracts establish execution and recovery behavior, not summary quality
or autonomous task success rates with a real provider.

## Durable task recovery (GA-1)

Use `scripts/gates.sh task:: application::task_recovery_tests::` for task state,
admission/CAS, full-fold/checkpoint parity, capability-relative artifact checks,
conservative intent/result crash windows, explicit human resolution and reserved
budget accounting. The Application scenarios use scripted Providers and temporary
projects; the report scenario reads materials, produces a CSV, exits/reopens and
explicitly continues to a report while preserving prior bytes and operation
counts. Internal compaction must preserve questions, task facts and usage.
The rework regressions cover Goal create/resume/arm/run exclusion (including
unarmed creation and cold conflicting state), failed Skill resolution before
admission, crashes before/after admission but before the task turn binding,
binding persistence failure, and verified/unverified/text-model tool visibility.
Model changes must invalidate old recovery checks without executing a request.

Run `scripts/gates.sh exec::tests::ga1 serve::tests::ga1 tui::snapshot_tests::ga1`
for the actual headless, local-socket command/SSE and terminal-command mappings.
These are deterministic product-contract checks, not evidence of improved real
model autonomy or an external job provider's behavior. Job recovery uses an
injected local fake, with unavailable/disconnected/expired/deadline/cancel cases.
Fault and deliberate-defect checks must retain the first failing output and
restore source using reverse patches. A green compile is not runtime evidence.
Do not run full release gates or touch the live host for this development task.

## Task acceptance and completion gates (GA-2)

Use `scripts/gates.sh task:: application::task_acceptance_tests::
application::run_lifecycle::acceptance_cleanup_tests:: plugins::process::acceptance::`
(on one shell line) for contracts, bounded checks, producer identities,
currentness/CAS, human authority, reports, final disposal and persistence failure.
The programming sequence compiles/runs a real temporary Rust test harness,
observes ignored-only/failing/passing receipts and explicitly authorizes a new
invocation after the cross-Run replay fence. The records sequence independently
checks JSON source rows, i64 totals and report text, repairs wrong totals, submits
a model candidate and cold-reopens the completed state. Both use temporary roots
and scripted Providers; neither calls a real model or validates an external host.

`tests/fixtures/task-acceptance-v1.json` pins admission, full fold, checkpoint/tail
and completion provenance with LF bytes; the unchanged GA-1 golden remains a
separate compatibility assertion. Adversarial cases cover combined/forged
transitions, cross-task producer IDs, stale bytes/contract/revisions, omitted
clauses, missing/unsupported/invalid/oversized sources, symlinks, cancellation,
checker panic/deadline and changes during checking. Large Task DTO tests page
all operation/report identities under the generic pruner's size threshold.
Internal compaction must preserve confirmed rules and saved report identities
outside the model's summary, then permit user completion after normal settlement.

Use `scripts/gates.sh ga2_headless ga2_tui ga2_pwa` for real headless command,
terminal key/scrollable-view and local socket/PWA mapping sequences. Read-only
checking must leave Task revision and durable bytes unchanged; explicit saves
advance CAS. Run-candidate cases exercise later file effects, no user opt-in,
cancellation, panic, actual journal/ledger/disposer failures and an unrelated Run
that must not revive an older candidate. Fault checks and deliberate-defect
checks retain original failing output and restore only their own reverse patch.
These local checks do not replace independent review, remote Linux/native Windows
CI, real-model dogfood or release acceptance. Ordinary handoff uses these affected
checks and architecture/shape hygiene; full release gates remain owner-triggered.


### GA-2 review regressions

`scripts/gates.sh ga2_rework_ ga2_actual_run_` exercises the review repairs using
real Application workers and native commands in temporary projects. Final gate,
Task settlement, scope disposal, journal and final-completion commit failures
must agree across the completion channel, exactly one live terminal and the
journal/replay reason. Success persists adjacent Task completion/turn-end facts
in the same final atomic group, checked again after cold reopen. No success
terminal is emitted before these gates.

Both native command wrappers are exercised in both directions across cold
reopen. Replaying argv/cwd without a grant must leave the executable's actual
counter unchanged; one user grant produces a new operation/receipt under normal
permissions and spend accounting, and a later Run cannot reuse that consumed
grant. Shell aliases/resolved workdirs share the fence. A running exec launch
and its real stdin poll remain inconclusive, without adopting the poll as a new
producer. These are local contract regressions; remote/native Windows release
verification and independent re-review remain separate.

### GA-3 capability discovery contracts

`scripts/gates.sh ga3_` covers registry/legacy compatibility, native and real WASM
component mapping, exact/Chinese/English matching, explicit I/O/scope differences,
actual JSON byte bounds and schema chunk reconstruction, frozen visible sets,
lease/Run/session invalidation, cancellation, concurrency and read-only queries.
The explicit local MCP fixture is gated:
`cargo test -p clat-core --lib ga3_mcp_fixture_discovery -- --ignored`.
It starts a temporary Python fixture during setup, then asserts ten query/detail
cycles send zero new MCP messages before a separate actual tool call. This is a
test-only dependency, not a new product runtime requirement.

Application scripts reuse GA-2 acceptance scaffolding for JSON→table/report and
real Rust harness receipt→acceptance. They inspect actual query ids/schema, run
normal tools, retain failed/inconclusive gates, deny replay without a grant and
consume one explicit grant for fresh runtime evidence. Discovery adds no Task
operation; normal model tokens remain accounted. Headless commands, TUI Enter
and current PWA `command.run` use the same Application content, with attached
TUI argument preservation checked separately. Existing GA-1/GA-2 regressions
remain relevant; a scripted provider is wiring evidence, not a natural model
selection-quality measurement.

Adversarial checks remove visibility, generation or required missing-condition
mechanisms and inject an unintended target call/approval bypass in temporary
projects. Each claimed discriminator must fail at runtime; reverse patches must
restore the exact pre-mutation bytes. Keep invalid fixture observations separate
from product defects. Architecture, fmt, diff and code-health close ordinary
handoff; remote Linux/native Windows, full release gates and independent review
remain separate owner-controlled acceptance.

GA3-R1/R2 regressions preserve ordinary configuration words and colliding property
names in a real local MCP registration, compare published model definitions with
details, and reject a separate tool containing a protected structural value.
Shared legacy-tool tests cover nested annotations, opaque `const`/`enum` data,
secret constraints, full chunk reconstruction and both query domains. An
Application script compares actual ModelRequest schemas with user details and
checks normal persisted/replayed discovery results for the fixture secret.
HTTP transport checks exercise the same boundary without network requests.
Temporary mechanism removals must make annotation, structural-rejection and
ordinary-configuration regressions fail at runtime, then restore exact bytes.

The explicit local stdio regressions for credential environment echoes run with
`cargo test -p clat-core --lib ga3_mcp_ -- --ignored`. A shared Python fixture feeds
both adapter registration and an Application script: root descriptions, actual
ModelRequest definitions, idle/model schema chunk reconstruction and normal
persisted/replayed results must exclude the bare configured `API_KEY` value.
Separate unsafe tools still reject credential-bearing constraints; ordinary
mode/format words retain their schema semantics, and discovery sends zero MCP
messages. These are local fixture checks, not external-provider acceptance.
