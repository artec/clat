# Development verification

## Web and Desktop presentation changes

Automated tests protect behavior, data consistency and security boundaries.
Ordinary interface wording, colors, icon shapes, exact spacing, menu counts and
DOM arrangement are not fixed CI contracts. Copy/style/layout-only edits do not
require new automated tests. Review representative screenshots for visual
quality; do not introduce full-page pixel goldens or a language × theme ×
viewport × page test matrix.

Use stable action/test identifiers and assert operation results, draft
preservation, ownership, state transitions and permission outcomes. Localization
checks protect keys, parameters, references and catalog format while allowing
translation wording to improve. Assertions about actual user input, file
contents, tool output, wire fields and signed/byte-exact artifacts remain valid.

When a Web/Desktop component moves, remove or merge brittle presentation
assertions with a recorded mapping to the behavior coverage that remains.
Investigate whether a failure represents a product regression or an obsolete
UI expectation before changing it; do not repeatedly refresh strings or
screenshots merely to keep a low-value assertion green.

The ordinary Web/Desktop suites do not capture review screenshots. Explicit
manual captures live in `web/e2e/manual/` and run with
`npx playwright test --config playwright.visual.config.js`; they do not run in CI
and have no image-comparison baseline. Visual acceptance belongs to the owner.

The October 2026 presentation cleanup removes the following checks without
dropping their useful behavior coverage:

| Removed presentation contract | Retained coverage |
| --- | --- |
| Code-block border width and Markdown header alignment | Fenced-code text, table cells, escaped pipes, safe links and rejection of raw HTML remain tested; visual treatment is manually reviewed. |
| Shell dimensions, title ellipsis and exact reading offsets | Full user titles survive rename/reload; loading history keeps the reading message visible. |
| Repeated language × collapsed-navigation checks and control spacing | Navigation retains drafts; live language switching retains composition, selection and pending operations. |
| Plugin panel wheel/viewport geometry | Catalog search, local-token protection, permissions and installation/configuration outcomes. |
| SVG presence/count/shape and composer DOM order | Send/stop call the correct action, model selection works, Escape restores focus, and native close closes its panel. |
| Routine screenshots and repeated translated display strings | Manual screenshots for appearance; stable action identifiers/catalog keys for behavior. |
| Exact inventory of visible public links | Explicit external-destination allowlist; unknown addresses and credential-bearing URL variants remain rejected. |

Feature-page focus checks cover navigation ownership and keyboard reachability:
opening a page must not steal focus, and Tab must still reach its actions. They
do not assert outline colors or shapes.

Keep semantic checks for message chronology and protected content. A tool result
appearing in the wrong turn or a late response erasing a draft is a behavior
regression, even when the symptom is visible in the interface.

Cold-reopen fixtures pass fallible mount results to the shared bounded lease
retry helper; wrapping a panic-producing mount in `Ok` bypasses that protection.
Random-port Host checks correlate live or replay terminal frames with the
submitted client message, so an older turn cannot finish a newer request's wait.
The port-independent lifecycle inventory covers the `random_port_` cases,
while pure SSE predicate unit tests remain ordinary tests in the same binary.
Held-lease fixture probes run on a different thread because Windows named
mutexes permit recursive acquisition on the owner thread.
Startup probes extract the actual listener URL without matching translated
announcement copy. These checks retain storage exclusion and outbound-isolation
assertions.

## Targeted verification

Localization error-path regressions use `scripts/gates.sh host_client::
model_editor::remote serve::tests::`, `bun test scripts/i18n-check.test.ts`, and
the `i18n.spec.js` / desktop shell browser tests. These cover Host build/trust
rejections, startup and serve argument recovery, hidden profile fields, failed
image staging and local validation errors across live language changes. The
catalog checker guards direct construction of ordinary JavaScript Errors from
translated messages; aliases and raw diagnostics still require review.

Native Web contracts run with `scripts/gates.sh native_web`. They cover the
model → permission → tool → model path with an injected transport,
configuration sharing, zero-dispatch denials/cancellation, redirect boundaries,
response limits, secret redaction and source retention through pruning.
`web/e2e/tests/native-web.spec.js` exercises configuration and three-language
switching against the isolated workbench Host. The separately armed
`CLAT_WEB_PUBLIC_PROBE=1` ignored `native_web_public_http_probe` uses anonymous
public HTTP only; mock API results do not establish paid-provider availability.

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

