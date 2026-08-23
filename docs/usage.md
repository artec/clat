# Using CLAT

This guide is for people running CLAT. It covers the terminal UI, headless
runner, local web workbench, and DSH client. Configuration internals belong in
[Model editor](model-editor.md), security semantics in
[Permissions](permissions.md), and persistence details in
[Persistent state](storage.md).

## Choose an interface

| Need | Command | State owner |
|---|---|---|
| Interactive work in the current repository | `clat` | local CLAT |
| One run for a script or CI job | `clat exec [PROMPT]` | local CLAT |
| Browser workbench or local API | `clat serve` | local CLAT |
| CLAT TUI connected to DeepSeek Harness | `clat dsh` | DSH host |
| Offline core-loop smoke test | `clat demo` | temporary demo state |
| Check/install a published update | `clat upgrade [--check]` | installed binary |

All local interfaces use the same Application core, session journal, tools,
and permission semantics. They differ only in how input, events, and approval
requests are presented.

### Terms used in this guide

| Term | Meaning here |
|---|---|
| DSH | DeepSeek Harness, whose event/session vocabulary is CLAT's interoperability reference |
| PWA | installable browser application served by `clat serve` |
| SSE | server-sent event stream used for replay and live run events |
| NDJSON | one JSON object per line, used by `clat exec --json` |
| WIT | typed component contract used by WASM plugins |
| journal | append-only authoritative session event log |
| projection/checkpoint | derived state / a cache of derived state; neither replaces the journal |
| durable terminal | a run outcome published only after its closing journal events commit |

## First run and project trust

Run `clat` in the repository you want CLAT to work on. The first open of an
untrusted directory shows a full-screen trust prompt. Before you approve it,
CLAT does not create a session, read project files, start MCP servers, or call a
model.

After trust is accepted, run `/model`, choose a built-in preset or Custom, and
enter the provider credential. The setting is reused by `clat exec` and
`clat serve` for the same local installation.

For non-interactive setup, `clat exec --trust "..."` accepts trust for the
current project. `clat serve` never grants trust by itself; open `clat` once or
run `clat exec --trust` before starting the server.

## Upgrade

An installed binary can check or install the newest GitHub release:

```bash
clat upgrade --check   # report availability without changing the binary
clat upgrade           # verify and install the newest release
```

Upgrades authenticate the asset manifest with the Minisign public key embedded
in the current binary, verify the SHA-256 digest and release-tag binding, then
stage and replace the executable with rollback on failure. The first-install
bootstrap has a different trust boundary; see [Release signing](releasing.md).

## Terminal UI

Running `clat` without a subcommand opens the TUI. The previous session is
restored in the background; input stays disabled until replay finishes, then
the conversation becomes interactive.

The main screen has three surfaces:

- **Conversation** — durable user and assistant messages, tool activity, and a
  scrollbar. Streaming responses use an animated marker.
- **Message input** — Unicode-aware editing, multiline input, queued steering,
  attachment chips, and the active permission mode.
- **Status** — current model/tool activity and available usage, cache, context,
  balance, or quota telemetry.

### Keyboard

| Key | Action |
|---|---|
| `Enter` | submit; during a run, queue steering for the next model boundary |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | insert a line break |
| `←` / `→`, `Home`, `End` | move the input cursor |
| `Backspace` / `Delete` | edit input |
| `↑` / `↓` | recall input history; if none, scroll the conversation |
| `PageUp` / `PageDown` | scroll the conversation |
| `Shift+Tab` | cycle the active vendor's reasoning level |
| mouse wheel | scroll the conversation |
| mouse drag | select text and copy it with OSC 52 on release |
| `Cmd+C` / `Ctrl+Shift+C` | copy the current selection again |
| `Cmd+X` / `Ctrl+Shift+X` | cut the input selection |
| `Esc` | recall the newest queued steering; otherwise cancel the active run; when idle, clear input |
| `Ctrl+C` | copy an active selection; otherwise quit |

While a permission dialog is open, `Esc` denies that tool call instead of
cancelling the run. Hold `Shift` while dragging if you want the terminal's own
selection behavior instead of CLAT's mouse handling.

### Slash commands

