# Agent effectiveness campaigns

The deterministic Scenario Harness under `tests/fixtures/agent-scenarios/`
uses the real Application, tools, permissions and journal with a scripted
provider. It proves conformance. Real model task completion and an improvement
claim require a separately authorized live campaign.

Run a paid campaign only after the repository owner explicitly provides a
credential class and cost/time budget. Fill `campaign-template.md` before the
first run. Freeze tasks, acceptance checks, model/preset/instructions and budgets;
retain failures, timeouts and cancellations in the denominator. Never store
credentials here. No script in this directory calls a model.

## Offline first version

`tasks.json` registers eleven historical production-function microtasks in six
families. Ten qualify; AGEVAL-09 remains in the investigation pool because its
historical reference fix still reads a same-directory relative symlink. Each
scored task must have three **compiled behavioral** observations: original bug
red, historical reference fix green, deliberately incorrect fix red. Compiler
failures are infrastructure failures. Normal inputs and boundary cases prevent
trivial zero/MAX implementations from earning credit. These are related tasks,
not ten independent capability dimensions or an end-to-end repository benchmark.

`eval.py` is a developer-only Python 3 utility, outside the one-binary product.
It runs pinned Rust production items with a separate behavioral oracle and
prebuilt dependency artifacts. It does not build or validate the entire
historical crate. The controlled extractor understands only the registered
items; unsupported Rust syntax fails qualification rather than weakening it.

On Unix, first build the ordinary harness and check evaluator contracts:

```bash
scripts/gates.sh agent_eval
python3 -m unittest discover -s tests/agent-campaigns -p test_eval.py

# --out must be new, outside this repository and the real state root.
python3 tests/agent-campaigns/eval.py qualify \
  --repo "$PWD" --dependencies "$PWD/target/debug/deps" \
  --out /tmp/clat-eval-qualification-NEW
python3 tests/agent-campaigns/eval.py recompute \
  --campaign /tmp/clat-eval-qualification-NEW
```

The preregistration is written before execution and includes the fixed roster,
input digests, immutable source revisions, compiler, dependency digests and
budgets. Frozen evaluator inputs, exported exam sources, probe source, raw
compiler/oracle logs and results are retained. Recompute checks the report's
preregistration digest, complete roster, revision, source and log digests,
then derives qualification from raw oracle outcomes rather than the recorded
`qualified` flag. This is reproducibility and accidental-tamper detection,
not cryptographic provenance against somebody rewriting the whole evidence set.

For one actual Application control (no paid calls), locate the `clat-core`
test executable in Cargo's compiler-artifact JSON:

```bash
cargo test -p clat-core --lib --no-run --message-format=json
python3 tests/agent-campaigns/eval.py dry-run \
  --repo "$PWD" --harness /absolute/path/to/clat_core-TEST_EXECUTABLE \
  --out /tmp/clat-eval-control-NEW
python3 tests/agent-campaigns/eval.py recompute \
  --campaign /tmp/clat-eval-control-NEW
```

The control exercises the original permission fixture through Application and
exports observed tool results, durable journal event kinds, output-file hashes
and close status. An external comparison against the frozen fixture's expected
file bytes decides success. It reports one mechanism control, not eleven model
attempts. Missing token/cost measurements remain `null`; a scripted 1/1 does not
establish model effectiveness. An unsuccessful started control stays a failure.

## Isolation and acceptance limits

Exam directories contain only registered production source and `TASK.md`,
without Git history, reference fixes or oracle files. The judge is a sibling
outside the exam. **This separation alone is not an OS read fence.** The runner
is for trusted offline historical code only: an untrusted candidate can access
other same-account files. No live candidate execution is authorized by it. The offline assertions share
a process with trusted historical code. Untrusted generated code needs an
external judge that checks function results and independently observes side
effects after the candidate process exits; test-output text is not a receipt.
Before P2, provide a separately verified OS/container read/write/network fence,
independent state root, process containment and hidden-oracle placement. Also
prepare a public candidate build/smoke-test contract: the current source-only
exports are microtask inputs, not standalone Cargo checkouts. CLAT's
current partial sandbox and an all-approving test policy are insufficient for
that claim. Do not fall back to the owner's working tree, `~/.clat` or active host.

Commands have deadlines and isolated HOME/TMPDIR, with streamed logs and a
post-command size check. Excess output or a residual verifier process group
stops the batch. The size check is not a real-time disk quota. Unix teardown
probes independently observe live ordinary owner/child PIDs, their process
group and a private scratch canary before checking their removal. They do not
prove cleanup of escaped sessions, arbitrary temp files or Windows processes.
Security violations stop the entire batch; they never count as ordinary failures.

Normal Rust tests include the Application control and Unix teardown probes.
Historical qualification and Python contracts run explicitly; they are not
paid tests and are not silently added to production CLI or CI. Delivery still
requires `scripts/gates.sh --full`. Native Windows CI after the owner's commit
remains a separate acceptance obligation.

The first-version result and retained iteration ledger are in
[evidence/2026-10-09/result.json](evidence/2026-10-09/result.json).
The complete local raw run directory is recorded there. Recreate evidence with
the commands above; the compact committed summary alone is not a raw campaign.