DSH 0.2.1-rc.2 metadata admission has focused coverage in
`session::dsh_golden::rc_021::`: native required records, malformed payloads,
generation windows, opaque plugin extensions, checkpoint cuts, incremental
replay, and resume/append/close/reopen without rewriting source bytes.
Run `scripts/gates.sh session::dsh_golden::rc_021::` for this contract. Its new
goldens regenerate with `tests/fixtures/dsh-session/gen-v4-021-fixtures.mts`
from the exact DSH revision named in that script; historical generators and
adapter cohorts retain their own pins. Set `DSH_CHECKOUT` only on the explicit
`dsh_and_clat_exclude_each_others_session_writer` test when checking a new
upstream lease implementation, rather than exporting it across other cohorts.

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

The four macOS/Windows targets also warm shared Release dependencies on
default-branch cache misses. The two Linux targets skip shared-cache restore/save
and extra warming;
their existing checks and six-platform release inventory are retained.
Native runners, explicit target directories and cache identities match
the release inventory; PRs and tags do not publish the shared cache. Extra warm
builds are skipped on exact hits and never package or publish artifacts.
`bun test desktop/scripts/ci-contract.test.ts desktop/scripts/cache-reuse.test.ts`
checks that contract and runs disposable, offline Cargo workspace experiments:
release-library warming survives a product version bump, while debug artifacts
remain distinct. An additional core/CLI/GUI fixture gives the products different
features on a shared dependency: core-only warming must miss for the CLI, the
CLI warm command taken from CI must prepare the full release graph, and separate
CLI/GUI variants must remain fresh after a product version bump. The workflow
contract also requires the CLI warm and Release build commands to match exactly
and all cache readers to use v4, excluding incomplete immutable v3 snapshots.
The experiments honor `rust-toolchain.toml` and do not use
the user's target directory. It verifies local Cargo reuse, not GitHub cache
access or a promised speedup. These cache preparation steps belong to the
parallel platform jobs; the sequential Linux/container gates stay unchanged.

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

The GitHub `Complete Linux gates` step reserves 30 minutes for the complete
sequential `scripts/gates.sh --ci` sequence, including cold compilation and the
real JS component probe. Individual tool, checker and test deadlines remain
unchanged. `scripts/test_ci_windows.py` also checks this aggregate budget and
keeps the Windows Cargo commands in separate failure-preserving steps.

Real compiled task-acceptance fixtures allocate their temporary project before
building argv, then use that project's absolute executable path in both the
invocation and acceptance contract. This avoids Windows PATH/parent-directory
lookup for a just-built child binary. Escaped-path read regressions retain at
least 640 JSON escape characters on every platform: Unix uses quoted directory
names, while Windows uses legal repeated path separators. Both still resolve
the actual file, consume all segments and reconstruct every source byte; they
are not skipped on Windows.

Real Host subprocess fixtures in `ag4_joint_real_host` and
`ag5_joint_real_host` share `tests/support/serve_stdout.rs`. Reporting the
listening port must leave the reader draining stdout until EOF: the Host still
prints after its readiness announcement, and closing the pipe can terminate it
before an SSE client subscribes. Their controlled regression holds back later
output until readiness is received, then requires that output to be consumed
through EOF. Child stderr is inherited so a future reset exposes Host panics
instead of hiding them behind an SSE error. No reconnection retry masks the
first failed subscription.

## Desktop verification

Desktop is an optional native client and does not add GUI dependencies to default
CLI/core gates. Run targeted `scripts/gates.sh serve:: host_client:: upgrade::tests::`
for shared Host/cache/upgrade changes and the architecture boundary test. Desktop
native checks are `cargo test -p clat-desktop --locked` and
`cargo clippy -p clat-desktop --all-targets --locked -- -D warnings` on macOS/Windows.
Automatic native verification runs in CI's existing four macOS/Windows platform
jobs with an explicit `--target`; the dedicated Desktop workflow retains manual
single-target/all-target native checks and optional Windows test installers.

The Desktop planning job validates language keys, parameters and references; it
does not compare wording or appearance. Install the locked desktop development
dependencies before invoking this checker, which uses the TypeScript JSON parser.
Keep this ordering on automatic pushes as well as manual runs. A developer's
existing `node_modules` directory is not evidence that a clean runner can execute
the same check.