| Command | Purpose |
|---|---|
| `/model` | choose a provider preset or manage named custom profiles |
| `/new`, `/clear` | start a fresh, lazily materialized conversation |
| `/resume` | switch to a prior conversation in this project |
| `/rename` | replace the current conversation title |
| `/compact` | summarize older context in the background; original history remains on disk |
| `/perm`, `/permission` | switch Read Only, Project Write, or Full Access |
| `/mcp` | inspect MCP/WASM connection state, tools, and isolated failures |
| `/help` | open the command and key reference |
| `/quit`, `/exit` | close CLAT cleanly |

The command catalog is shared with `clat exec --command`. Commands that require
an interactive picker, such as `/model`, `/resume`, and `/perm`, report a usage
error in headless mode instead of inventing a selection.

### Steering, cancellation, and long runs

Submitting text while a run is active queues it for the next model-request
boundary; it does not interrupt the in-flight HTTP request or tool call. The
queued message appears immediately in the conversation. `Esc` recalls the
newest queued message into the input. When no steering is queued, `Esc`
cancels the run and keeps already streamed output.

The agent loop has no fixed turn count. A run ends when the model completes or
refuses, the user cancels, a failure occurs, or the configured token spend
budget is exhausted. The default spend budget is 10 million input+output
tokens per run. Change it in `/model`; `0` disables the guardrail. Warnings are
journaled at 50% and 90%.

Context pressure is handled separately. Presets seed a context window, and
automatic compaction triggers near 80% of it. `/compact` forces the same
process manually. Compaction changes model context, not the human-readable
transcript or the authoritative journal.

### Image attachments

Pasting or dragging exactly one existing absolute image path turns it into an
attachment chip. `~` is accepted; bare relative names and mixed text are not
auto-detected. Supported formats are PNG, JPEG, WebP, and GIF up to 4 MiB.

On send, CLAT copies the image into the session attachment directory before
journaling the turn. The journal stores a reference rather than image bytes.
Vision-capable endpoints receive native image input. If an OpenAI-compatible
endpoint rejects the image, CLAT retries with a text note that names the local
attachment path so a configured vision tool can inspect it instead.

### Rendering, copy, and notifications

Assistant Markdown supports fenced code, inline code, emphasis, headings,
lists, blockquotes, links, and horizontal rules. Unsupported syntax degrades
to plain text; model output is never interpreted as terminal control input.

CLAT plays a focus-aware notification when an unattended run finishes or a
permission/question dialog needs attention. Set `CLAT_NO_BELL=1` to disable
it, or `CLAT_BELL_COMMAND` to a detached shell command that should provide the
sound. When native system audio is unavailable, CLAT falls back to the
terminal bell.

### Model and usage status

The header shows the active model and reasoning level. `Shift+Tab` changes the
level for the next run and persists the effective model configuration:

- DeepSeek, GLM, and Kimi use CLAT's Low / High / Max ladder.
- Qwen maps Low / High / Max to its `low` / `medium` / `xhigh` values.
- Custom profiles on a known vendor domain use that vendor's mapping.
- Unknown endpoints do not receive an inferred reasoning parameter; configure
  it explicitly in Extra Body.

Cache and context values are session facts restored from journal usage events.
Where supported, DeepSeek shows wallet balance and GLM/Kimi show remaining
plan quota. Missing provider data is shown as unknown rather than fabricated.

## Headless runner (`clat exec`)

`clat exec` performs one agent run and exits:

```bash
clat exec "run cargo test and summarize any failure"
git diff | clat exec "review this diff"
clat exec --continue "continue with the second issue"
clat exec --session SESSION_ID "check the previous fix"
```

### Input and output contract

- With only a positional argument, it is the prompt.
- With only piped stdin, stdin is the prompt.
- With both, the argument is the instruction and stdin is labelled context.
- Piped input is capped at 8 MiB. Use `--` before a prompt beginning with `-`.
- Normal stdout contains assistant text only, streamed with one terminating
  newline. Status and tool activity go to stderr; `--quiet` suppresses them.
- A broken stdout pipe cancels the run and exits non-zero instead of reporting
  a successful truncated answer.

Exit codes are `0` for success, `1` for runtime failure, `2` for usage error,
and `130` for Ctrl-C. The first Ctrl-C requests a graceful cancellation; a
second hard-exits.

