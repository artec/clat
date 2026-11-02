# Harness² research adapter

This is opt-in development tooling, not a CLAT runtime dependency. Python 3.12
and the adjacent Harness² checkout at
`1cfe254e8d25b15f37048c3730134686abe60335` are required for method integration.
The checked-in compatibility patch adds only a CLAT prompt surface, channel
identifier and installation-mode identifier. The upstream recursion and selection
algorithms remain unchanged. A separate acceptance calculation can retain the
production baseline even when upstream selected a candidate.

## Offline checks

```sh
python3 -m unittest discover -s scripts/harness2/tests -v
scripts/gates.sh harness2 compaction
```

To exercise the actual pinned upstream without credentials, use a new absolute
output directory:

```sh
../harness2/.venv/bin/python scripts/harness2/integration.py \
  --source ../harness2 --output /absolute/private/offline-method
```

This uses scripted edits and synthetic artifacts. Separately, the Rust tests
capture actual summary requests, enforce request capacity and verify instance
isolation. Neither is real semantic acceptance.

## Build and freeze the private worker

Build `cargo test -p clat-core --lib --no-run --message-format=json`; obtain the
`executable` from the `compiler-artifact` record for the core test target. Copy
that executable to a new private path and make it read-only. Always pass that
frozen copy, rather than a `target/` path that a later compilation can replace.
Freeze the complete Python adapter directory alongside it. The adapter hashes
its Python sources; changing the driver invalidates reuse even with the same
worker. Reporting may be recomputed separately, with its own version and hash;
retain earlier reports and record any checker adjudication.
No new product CLI command, RPC or durable event is added.

The private ignored test is
`application::reliability_tests::harness2::harness2_bridge`. It reads no stdin or
experiment files unless explicitly armed. Its manifest is supplied through stdin,
not argv. The Python launcher supplies the per-worker arm; live orchestration
additionally requires `CLAT_HARNESS2_ARM=1`.

## Live experiment

Supply `CLAT_GLM_CODING_PLAN_KEY` and `CLAT_HARNESS2_DEEPSEEK_KEY` from a trusted
temporary launcher. Never put secrets in command arguments, a shell-history
assignment, `.env`, saved CLAT settings, or reports. Real models only receive
synthetic tasks and generated images. Check the frozen route identities before
calling either model; this version uses GLM Coding Plan / `glm-5.3-flash` and
DeepSeek / `deepseek-v4-pro`. There is no automatic route fallback.
The improvement and review profile uses a 1,000,000-token context window,
16,384 output tokens and low internal thinking. Its editing Application disables
compaction and tool-result pruning so full evidence cannot silently change.
GLM scenario execution remains at 24,000 / 4,096 (the original-text controls
use a 1,000,000-token window). These profiles must be preregistered together.

```sh
../harness2/.venv/bin/python scripts/harness2/run.py prepare \
  --binary /absolute/private/frozen-worker --output /absolute/private/experiment
../harness2/.venv/bin/python scripts/harness2/run.py campaign \
  --binary /absolute/private/frozen-worker --output /absolute/private/experiment
```

Run these sequentially. Alternatively run `probe`, `train`, `evaluate`, `review`
and `report` separately with identical binary and output arguments. `report` does
not require live arming. An incomplete attempt or changed identity is retained
and rejected, rather than overwritten or silently rerun. Concurrent submissions
of the same execution identity fail before model calls.

The current local launcher requires macOS Seatbelt. It denies access to home
files except the worker and its own evidence subtree, restricts writes and
temporary files to that subtree, and denies direct outbound network access.
Only a local CONNECT relay can contact the selected provider on port 443; the
relay rejects other authorities and leaves TLS opaque. Directory ancestors may
be traversed for capability-based storage and leases. No real Host is connected.
Linux requires a separately confined launcher; unsupported platforms fail closed.
This is not a claim that an official Linux upstream benchmark was executed.

## Experiment and evidence

The frozen roster contains three training tasks, six held-out tasks with three
replicates each, and the original twelve reliability regression cases. Planned
scenario executions are 12 discovery, 3 unified-candidate training checks, 36
held-out comparisons and 24 legacy comparisons: **75**. Probes, proposals,
composition, reflection and semantic audit requests are additional.

Only `summary_guidance.md` is editable. Its bytes participate in summary capacity
planning and actual requests. Models cannot change windows, output allowances,
permissions, tool definitions, the checker, answers or source tasks. Worker
results include raw observations and artifacts; independent expected-value
checking runs outside the model context. Semantic audits see the complete
before/after view at the actual durable replacement carrier, not the log-only
summary event. Literal quotes are validated, but a valid quote does not prove
the model's interpretation. Textual image placeholders are not pixel evidence.
Every serialized fixed-ID evidence result is checked against the complete
original before accepting an improvement. The tool probe includes a long
synthetic evidence read to exercise this boundary under realistic pressure.

All failures and original responses remain in private output directories. Missing
usage and prices remain unknown. Upstream ledger default zero-dollar values are
not billing evidence. Adoption requires complete safety/lifecycle and fact checks,
no regression, and observed improvements in at least two domains; semantic
opinions do not replace independent human review. Incomplete or inconclusive
experiments retain the production baseline. Held-out failures cannot be reused
for tuning within the same experiment.
An upstream final with no artifact blocks promotion; it is not silently replaced
with a successful earlier rollout. Remaining frozen training domains may be
collected separately without retrying failed samples, and must be recorded as a
continuation rather than a successful complete campaign.
