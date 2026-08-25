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
| Manage local plugin packages | `clat plugin <COMMAND>` | local CLAT package store |

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

CLAT loads the root `AGENTS.md`, falling back to `CLAUDE.md`. When an approved
successful file tool reaches a nested path, the next model request also sees
the first instruction file found in each directory from the root to that
path. More-specific scopes appear later and apply only below their directory.
The resolved source paths and digests are durable request-header facts, so a
resume can rebuild the same scope set and detect changed or removed files.

The native `search` tool defaults to case-insensitive literal matching and can
also use bounded regex, globs, extension filters, gitignore/hidden policy and
snapshot-bound cursors. `apply_patch` performs one existing file's exact
multi-hunk update atomically; creation remains `write_file` and v1 does not
delete or rename files.

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

## Plugin packages

Use `clat plugin inspect <package-dir>` to verify a package without changing
state. Install, update, enable/disable, rollback and uninstall are top-level
commands because they operate on the global local package store, not on one
conversation or project:

```bash
clat plugin install ./plugin --config-file ./plugin-config.json \
  --accept-capabilities
clat plugin list
clat plugin update ./plugin-v2
clat plugin rollback dev.example.plugin
clat plugin disable dev.example.plugin
clat plugin uninstall dev.example.plugin
```

Package mutation fails with a busy diagnostic while another CLAT process owns
the storage root. Restart CLAT after a successful activation change; mounted
project scopes intentionally keep their frozen tool/prompt surface until
restart. See [CLAT plugins](plugins.md) for transactions, signatures and trust
labels.

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
| `/plan`, `/plan off` | enter or leave durable Plan Mode at an idle boundary |
| `/memory ...` | explicitly list, add, edit, or delete local memory records |
| `/goal ...` | inspect or control one bounded goal for the current session |
| `/subagents on|off` | opt into or leave the read-only subagent experiment |
| `/perm`, `/permission` | switch Read Only, Project Write, or Full Access |
| `/mcp` | inspect MCP/WASM connection state, tools, and isolated failures |
| `/context` | inspect a one-shot estimated model-context breakdown |
| `/help` | open the command and key reference |
| `/quit`, `/exit` | close CLAT cleanly |

The command catalog is shared with `clat exec --command`. Commands that require
an interactive picker, such as `/model`, `/resume`, and `/perm`, report a usage
error in headless mode instead of inventing a selection.

### Plan Mode, skills, LSP, and context inspection

`/plan` enters durable Plan Mode. The next run receives a planning policy and a
filtered tool catalog: only `Pure`/`Read` tools plus `exit_plan_mode` remain
model-callable. The same frozen policy guards the model schema, durable
`request/header`, direct dispatch, permission construction, and plugin-host
tool calls. When the model submits `exit_plan_mode`, approval saves the plan and
ends that run; implementation tools return only on the next user run. `/plan
off` leaves the mode at an idle boundary. Plan Mode is an agent authority guard,
not an operating-system sandbox.

Skills are instruction bundles discovered one directory deep from three layers:

```text
<project>/.clat/skills/<name>/SKILL.md   # highest priority
~/.clat/skills/<name>/SKILL.md           # user layer
compiled-in skills                        # fallback
```

Each run freezes one deterministic catalog. The model sees lightweight skill
metadata and can use `skill(name, resource?)` to load the selected instructions
or an explicitly referenced file under `references/`, `scripts/`, or `assets/`.
Loading never executes code. A skill marked `requires-execution: true` is exposed
only where CLAT has graduated `sandbox="required"` enforcement; any script still
runs only through the ordinary Execute tools, approvals, ProcessService, and
sandbox policy.

Optional read-only language intelligence is configured only by the user-level
`~/.clat/lsp.json`, for example:

```json
{
  "version": 1,
  "servers": {
    "rust": {
      "command": "rust-analyzer",
      "args": [],
      "extensions": { ".rs": "rust" }
    }
  }
}
```

