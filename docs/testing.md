# Development verification

The edit loop and the delivery gate have different jobs. After a local edit:

```bash
scripts/gates.sh session::admission::   # one Rust area
scripts/gates.sh model:: tui::         # OR filters, one test harness build
scripts/gates.sh --plan                # inspect automatic selection; no build
scripts/gates.sh                       # choose from changed paths
python3 scripts/measure-inner-loop.py --package clat tui::model_editor::
```

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

The measurement command reports build time, startup plus tests, artifact
size, cache freshness and total time. Run it immediately after an actual
edit. `--rebuild src/tui/model_editor.rs` explicitly requests a synthetic
mtime-only rebuild; `--budget 30` turns latency regression into failure.
It never signs artifacts, changes security settings or removes metadata.

Automatic selection includes staged, unstaged and untracked files. Known
domains include their principal consumers; shared contracts, fixtures, build
inputs and unknown domains fall back to all Rust targets. Documentation-only
changes need only the diff check. This is a feedback heuristic, not a proof
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

Before handing off a completed batch, run once:

```bash
scripts/gates.sh --full
```

This includes selection-script tests, fmt, clippy, local xwin static checks,
rustdoc, all Rust targets, adapter build/tests and the ignored test face.
After a failure, repair and rerun the failed/affected checks; do not restart
every already-green check without a reason. Test failures are evidence to
investigate, not permission to retry until green.

Linux CI and the Linux container call `scripts/gates.sh --ci`, the same full
sequence without xwin. Windows CI still runs native clippy and tests on every
PR/main push. Superseded CI runs for the same ref are cancelled. CI has no
path-based omissions; it remains the safety net for quick-selection misses.

`scripts/ci-box.sh` runs the full Linux gate on a two-CPU container when
platform semantics need verification. `--stress N` remains available for a
specific diagnosed timing concern, rather than routine repeated full runs.
DSH checkout oracles remain opt-in with `DSH_CHECKOUT`; real-model/device
checks follow [Live validation](live-validation.md).

## Standing test rules (2026-09-11 consolidation)

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
