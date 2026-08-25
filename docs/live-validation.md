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
- **vision** — attach a small local image and verify the vision preset receives
  it natively;
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

## Workflow/intelligence live spot-checks

These checks do not replace the deterministic scenario suite:

- enter `/plan`, ask for an implementation, and confirm the model can investigate
  but cannot call write/execute/external-read tools; submit a plan, approve it,
  and confirm full tools return only on the next run;
- create a project skill that shadows a user skill, load it with `skill`, then
  remove the project copy and confirm the next run falls back to the user digest;
- run `/context` before/after Plan Mode or a skill change and confirm only the
  expected estimate components/tool list move; the command itself must not add a
  conversation event;
- on macOS with real servers installed, configure `rust-analyzer` for `.rs` and
  `typescript-language-server` for `.ts`/`.tsx`, then exercise definition,
  references and hover against small Rust and TypeScript fixtures. Kill each
  server once and confirm the next query performs one clean restart. Close CLAT
  and confirm no managed LSP process survives.

A fake JSON-RPC server proves protocol conformance only. Do not record the
Rust/TypeScript live LSP gate as passed unless those real servers were actually
installed and exercised on the recorded machine.

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