`bun app:test` covers typed
assets, archive fencing, version authority, signing environment isolation and
partial-release rejection. `npm --prefix web/e2e test --
--config=playwright.desktop.config.js` runs isolated shell UI tests through the
existing Playwright dependency. These tests mock native commands and do not start
a real Host, contact a model or install updates.

Rust update tests use an independently generated throwaway key and signed fixtures
pinned `-text` in .gitattributes. They exercise the official updater verifier,
package tampering, version inflation/downgrades, wrong targets, index tampering and
production/fixture trust-root separation. On macOS one test uses the real native
installer to replace a disposable dummy bundle, verifying that the separate CLI
and leased backend remain untouched. It never updates an actual installation.
Host regressions verify observer subscriptions do not exhaust project capacity,
notification session routing is frozen at admission, readiness reads are inert,
and cache leases protect executing runtime slots.
Desktop release-script tests also run inside the shared full/CI gate; Linux does
not build the native shell. ARM64 xwin checks require `--cross-compiler clang`
for ring's ARM assembly; the default clang-cl path mixes incompatible include flags.

Use debug CLAT_DESKTOP_SMOKE=1 for isolated native-window checks. Native Chinese
IME/clipboard/drag-drop/scaling, Windows installation, real N-to-N+1 installation,
Apple notarization and final signed artifacts require their own platform evidence;
browser tests, mock-runtime verification and xwin compilation do not establish
those conclusions. Record exact checks and unverified faces at handoff.

Source-shape statistics include desktop/src as production code. Compare against
the same script's --revision HEAD result; no new >=80-line production function
and no extension of an existing giant is accepted.


### Desktop local commands

From the root, `bun start` stages the private backend and opens native desktop
development mode; `bun app:build` packages this machine's test installer without
production signing or upload. `bun app:test` runs desktop tooling contracts,
including architecture detection, exact release inventories, checksum/path
binding, upload/publication ordering and rejection of replaced remote files.
Signing-key fixtures are temporary and never read the owner's real private keys.
These checks do not establish native installer, Apple notarization, Windows UI,
or remote GitHub release acceptance. The owner's existing `./publish` is the
single formal signing/upload/publication entry and is not a test command.

Desktop tooling contracts are Bun/TypeScript tests (`*.test.ts`) and replace all
26 earlier Python desktop-tool contracts. Type checking runs with
`bun desktop/node_modules/typescript/bin/tsc --project desktop/tsconfig.json`.
CI and the Linux container pin Bun 1.3.14. Real temporary Minisign encryption
and signing checks require Minisign and report skips explicitly when unavailable;
macOS-only DMG/owner-platform checks and POSIX flock checks are platform-gated.
The existing unrelated Python Rust-test fixtures remain in their own domains.
Signer regressions use a disposable password-protected key with the actual
pinned Tauri CLI and independent Minisign verification. Unbroken Base64, trailing
LF and wrapped CRLF must all sign without changing the source key. Temporary
key copies use permissions 0600 and must be removed on success or failure;
passwords remain outside process arguments. The owner's real key is never read.

Desktop push/PR verification covers all four native platforms in CI without a
second set of native build jobs or installer generation. CI and Desktop use
`desktop/scripts/check-plan.ts` to classify the complete push or PR merge diff.
Rust/shared-core, native resources/configuration, toolchain/workflow and unknown
changes retain native clippy/tests; scripts select tooling. Native bridge
JavaScript/HTML and its browser harness select the desktop web-shell suite;
PWA-only source changes do not select that unrelated mock suite. Known CSS,
icons, manual captures and catalog wording edits do not select Desktop native,
tooling or web-shell jobs. Planning validates language keys/parameters once,
without installing desktop dependencies or computing an installer matrix on
automatic runs. Language runtime/manifest/checker changes still select all
Desktop checks. Docs-only changes add no desktop jobs. Multi-commit pushes, deletion,
rename and NUL-separated paths are covered; missing/invalid history selects all
checks. Tooling still runs on all four native platforms when relevant; fast
Linux tooling/type checks and browser tests stay in Desktop, and the ordinary
Linux/container and Windows product gates are retained.

