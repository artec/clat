# Live-model validation

The normal test suite is deterministic and credential-free. It cannot prove
that a real provider still accepts CLAT's request shape, streams correctly, or
continues after a tool call. Run the two gates below whenever provider adapters,
presets, authentication, streaming, reasoning replay, or model/tool sequencing
change—and before relying on a new provider for dogfood work.

These checks may consume paid tokens. Use a dedicated low-risk credential and
never paste the key into an issue, log, shell history, or validation record.

## Prerequisites

Start from a gate-green local build:

```bash
cargo test --all-targets --all-features
cargo build
./target/debug/clat demo
```

Then open the development binary in a safe test repository:

```bash
./target/debug/clat
```

Use `/model` to select the exact preset/profile under test and provide its real
credential.

## Gate 1: streamed text

Send:

```text
只回答：CLAT_LIVE_MODEL_OK
```

English equivalent:

```text
Reply with exactly: CLAT_LIVE_MODEL_OK
```

Pass only when all conditions hold:

1. the provider accepts authentication and the request;
2. text arrives incrementally rather than only after connection close;
3. the visible answer contains `CLAT_LIVE_MODEL_OK`;
4. the run reaches a successful terminal state;
5. usage/status data, if returned by the provider, is sane rather than required
   for success.

A single final blob, decode error, authentication retry loop, or success UI
without a durable run terminal is a failure.

## Gate 2: model → native tool → model

In the same session, send:

```text
请必须使用 list_files 查看当前项目根目录，然后告诉我有哪些文件。
```

English equivalent:

```text
You must use list_files on the current project root, then tell me which files are there.
```

Pass only when the event sequence shows:

1. a model request;
2. a `list_files` tool call with valid arguments;
3. native tool execution inside the intended local context;
4. the tool result returned to the provider;
5. a subsequent model request;
6. a streamed final answer grounded in the returned listing;
7. a successful durable run terminal.

This gate distinguishes a chat client from an agent runtime. A model that
describes what it would do without calling `list_files` does not pass.

## Optional provider-specific checks

Run only those relevant to the change:

- **reasoning replay** — request a multi-turn task with tool calls and confirm
  the second provider request accepts retained reasoning state;
- **vision** — attach a small local image through the probe-verified GLM 5.3
  Flash preset and verify it is received natively;
- **cache/usage** — make a repeated long-prefix request and confirm reported
  cached/context fields remain numerically sane;
- **cancellation** — cancel during a long stream and verify prompt return,
  partial persistence, and clean next run;
- **retry** — use a controlled endpoint that returns a transient response before
  success, and confirm no retry occurs after visible stream output.
- **command session** — require the model to start a short `exec_command` that
  waits for stdin, continue it through `write_stdin`, then start and terminate a
  watcher. Confirm both Execute approvals show full arguments, output is
  incremental, the terminal metadata is visible, and no child survives run end.
- **macOS sandbox** — from Project Write, run one project write and one project-
  external write with `sandbox: "required"`; the first must succeed and report
  Seatbelt/full plus a policy digest, while the second must fail without
  creating the target. Repeat network-disabled against a controlled listener.

## GLM 5.3 Flash multimodal campaign

This is a paid, explicit campaign for changes to native image input. It is not
replaced by request-shape unit tests, a text-only smoke test, or a pass through
a different vision provider. Select the built-in **GLM 5.3 Flash** preset
(`glm-5.3-flash`) and use non-sensitive local PNG/JPEG fixtures. Do not put the
credential, image bytes, local paths, or pairing token in the record.

For a narrowly scoped adapter regression before the UI campaigns, the ignored
`providers::openai_compatible::tests::live_glm_flash_adapter_preserves_two_image_order`
test sends two generated colour PNGs through CLAT's actual compatible adapter.
Arm it only with an explicitly supplied, process-local
`CLAT_GLM_CODING_PLAN_KEY`; it must never be added to a shell profile, CLAT
storage, fixture, log, or test report. Both requests carry the same substantial
system prefix and the second replays the first image-bearing turn; the test
prints only provider-reported input/cache token counts. This checks adapter
projection, SSE events and cache-field plumbing, not the frontend acceptance
items below.

Five additional paid gates and one credential-free physical-terminal gate
exercise more of the product path. All are default-off and become strict only
when explicitly armed, so the CI ignored-test lane can enumerate them without
spending quota or requiring a TTY:

```sh
cargo test application::tests::live_glm_application_calls_view_image_and_consumes_its_typed_result \
  -- --ignored --exact --nocapture

cargo test application::tests::live_glm_auto_compacts_long_history_before_an_image_turn \
  -- --ignored --exact --nocapture

cargo test tui::snapshot_tests::live_glm_tui_multi_image_and_image_only_history \
  -- --ignored --exact --nocapture

cargo test tui::snapshot_tests::live_glm_tui_manual_compaction_cold_reopen_and_continue \
  -- --ignored --exact --nocapture

CLAT_PHYSICAL_PTY=1 cargo test tui::snapshot_tests::physical_pty_tui_attachment_composer_smoke \
  -- --ignored --exact --nocapture

cd web/e2e
CLAT_LIVE_GLM_E2E=1 npx playwright test --grep "MM-5 live GLM PWA"
```

The first Application gate requires a real `view_image` tool call and a typed,
ref-only image result. The second seeds long history through normal admission,
Run and journal paths, cold-remounts with a 12k test window, then requires a
real GLM summary to be durably committed before GLM consumes the retained
image turn. The TUI gate drives the production paste/input state machine,
async admission and rendering through a TestBackend, plus real GLM
`view_image`, image steering, cancellation recovery and retry; it is not a
physical-terminal or system-clipboard check. The second TUI gate sends
`/compact` through the production command surface, uses GLM for the summary,
cold-remounts the same session, verifies durable compaction replay, and then
continues with GLM. The physical-PTY gate must be run interactively: type
`/attach physical.png`, confirm the generated 96×64 fixture appears as
`[Image #1]`, then press Ctrl+C. It exercises the real crossterm input thread,
alternate screen, raw mode, bracketed paste/mouse mode setup, attachment rail,
and terminal restoration against an isolated storage root. It does not read
the OS clipboard and therefore does not replace `/paste-image` platform
validation. The Chromium gate covers ordered
multi-image input, reload/replay, an image-only turn, and a subsequent question
grounded in that history. The paid gates' shell environment must also contain
`CLAT_GLM_CODING_PLAN_KEY`; the physical-PTY gate does not need it. Do not
place the credential value in command history or a validation record.

Run the following separately in the TUI and local workbench. Keep the same
saved session for the replay checks.

1. Attach one image with a simple known fact (for example, a rendered colour
   label), ask for that fact, and confirm incremental text plus one durable
   terminal.
2. Attach two intentionally different images and ask for their ordered
   difference. Confirm the answer is grounded in their order, not merely that
   the request was accepted.
3. Send an image-only message, then ask for a follow-up that requires the
   prior image. Restart or resume and confirm the history projection still has
   a working thumbnail without exposing a host path.
4. Ask the agent to inspect an already-reachable image with `view_image`, then
   continue with a normal tool round. Confirm the tool argument is an opaque
   attachment identifier or another documented fenced reference, never an
   absolute host path.
5. While a long stream is live, queue an image steering draft; confirm it is
   either claimed exactly once at the next model boundary or returned as an
   intact retryable draft when the run ends. Cancel one streaming run and
   repeat the retry path.
6. Repeat a long shared prefix on both sides of an attachment and record the
   returned usage/cache fields when the provider supplies them. Treat absent
   cache fields as unknown, not as a miss or hit.

For the workbench, also verify file-picker, paste or drop staging; an upload
failure must retain the ordered browser draft, while a session switch must
revoke it. For the TUI, exercise explicit `/paste-image` on each platform that
will be supported; ordinary terminal paste must not probe the system clipboard.
After copying a non-sensitive image manually, the default-off
`tui::attachments::tests::live_system_clipboard_image_is_readable_and_privately_staged`
test can isolate the production OS-clipboard → bounded RGBA encode → private
draft-staging leg. It reads but never replaces clipboard contents. Passing that
test still does not replace typing `/paste-image` in a physical terminal.
Arm it with
`CLAT_LIVE_CLIPBOARD=1 cargo test tui::attachments::tests::live_system_clipboard_image_is_readable_and_privately_staged -- --ignored --exact --nocapture`.

Before the near-limit multi-image cases, record an idle RSS baseline with the
same binary and platform. Record the peak during admission, request creation,
and PWA upload/reconnect separately, along with image count and byte sizes.
There is no universal pass number yet: the evidence establishes a future
budget; an "it did not OOM" observation is not a performance pass.

The default-ignored
`providers::openai_compatible::tests::mm5_near_limit_multimodal_profile`
provides a repeatable core baseline for the first two phases. It generates a
deterministic near-32-MiB raw PNG batch, runs admission and GLM request
projection in separate fresh processes, reports `MM5_PERF` JSON lines, and
cleans its temporary store. It does not exercise browser upload/reconnect and
therefore cannot close that campaign leg.

Arm it explicitly with
`CLAT_MM5_PERF=1 cargo test providers::openai_compatible::tests::mm5_near_limit_multimodal_profile -- --ignored --exact --nocapture`.