The `lsp` tool supports `definition`, `references`, `implementation`, and
`hover`. It is `ExternalRead`, disappears from Plan Mode, and starts configured
servers only on demand through CLAT's managed stdio path with
`sandbox="required"` and `network=false`. Project files cannot choose the
server executable. Platforms without the required sandbox fail before spawn.

`/context` takes one read-only snapshot of the next model-facing context and
reports conservative estimates for base prompt, project instructions, plan
policy, skill catalog, tool schemas, history/compaction view, output reserve,
and total, plus skill discovery diagnostics. The numbers are estimates from the
same estimator used by model-request budgeting; the command does not call a
model, write a session event, or start a live monitor.

### Memory, goals, and read-only subagents

Memory is explicit local knowledge, not an automatic transcript extractor.
Only the user-facing command/Application control plane can write it:

```text
/memory list [all|project|user]
/memory show <id>
/memory add <project|user> <content> [--source file:path]
/memory edit <id> <revision> <content>
/memory delete <id> <revision>
```

Project records are visible only in the matching canonical project. User
records are global to this CLAT storage root. Updates and deletes require the
displayed revision, so a stale editor cannot overwrite a concurrent change.
The model receives only bounded run-start injection and the read-only
`memory_search` tool. CLAT never turns model output into memory automatically.
`/context` reports the actual injected byte count (zero when no future prompt is
known) and the fixed 8 KiB injection budget.

Each session may have one current goal:

```text
/goal show
/goal create <objective> [--run] [--rounds N] [--tokens N]
             [--seconds N] [--failures N]
             [--accept user|file-exists:path|file-contains:path:text]
/goal run | pause | resume | complete [summary] | cancel
```

`--run` and `/goal run` are the only operations that arm continuation. Restart,
session switch, an ordinary user prompt, cancellation, or a terminal goal state
removes that process-local authority. Goal state itself is durable and uses
revision/CAS transitions. V1 is capped at 8 rounds, 1,000,000 input+output
tokens, one hour, and 3 failed rounds or rejected completion candidates;
user-supplied limits can only narrow those caps. V1 has no monetary-price
guard: the token cap is the effective cost boundary, and CLAT does not claim a
micro-USD limit without provider pricing evidence. `user` acceptance can only
be completed by `/goal complete`.
File acceptance is project-relative and may be proposed by the model through
`update_goal`, but CLAT verifies it before committing completion.

`/subagents on` exposes `delegate_readonly` for the current session and process;
the default and every restart are off. One call may launch one or two fixed
`explorer`/`reviewer` children. Children have independent empty history, depth
1, only project-confined `list_files`, `read_file`, and `search`, and no memory,
interaction, delegation, LSP, write, execute, network, or session-write tools.
Child count, token, wall-time, task, reference, and output sizes are hard-capped;
parent cancellation propagates to children, and start/end provenance is written
to the parent journal. Child usage is included in the parent run result and
spend ledger; a child reservation that cannot fit the remaining parent/Goal
token budget is rejected before launch. This remains an experiment:
deterministic conformance is not evidence that it improves real-model task
completion.

The shipped HTTP provider adapters consume the child deadline and cancellation
token. The provider interface is cooperative, so a future third-party adapter
must poll the token and honor the deadline; CLAT does not claim that arbitrary
adapter code can be forcibly killed inside the process.

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
including command execution. On macOS the native command tools still default
to workspace-write Seatbelt confinement; on Linux/Windows there is currently
no graduated provider, so unattended `--yes` should run only inside an
external container, sandbox or disposable environment.

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
Memory, goal-state, and subagent enable/disable commands use this same registry;
an armed goal continuation itself requires the TUI or local web workbench so it
has normal approval/event channels. The web workbench routes slash commands
through `command.run`; it never sends them to the model as ordinary prompts.

## Command sessions and sandbox controls