### Sessions, trust, and permissions

Each invocation starts a fresh session by default. `--continue` resumes the
project's newest session; `--session <id>` selects a specific journal. The two
options are mutually exclusive.

An untrusted project fails closed unless `--trust` is present. With terminal
stdin, a side-effecting tool displays its full arguments and waits for `y` +
Enter; `Esc` or any other answer denies it. Input typed before the prompt is
discarded. With piped stdin there is nobody to ask, so side effects are denied
and returned to the model as tool errors. `--yes` approves every side effect,
including command execution, and should be used only inside an already
contained environment.

### Machine-readable events

`--json` replaces text stdout with versioned NDJSON:

```json
{"v":1,"event":{"type":"run_started","project":"/repo","prompt":"hi"}}
{"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"text_delta","delta":"hello"}}}
{"v":1,"event":{"type":"run_completed","output":"hello","turns":1}}
{"v":1,"event":{"type":"exec_completed","exit_code":0}}
```

Run-terminal events close the model/tool lifecycle. The final
`exec_completed` or `exec_failed` line closes the whole invocation after
teardown and persistence, and is the authoritative machine result. Within v1,
event objects may gain optional fields; consumers must ignore unknown fields.
A changed event type requires a new envelope version.

`--json` cannot be combined with `--command`. Permission prompts still use
stderr/stdin; non-interactive consumers normally choose either fail-closed
behavior or an explicitly contained `--yes` run.

### Headless slash commands

`--command /name` dispatches one command through the same core registry and
does not call a model or read stdin:

```bash
clat exec --command /help
clat exec --continue --command /compact
```

Query results use stdout. Interactive-only continuations exit with code 2.

## Local server and web workbench (`clat serve`)

Start the loopback server from a trusted project:

```bash
clat serve
# listening on http://127.0.0.1:2691/
# pair once with the token in ~/.clat/web-token

clat serve --rotate-token
clat serve --port 8099 --token temporary-secret
clat serve --port 0 --token temporary-secret   # OS-assigned test port
```

`--rotate-token` and `--token` are mutually exclusive. `--token` is a
process-only override; it neither reads nor changes the persistent token.

### Security boundary

- The listener is IPv4 loopback-only. There is no `--host` option.
- The default token is created once at `~/.clat/web-token`, stored as a normal
  `0600` file, and reused across restarts.
- `--rotate-token` atomically replaces it and revokes previously paired
  clients after the new server starts listening.
- Credentials are never accepted in URL queries, cookies, manifests, static
  assets, logs, or journals. Authenticated requests use
  `Authorization: Bearer <token>`.
- Requests carrying `Origin` must match the server's exact origin. No CORS
  headers are emitted.
- The token grants the entire local API. This is a single-user, local-machine
  boundary, not multi-tenant authentication.

The static pairing shell and assets are intentionally accessible without a
credential so an installed PWA can always open `/`. They expose no project or
session data. On first use, paste the token from `~/.clat/web-token`; the app
checks it through `POST /auth` and stores its paired copy only in
origin-scoped browser localStorage. An origin includes the port, so a custom
port has separate pairing state.

### Web workbench

The embedded zero-build PWA provides a session sidebar, conversation surface,
and project/model/run/MCP inspector. On narrow screens the side surfaces
become drawers. Browser storage is limited to presentation preferences and the
pairing credential; session content, run state, permission mode, model state,
and MCP facts are rebuilt from authenticated snapshots, journal replay, and
live events.

The workbench supports streaming responses, tool cards, approvals, new/switch/
rename session actions, permission-mode changes, cancellation, and in-run
steering. Full Access requires both a UI warning acknowledgement and the
protocol confirmation described in [Permissions](permissions.md).

The manifest and all asset URLs are credential-free. With the default stable
port and persistent token, an installed PWA survives normal server restarts.
Token rotation returns it to the pairing screen. There is no offline data
mode; without a running server the shell cannot load conversations.

Changing the port changes the browser origin. An app installed from the old
port keeps opening that old origin; it is not redirected automatically. Open
the new `http://127.0.0.1:<port>/` in a browser, pair again for that origin, and
install that origin as a separate app if a standalone window is desired. The
old app can then be removed independently.