Run the separate Chromium/PWA leg from `web/e2e` with
`CLAT_MM5_PERF=1 npx playwright test --grep "MM-5 PWA near-limit"`. It uploads
four generated valid PNGs just below the 32-MiB raw batch limit, sends the
image-only draft, reloads the page, verifies all four protected history blobs,
and prints one `MM5_PWA_PERF` JSON line. The temporary e2e handshake exposes
only the test-host PID for RSS sampling; it is not a production protocol field.
Add `CLAT_E2E_RELEASE=1` to run the same browser leg against the optimized
release test binary. Run at least three fresh invocations and record a range;
do not compare one debug high-water sample with one release sample as if
allocator scheduling were deterministic.

## Workflow/intelligence live spot-checks

These checks do not replace the deterministic scenario suite:

- enter `/plan`, ask for an implementation, and confirm the model can investigate
  but cannot call write/execute/external-read tools; submit a plan, approve it,
  and confirm full tools return only on the next run;
- create a project skill that shadows a user skill, load it with `skill`, then
  remove the project copy and confirm the next run falls back to the user digest;
- run `/context` before/after Plan Mode or a skill change and confirm only the
  expected estimate components/tool list move; with image history, also verify
  retained/original/omitted counts, normalized bytes, visual-token estimate,
  2.0x safety factor, and output reserve. Cross a 1024-token image-pressure
  bucket and confirm older images are omitted oldest-first while the latest
  turn remains native. The command itself must not add a conversation event;
- on macOS with real servers installed, configure `rust-analyzer` for `.rs` and
  `typescript-language-server` for `.ts`/`.tsx`, then exercise definition,
  references and hover against small Rust and TypeScript fixtures. Kill each
  server once and confirm the next query performs one clean restart. Close CLAT
  and confirm no managed LSP process survives.

A fake JSON-RPC server proves protocol conformance only. Do not record the
Rust/TypeScript live LSP gate as passed unless those real servers were actually
installed and exercised on the recorded machine.

For agent phase 4, keep correctness gates separate from effectiveness claims:

- add one project and one user memory, restart, verify project isolation and
  stale-source display, and confirm no model answer creates a new record;
- create a two-round file-acceptance goal with narrow limits, arm it explicitly,
  cancel once, restart, and confirm the durable counters restore while the arm
  remains off;
- with `/subagents on`, delegate one explorer read, confirm the child sees only
  the three project-confined read tools, cancel the parent once, and verify the
  durable start/end provenance and zero surviving workers; restart and confirm
  the tool is hidden again.

The default-off subagent effectiveness campaign is a separate paid experiment:
two cross-directory location tasks and two review tasks, single-agent versus
subagent, five repetitions each. Pre-register verifiers and require at least a
20 percentage-point correct-task improvement, no error-rate increase, zero
write/execute/out-of-project events, no more than 2.0x total tokens, and no more
than 2.0x p50 latency. Do not enable the experiment by default or claim an
improvement until that campaign is explicitly authorized and recorded.

## Headless parity spot-check

After the TUI gates, the same saved model can be checked through the headless
frontend:

```bash
./target/debug/clat exec "只回答：CLAT_EXEC_LIVE_OK"
./target/debug/clat exec --json "只回答：CLAT_EXEC_JSON_OK"
```

For `--json`, verify the final line is `exec_completed` with exit code 0. A
`run_completed` event alone is not the invocation verdict.

When `clat serve` or the PWA changed, repeat one prompt through the workbench
and verify pairing, replay, streaming, `prompt.settled`, and restart behavior.
For compaction changes, create at least five ordinary turns, use **Compact
history** in the Session inspector, wait for the successful completion notice,
reload the page, confirm the durable **History compacted** replay marker, and
complete one more prompt. Also start one deliberately slow compaction, reload
while it is active, cancel it from the restored control, and confirm no durable
success marker appears. The deterministic Playwright versions are:

```sh
cd web/e2e
npx playwright test --grep "history compaction completes|active history compaction survives"
```

## Record the result

Record only non-secret facts:

- CLAT commit/version;
- date and platform;
- preset/profile name and model id;
- endpoint host, if it is not private;
- which gates passed;
- any optional checks;
- observed provider error code or sanitized diagnostic on failure.

Keep static test results and live results separate. A green Rust suite proves
the local implementation gates; a live pass proves only the tested provider,
credential class, endpoint, and moment in time.

There is no canonical result file in the repository. Put the record beside the
decision it supports—for example the maintainer's release checklist, review
thread, or private build artifact—and use a compact template:

```text
CLAT version/commit:
Date / platform:
Preset / model:
Endpoint host:
Gate 1: pass | fail
Gate 2: pass | fail
Optional checks:
Sanitized notes:
```