Native CI jobs share one target tree/cache for core, desktop and Release
preparation. Only successful main native-input runs save complete snapshots;
PRs, script-only runs, manual Desktop checks and Release do not save them.
`CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` apply only to platform
CI and matching cache readers. Assertions, overflow checks, optimization and
local developer profiles are unchanged. The isolated Cargo test exercises real
linking/reuse and executes assertion/overflow cases, rather than inferring safety
from workflow text. Validate routing/profiles with
`bun test desktop/scripts/check-plan.test.ts desktop/scripts/ci-contract.test.ts desktop/scripts/cache-reuse.test.ts`.
These local contracts do not establish a remote cache hit or a timing result.

`bun app:test` also verifies this workflow boundary, the
Windows-only formal CI matrix, and local macOS staging contracts: clean/tag-bound
source, interrupted signing reuse, changed-byte rejection and rejection of source
changes during a build. Stage tests use disposable files and injected build/sign
operations; they do not use the owner's Apple credentials or prove native builds,
notarization or remote timing. Formal macOS packaging runs on the owner's Mac via
`./publish`; manual Desktop workflow installers are optional Windows test artifacts.
Backend/release-build tests additionally cover official CLI receipt identity,
PE/Mach-O architecture, unchanged staging without backend compilation, restored
executable permissions, reuse while other platforms are running, rejection of
foreign tag/commit/run or expired artifacts, and bounded failure/timeout handling.
Remote orchestration tests inject GitHub responses; they do not initiate jobs,
read credentials or establish actual GitHub artifact timing.


### CLI self-update progress

`scripts/gates.sh upgrade::` covers signed-manifest/hash checks, extraction
fences, atomic replacement/rollback and progress phase ordering. New progress
contracts check read-only `--check`, already-current/offline/missing-trust-asset
paths, rejection before extraction, executable validation, real fixture extraction
and success only after replacement of a disposable binary. The compressed fixture
is pinned `-text` in .gitattributes. No live executable or Host is replaced.
The extraction test also checks gzip framing before invoking the real extractor:
BSD tar may accept a plain tar mislabeled `.tar.gz`, while GNU tar rejects it.
The fixture must be genuinely gzip-compressed with the same executable payload;
Linux container coverage uses GNU tar rather than relying on a macOS-only pass.
`cargo test -p clat --bin clat upgrade_cli:: --locked` verifies CLI line rendering
and terminal outcomes independently of the core observer. Tests do not download a
real release or establish remote update acceptance.

### Concurrent first Host startup

`scripts/gates.sh control_storage::sentinel:: session::root_lease::` checks
new-directory ownership before sentinel publication, zero publication on lease
failure, and a competing initializer's ability to complete. The three
`fresh_directory_` contracts fail with publication before the ownership check;
the lease interleaving is deterministic and adds no sleeps. Unix-only cases
exercise directory `flock`; Windows retains its stable named-mutex identity.
The `serve_lifecycle` integration target exercises two real launchers and Host
readiness. Run it in the isolated Linux container when the local fixed Host port
is occupied; do not stop the owner's Host to obtain a test result.

### Compact release index (v2.1.2)

Run `scripts/gates.sh upgrade::`, `cargo test -p clat-desktop policy::`, and, on
a supported native platform, `cargo test -p clat-desktop updater_tests::`.
`bun test desktop/scripts/release-index.test.ts desktop/scripts/workflow.test.ts desktop/scripts/local-macos.test.ts`
covers the exact 23-file public allowlist, combined macOS checksums, real temporary
Tauri/Minisign signing, both independent index signatures, tampering and unexpected
remote assets. CLI tests authenticate a signed envelope and reject wrong keys,
versions, filenames, hashes, schemas and download destinations. Desktop feeds the
same envelope into the real updater and verifies version-bound package signatures.
Fixture keys are disposable public test keys; the owner keys and installations
are never touched. Historical v2.1.1 upgrade compatibility is deliberately absent.
Local macOS timing tests use an injected monotonic clock to cover success, failure,
cache reuse and per-architecture totals without waiting for compilation/notarization.

Release output checks: `bun test desktop/scripts/release-progress.test.ts desktop/scripts/local-macos.test.ts desktop/scripts/workflow.test.ts`.
These cover blank-line step boundaries, colored status markers/times, plain output
and color opt-outs, completion after success only, and propagation of the original
failure. They preserve signing/publication ordering and stage-cache assertions;
they use disposable test keys and do not perform production signing, Apple
notarization or publication.

### Shared Host coexistence (v2.2.0 task 1)

