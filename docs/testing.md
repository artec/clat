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