### RPC and event contracts

RPC uses `POST /api/<method>` and returns either
`{"ok":true,"value":...}` or
`{"ok":false,"error":{"code":"...","message":"..."}}`.

Current methods are:

- `workbench.info`
- `session.list`, `session.info`, `session.new`, `session.switch`,
  `session.rename`
- `prompt.send`, `steer.send`, `run.cancel`
- `permission.set`
- `approval.respond`

`workbench.info` is lightweight: it returns project, active-session, model,
permission, MCP, capability, and active-run summaries without loading a
transcript or returning credentials. Only one run is active at a time;
`prompt.send` reports `busy` rather than queueing another.

Stable v1 error codes are `bad-request`, `unauthorized`, `forbidden`,
`not-found`, `busy`, `not-pending`, and `internal`. Clients should preserve an
unknown raw code while treating it like `internal`.

`GET /api/events` is an authenticated SSE stream. On connection it sends
journal replay, `subscribed`, any buffered active-run events, and then live v1
RunEvent envelopes. Every accepted prompt ends with one `prompt.settled`
frame. Application notices and approval requests use separate frame kinds;
clients should ignore unknown notices.

There is no resume cursor: reconnecting replays the active session. Slow
consumers are disconnected rather than blocking the agent. If every subscriber
disconnects while an approval is pending, or no answer arrives within ten
minutes, the call is denied. Across several tabs, observation is per tab and
approval is first-answer-wins.

Ctrl-C cancels an active run, closes the Application, flushes the journal, and
stops the listener. A clean shutdown exits 0; accept-loop or persistence
failures exit non-zero for supervisors.

## DeepSeek Harness client (`clat dsh`)

`clat dsh` reuses the CLAT TUI as a client of a local DSH web host:

```bash
clat dsh
clat dsh --port 3080
```

It probes and fingerprints the host. If no DSH host is running and the `dsh`
executable is installed, CLAT starts `dsh web`; a host CLAT started is stopped
when CLAT exits, while a host started by the user is left alone. A non-DSH
process occupying the requested port is rejected.

DSH owns sessions, providers, permissions, tools, and execution. CLAT renders
the host's events and never writes `~/.dsh`. It remembers only the last opened
DSH session id in `~/.clat/dsh-last-session`; missing or invalid memory falls
back to the newest host session.

The shared commands are `/new`, `/resume`, `/model`, `/perm`, `/rename`,
`/clear`, `/help`, and `/quit`. `/compact` and `/mcp` are unavailable because
those concerns belong to the host. The DSH client reconnects after transport
loss, forwards prompts/steering/cancellation/approval answers through the API,
and shows host-reported usage without inventing local wallet data.

## Session and workspace behavior

Local CLAT sessions are project-specific and durable. `/new` persists the
workspace's Fresh selection so reopening does not silently revive the old
conversation, but creates no new session directory or journal until the first
prompt. `/resume` switches the project's active session, and reopening the
project restores that choice. Each materialized session carries its own
permission mode, title, todo state, usage, and transcript.

Only one CLAT process may own the local storage root at a time. A second local
process exits with a clear lease error. This also means `clat`, `clat exec`,
and `clat serve` should not be run concurrently against the same default
`~/.clat` root.

See [Persistent state](storage.md) for the complete file layout, recovery
rules, and the legacy SQLite cutover.

## Safe working habits

- Review the actual arguments in every approval instead of trusting the tool
  name alone.
- Check `git status` and preserve pre-existing work before asking the agent to
  edit. Use focused diffs and normal version-control recovery appropriate to
  your repository.
- Keep Project Write as the normal interactive mode. Use Full Access only when
  the repository and requested operations justify removing all prompts.
- In CI, prefer fail-closed defaults. Use `--yes` only inside a container,
  sandbox, disposable checkout, or similarly explicit containment boundary.
- Remember that `--yes` does not sandbox `run_command`: approved subprocesses
  can use all filesystem, environment, and network authority of the CLAT
  process even though native `write_file`/`edit_file` remain project-fenced.
- Treat MCP servers and WASM components as code you are choosing to run; read
  [MCP security](mcp.md#security-posture) and
  [WASM write grants](wasm.md#filesystem-write-grants) before adding
  third-party extensions.