Use `scripts/gates.sh host_client:: serve::workspaces::lifecycle_tests::` for
client reconciliation and the existing server-side atomic takeover contracts.
`host_client::reconcile::tests` uses isolated temporary storage, an ephemeral
loopback RPC fixture and injected activation/trust checks; it never opens the
owner's storage, port 2691 or a real background Host. Cases cover compatible
busy/idle/same/newer Hosts, atomic busy races, activation and non-busy failures,
untrusted projects, protocol/native-generation rejection, development build
mismatch, explicit stop and selected-project preservation without hot replacement.
Version-ordering fixtures use synthetic releases under the current compatibility
contract; they do not implement historical Host migration.

`cargo test -p clat-desktop` compiles the native integration and runs existing
policy/updater cases. From `web/e2e`, run
`bunx playwright test --config=playwright.desktop.config.js` to verify that welcome
and about distinguish connection failure from a verified connection and refresh
error/recovered states without sending stop commands. Browser native commands
are mocked; these tests do not claim real macOS/Windows process replacement or
cross-version installer acceptance. Real installed coexistence remains a separate
release check.

Desktop display-name checks are included in `desktop/scripts/release.test.ts`:
the installed name is CLAT while `clat-desktop` and `com.artec.clat.desktop`
remain separate from the CLI. Packaging selects the current `CLAT.app` and DMG
even when earlier named build outputs remain, without deleting those outputs.
The DMG layout test checks the CLAT volume label and Applications shortcut.
These checks use temporary files and injected commands, without real signing or
publication; native Windows shortcut rendering remains a platform smoke check.

## Interface language contracts

Ordinary test entry points pin `CLAT_LANG=en` so a developer's system language
does not change existing assertions. New language tests choose an explicit
`LocaleContext` or an isolated process/browser preference. They never use the
owner's running Host or terminal preference file.

`bun scripts/i18n-check.ts` validates the manifest, duplicate keys, identical key
sets, named parameters, quantity forms, templates and literal source references.
`bun scripts/i18n-installers.ts --check` rejects stale embedded installer
subsets. The shared CI gate runs both checks and their Bun tests; changes to language
runtime/manifest/checker code also select native desktop and web checks. Catalog
wording alone uses structural validation rather than four native checks. Windows CI parses
the PowerShell installer with the native parser.

Use focused checks during editing:

```sh
scripts/gates.sh i18n:: exec:: tui:: host_client:: plugin_cli
cargo test --test cli_language
bun test scripts/i18n-check.test.ts scripts/i18n-installers.test.ts
cd web/e2e
bun x playwright test i18n.spec.js
bun x playwright test --config playwright.desktop.config.js
```

The tests cover precedence and corrupt-file preservation, independent atomic UI
preference merges, literal parameters and HTML injection, English fallback,
localized command failures with original receipts, byte-identical NDJSON usage
errors, original model answers and persisted prompts, active-stream switching,
IME/composer/attachment node preservation, client isolation and plugin-consent
reset without losing its ticket or secret form. TUI permission switching resets
contiguous review progress before approval can be accepted again. Existing
workspace-review tests also verify translated explanations beside unchanged raw
patches; avoid a second concurrent query against its intentionally nonblocking
query guard.

The desktop browser suite mocks native commands; native Rust updater tests use
throwaway signing fixtures and a disposable dummy bundle. Neither proves real
WKWebView/Windows IME, multi-screen behavior, native menu interaction or NSIS
installation. Record macOS, Windows and installer acceptance separately.

The only intended TUI golden change is the added local language shortcut in help.
Journal and RunEvent wire goldens retain their original bytes. See
[Interface languages](localization.md) for the resource maintenance contract.

The language browser suite also drives real isolated Host approval allow/deny
and model → tool → model flows in all three languages. The normal Node web
contracts initialize the real shared locale runtime, retaining their original
queue/receipt assertions. They reject malformed display metadata and guard
against static placeholders overriding newer text or accessibility bindings.
The catalog checker rejects translation calls used as `el` CSS class arguments;
selectors and status identities must remain independent of language.

### GitHub desktop update lifecycle

Run `cargo test -p clat-desktop --bin clat-desktop --locked` on a supported native
platform. The updater tests use throwaway fixture signatures and bounded loopback
HTTP servers to exercise network failure, tampering, successful download and a
macOS temporary bundle replacement. CLI and a leased backend fixture must retain
exactly their original bytes. No production key or installed app is involved.
State tests cover exclusive operations, retained verified bytes after failure,
cancellation at the wait-to-install handoff, irreversible installation, and the
package size limit. TLS initialization is part of the real download path; restored
metadata must work without first calling the plugin's network `check()` method.