The model-facing command surface has two session tools plus a compatibility
wrapper:

- `exec_command` starts a command in the project or a project-relative
  workdir. It waits up to `yield_time_ms`; a still-running command returns a
  numeric `session_id`. Command text is capped at 64 KiB.
- `write_stdin` writes characters, closes stdin, polls for output, or
  terminates that same-run id. An empty `chars` value is a poll. Raw characters
  are redacted from the durable tool-call journal; the model tool rejects
  `sensitive: true` rather than pretending to protect a secret already sent to
  the model. One write is capped at 256 KiB; five seconds of write backpressure
  terminates the session instead of starving cancellation/TTL teardown.
- `run_command` remains the one-shot project-root API and uses the same core
  ProcessService and command-text cap.

`tty: true` requests a real PTY on macOS/Linux; Windows rejects PTY sessions
until its process-tree isolation graduates. At most eight sessions may run in
one run. Every id belongs to exactly that run/session; cancellation, run end,
TTL, explicit termination or Application shutdown stops and reaps its process
group and ordinary descendants. Output is incrementally consumed from 256 KiB
per-stream transient rings, and one tool result is capped at 64 KiB. A `lossy`
or `output_truncated` flag means the cursor fell behind or more bounded output
remains.

That ownership guarantee covers CLAT-controlled lifecycle paths and descendants
that remain in the owned group. A deliberately daemonized child that creates a
new session/group can escape it. A fatal native crash, `SIGKILL`, power loss,
or another failure that cannot run teardown needs an external supervisor or
container boundary; macOS cannot promise parent-death cleanup for those cases.

Native command calls accept `sandbox: auto|required|off` and `network`:

- macOS `auto`/`required` uses a functionally probed Seatbelt profile outside
  Full Access. Network is denied unless explicitly requested. `off` requires
  Full Access. Account-readable files and inherited environment variables stay
  visible; Workspace Write also permits `/tmp` and the process temporary
  directory.
- Linux/Windows `auto` reports `provider=none, enforcement=none`;
  `required` fails closed. This is lifecycle supervision, not OS confinement.

Process completion also appears as an Application notice in TUI/PWA. It
contains only id and terminal metadata, never the command or raw output.

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
  headers are emitted by the local server. The static shell CSP permits one
  outbound public-data origin, `https://pi.at.cn`; its catalog request omits
  credentials and cannot call the local API on behalf of that origin.
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

Model lifecycle and reasoning traces use human-readable labels in the visual
surface (`Model request started`, `Reasoning summary`, and so on). The stable
wire event id remains available as diagnostic metadata; the frontend does not
rename or mutate the underlying `RunEvent` protocol. The workbench uses one
consistent inline SVG icon language for actions, tools, traces and panel
navigation, including keyboard and reduced-motion accessibility.

The sidebar's Plugin Index opens a searchable, display-only projection of the
public [pi.at.cn](https://pi.at.cn) catalog. That cross-origin GET explicitly
omits credentials, cookies, referrer and the local Bearer token. When the
catalog is unavailable, the PWA shows a clearly labelled built-in preview and
retains the external link. It cannot install or update packages; use the
signed local `clat plugin market` workflow documented in
[CLAT plugins](plugins.md#signed-remote-market).

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
- In CI, prefer fail-closed defaults. On macOS the default Seatbelt profile is
  an additional boundary, but exported environment secrets remain visible.
  On Linux/Windows use `--yes` only inside a container, sandbox, disposable
  checkout, or similarly explicit containment boundary.
- Inspect the returned `sandbox.provider`, `enforcement`, `policy_digest`,
  fallback and denial facts. Process-tree cleanup proves lifecycle ownership;
  it does not prove OS confinement on a fallback platform.
- Treat MCP servers and WASM components as code you are choosing to run; read
  [MCP security](mcp.md#security-posture) and
  [WASM write grants](wasm.md#filesystem-write-grants) before adding
  third-party extensions.