From `web/e2e`, run `bun x playwright test --config playwright.desktop.config.js`
for menu events, progress controls, local concurrency, retained errors, retry and
language changes. `bun x playwright test tests/desktop-update.spec.js` separately
uses an isolated Host to check actual workbench text/image/hidden-session drafts,
late image selection and restored editing. These are not native window tests.

Before claiming platform release acceptance, install N in an isolated macOS or
Windows account, update to N+1 through its real menu/page, and reopen it. Verify
that the new desktop version runs, CLI bytes remain unchanged and the cached Host
survives. Windows NSIS handoff/restart and macOS signed bundle permissions need
native verification; mock commands and a temporary test bundle do not establish
those results. Keep the real shared Host and signing credentials out of local tests.

History query coverage: `scripts/gates.sh history_query` exercises real committed compressed journals, English/Simplified/Traditional text, one-character queries, literal operators, external append/truncate/delete, derived-cache corruption, project and trust fences, current-step cutoff, read continuation and source-byte preservation. `web/e2e/tests/history-query.spec.js` uses isolated mock-model Hosts for search, event inspection, language switching, draft/selection preservation and text-only untrusted snippets. The replacement/fallback-title regression is expected to fail if old hidden content is restored to title indexing.

`web/e2e/tests/composer-icons.spec.js` checks sidebar tool icons in expanded and
collapsed layouts, three-language accessible names and tooltips, dialog access,
and the shared send/stop position through a real isolated mock-model run and
cancellation. An unsent draft must survive cancellation and language changes.

Desktop resume checks: `cargo test -p clat-desktop --bin clat-desktop startup::`
covers first launch, an existing Host, one startup attempt, explicit-stop/manual
open precedence, blocked failures and UI preference reload. `scripts/gates.sh
host_client i18n` covers saved-project trust gating, verified-page project scoping
and concurrent preference merges. All fixtures are isolated; these checks do not
claim native GUI reopening or actual shared-Host restart acceptance.

`host_client::lifecycle::tests::offline_management_discovery_keeps_localized_reason_without_retry`
uses a closed loopback endpoint to verify the English diagnostic and three-language
presentation survive discovery and management without retries. The desktop shell
suite (`web/e2e/playwright.desktop.config.js`) checks navigation handoff, live
language changes, draft protection and error recovery. These browser checks mock
native commands; they do not establish WKWebView/Windows startup behavior.

`web/e2e/tests/desktop-update.spec.js` also injects the native bridge as a same-origin
test script into the real isolated Host page, retaining its production CSP. It
checks update actions and unsent-work protection under the actual Host policy.
Sidebar icons, centering and language-control spacing are reviewed manually.

Native background job checks: `scripts/gates.sh native_job target_writer`. They exercise actual isolated child processes, Run cancellation vs Job cancellation, original-conversation facts, UTF-8/redaction, independent output cursors, timeout/overflow, registration-before-spawn, post-spawn uncertainty, panic, recovery, terminal-save and cleanup retry, G7 scope, and shared writer release. `background-jobs.spec.js` uses an isolated Host and deterministic model to check approval, new-session/reconnect survival, language switching/draft preservation and explicit stop. These do not establish native Windows/Desktop behavior or paid-model effectiveness.

When adding native tools or a projection, also run
`scripts/gates.sh failed_run_spawn_does_not_mark rc_021:: runtime_status_observes_all_projects idle_scheduler_poll_never`.
These pin the complete frozen tool order, foreign-owner Job isolation, checkpoint
parity and actual Host readiness. Do not replace complete comparisons with subset
checks. Sandboxed child-process fixtures must write their success markers inside
their authorized project; a marker beside the project is an unauthorized write on
Windows even if an unenforced local platform permits it. Empty scheduler polls
must not publish an active Run while waiting for the Application lock.

Automatic work: `scripts/gates.sh scheduler_ automatic_ target_run` covers frozen
authorization/CAS, scoped drafts, budgets, unknown admission recovery, original
session bindings and Job grants. Pure Jiff tests simulate DST gaps/folds,
overdue intervals and clock rollback. A real isolated loopback Host is stopped
and reopened to verify persistent rules and original-session delivery. The
`scheduled-work.spec.js` browser test checks draft-only creation, explicit review,
three-language switching, another conversation's Chinese draft and staged image,
and receipt-backed completion. Native windows and paid services remain separate
acceptance faces.
Keep asynchronous title writers out of byte-equality fixtures, or explicitly wait
for their completion; do not relax the byte-equality assertion to hide the race.

### Optional PWA visual review

`web/e2e/manual/visual-review.spec.js` captures representative long-form content,
themes and narrow layouts for human review. Run it explicitly with
`web/e2e/node_modules/.bin/playwright test --config web/e2e/playwright.visual.config.js`.
It uses the isolated E2E Host and is outside the ordinary CI test directory.
Screenshots are evidence for review, not pixel goldens. Font sizes, icon paths,
exact control gaps and fixed menu/button totals are not delivery assertions.

Workbench interaction tests cover navigation/draft retention, mode ownership,
question/approval routing, and native navigation protection. A native-command
mock verifies bridge wiring only; it does not establish macOS/Windows window,
installer or release acceptance.

`project-context.spec.js` walks same-document project switching through real
Host RPCs: draft/image retention, late reads and committed prompt receipts,
query/hash history, reading anchors, inactive interactions and native preflight.
The workbench factory's legacy fixture inspection ports are embedded only in
Rust test builds; production exports lifecycle/display summaries. Ordinary
settings headings are not fixed strings in language tests: language isolation,
unchanged user input and absence of unintended mutations remain the contracts.
Content-page checks verify that conversation-only actions are unavailable in
other features and that checked file references return to the retained draft.
The application/page hierarchy and wide/narrow composition are reviewed through
the optional `manual/content-architecture.spec.js` captures, without DOM-tree,
column-count or pixel assertions.

### Responsive conversation map and content canvas

The rounded content frame, theme backgrounds, and responsive map visibility are
reviewed through manual screenshots, not fixed CSS sizes, thresholds, color
comparisons, or pixel goldens. Existing map tests continue to protect history
loading/jump targets, hover preview continuity, user/assistant preview content,
and bounded virtualized navigation. The former fixed scrollbar-gutter distance
and role-color comparison were removed; their visual intent is covered by the
manual canvas review. No behavior test was removed or replaced with wording.

### Project/session tree

The dedicated project-tree Playwright Host admits fixture conversations through
core with an injected TestProvider, publishes the canonical second-project root,
and then uses normal workspace/session routes for all browser interactions.
Expanding folders is checked for absence of session mutations and foreign SSE;
opening a child session and per-project new conversation are checked for correct
selection and retained drafts. A held workbench response verifies that a newer
project navigation cancels the earlier conversation request. Existing keyboard
menu/focus, receipt ownership, running-project state, and native draft protection
checks remain in use. Icon geometry, duplicate-icon counts, exact indentation,
folder ordering, display labels, and shortened-list lengths have no fixed CI
assertions; representative screenshots are reviewed manually.


Navigation-page migration behavior mapping:

| Migrated surface | Retained behavior coverage |
| --- | --- |
| Navigation sections replacing old dialogs | Task and settings edits survive page/project navigation; Escape retains the current feature page. |
| Task lists, editor and associated conversation | Fixed delivery scope, authorization before execution, independent draft/image preservation, late-save isolation. |
| Resource/history lists and preview | Exact snapshot content, first-message provenance, bookmark reload, read-only selection isolation and historical attachment proof. |
| Settings row and group layout | Newer edits survive old acknowledgments; delayed pairing credentials cannot repopulate a departed page. |
| Existing schedule/background-job captions in three languages | Catalog checks cover translation integrity; operations and actual output remain asserted. |

`web/tests/navigation-data.test.cjs` checks retry identities and trigger preservation;
`web/e2e/tests/navigation-pages.spec.js` checks the corresponding complete client
operations. Rust `navigation_` tests protect private path fences, public history,
attachment proof, persistent grants/bookmarks and scheduler reservations. These
checks contain no screenshot baseline, icon geometry or fixed ordinary display copy.


Legacy settings tests now leave through navigation and use the stable New profile
action rather than selecting every nested summary. Plugin discovery tests explicitly
open Discover. Read-only Host status refreshes are permitted during language changes;
the checks still prohibit plugin activation and other writes. Model-key checks assert
credential state and absence of echoed secrets, rather than the phrasing of status
captions. No scenario was deleted for these migrations.
