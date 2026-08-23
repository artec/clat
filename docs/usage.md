# Using the TUI

Running `clat` with no arguments opens the terminal UI in the current
project directory. The UI appears immediately — resuming the previous
conversation loads in the background, showing the CLAT logo with a
`loading conversation…` status while input stays disabled; once the
session is ready the conversation appears and input unlocks.

## Panels

- **Conversation** — persisted user/assistant messages for the latest
  conversation in this project. User messages render as a soft dark
  block with a bright yellow `❯` marker; assistant messages are plain
  text with a quiet `⏺` marker — while a response is streaming the
  marker becomes a rotating quarter-circle (◐◓◑◒) so a long wait is
  never a still screen. A vertical scrollbar shows the position.
- **Message input** — cursor- and Unicode-aware, grows up to eight rows
  for multi-line messages.
- **Status line** — model/tool status, kept separate so status messages
  never pollute chat history. While the model reasons, it shows a
  rotating spinner plus a "Thinking…" label swept by a searchlight: a
  soft band of light advances one character every 80ms (each character
  stays lit for a fixed dwell; earlier characters return to the base
  color as the light passes), and the whole sweep simply takes longer on
  longer labels. The spinner is on its own constant rhythm — one frame
  per two illuminated characters — so neither the sweep length nor the
  label length changes its speed. An elapsed counter rides along, in the
  style of the DeepSeek Harness.

## Keyboard

| Key | Action |
|---|---|
| `Enter` | submit; while a run is active, submit steering — the message joins the run at the next step boundary and appears immediately as a dim `· queued` block at the end of the conversation (the `steering·N` badge counts the same queue). If the run has just reached its end, the message is submitted as a new run instead — a submitted message is never silently dropped |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | insert a line break |
| `←` / `→`, `Home`, `End` | move the cursor |
| `Backspace` / `Delete` | edit |
| `↑` / `↓` | recall input history; with no history, scroll the conversation |
| `PageUp` / `PageDown` | scroll the conversation |
| `Shift+Tab` | cycle the thinking level (`Low → High → Max`) |
| mouse wheel | scroll the conversation (2 rows per notch) |
| drag with the mouse | select text and copy it on release (OSC 52); a click positions the input cursor |
| `Cmd+C` / `Ctrl+Shift+C` | copy the current selection again (CLAT sends OSC 52; iTerm2/WezTerm/kitty/VS Code honor it) |
| `Cmd+X` / `Ctrl+Shift+X` | cut the input-box selection |
| `Esc` | stack semantics while a run is active: recall the most recent queued steering back into the input (edit and re-submit, the run keeps going); when nothing is queued, cancel the running request (partial text is kept). Otherwise clear the input |
| `Ctrl+C` | re-copy the current selection if one exists; otherwise quit |

Releasing a drag copies the selection immediately and overwrites the
system clipboard; `Ctrl+C` re-copies it (or quits when nothing is
selected — most terminals intercept `Cmd+C` while mouse reporting is
on, so `Ctrl+C` is the reliable retry path). Hold `Shift` while
dragging to use the terminal's own selection instead.

While a permission dialog is open, `Esc` denies the pending tool call
instead of cancelling the run.

## Commands

- `/model` — configure the active model/provider. Custom models are
  named profiles (each keeps its own API key; switching never loses
  keys); see [the model editor](model-editor.md). The editor's
  context-window field drives automatic compaction: once set, history
  that exceeds the window is compacted at the start of the next run
  (compaction inherits the previous summary, so earlier facts survive
  repeated compactions). Presets and the custom-profile template both
  seed this field, so automatic compaction works out of the box — it
  triggers when the estimated context passes 80% of the window and
  compacts back below it; a value you enter yourself always wins over
  the seed
- `/new` — start a new persisted conversation (an empty conversation is
  dropped automatically when you leave it)
- `/clear` — alias for `/new`
- `/compact` — summarize this conversation's earlier turns into a compact
  marker to free context; runs in the background (Esc cancels) and the
  original history stays on disk
- `/resume` — pick a previous conversation of this project and continue
  it; the list shows title and message count, with the current
  conversation marked. Entering a conversation (even read-only) makes
  it the startup conversation for the next launch
- `/perm` — switch the permission mode: **Read Only** (every side effect
  asks), **Project Write** (default — file edits, reads, and
  network/search tools run without prompts; commands and destructive
  tools still ask), or **Full Access** (no prompts at all; needs a
  confirm step, and unlocks absolute-path writes). The active mode is
  shown at the top right of the input box. Switching takes effect at
  the next permission check and **travels with the conversation**: it
  is journaled into the session (`sandbox/mode` events, DSH
  vocabulary), so restarting — which auto-resumes the startup
  conversation — picks up where you left off, while `/new` starts at
  the default and resuming a different conversation restores *its*
  mode. The permission dialog itself also offers escalation keys
  (`w` / `f`) for exactly the wider modes that would let the pending
  call run — see `docs/permissions.md`. The long form `/permission`
  is an alias
- `/rename` — rename the current conversation (available as soon as the
  conversation exists; a late automatic title never overwrites a manual
  name). The conversation title shows at the top right of the
  conversation box
- `/help` — open the help dialog (commands and keys; `↑`/`↓`,
  `PgUp`/`PgDn` scroll, `Esc` closes). Like every popup it is sized to its
  content and keeps margins on all four sides
- `/mcp` — inspect MCP servers: connection overview (including a
  `connecting` count while servers are still starting up in the background),
  per-server transport, negotiated protocol and server versions, registered
  tool counts, and mount failures (including the server's stderr tail, which
  is what npx errors show up in). `↑`/`↓`/`PgUp`/`PgDn` scroll, `r` re-reads
  the status, `Esc` closes
- `/quit` or `/exit` — leave CLAT

Sessions get their title from your first message; after a successful run
CLAT may replace that default once with a shorter model-generated title
(`TitleUpdated` refreshes the conversation's top-right title immediately;
manual `/rename` always wins). The model can also maintain a per-session
todo list via the `todo_write` tool — it only touches CLAT's own session
state and needs no approval.

## Image attachments

Drag an image file into the terminal — CLAT detects that the pasted
text is exactly one existing image **absolute path** (`~` allowed;
png / jpg / webp / gif, ≤4MB) and turns it into an attachment chip
above the input instead of inserting the path as text. Bare relative
filenames are never treated as attachments. `Enter` sends it with your message, `Esc`
drops it; mixed text pastes are never misdetected. You can also type a
path mentioning the file and let the model read it via an MCP vision
tool.

Attachments are **copied into the session's own storage** when the
message is sent, so reopening the conversation later still works after
you clean up the original file. The journal stores only the reference —
image bytes never enter the log. In context, an image is counted as
**vision tokens** (estimated from its resolution, ~512px tiles), which
the auto-compaction budget includes: old images leave the context when
their turns are compacted, and oversized files are rejected up front.
If the chat endpoint is not a vision model, the first request is
rejected with a 400 — CLAT detects that, replaces the image with a
text note carrying the attachment's file path, and retries
automatically; the model can then analyze the file through a vision
MCP tool (the GLM vision tools accept local paths), or tell you it
cannot see images. On a vision endpoint, images are sent natively and
this never triggers.

## Notifications

When a run finishes (successfully or with an error — but not when you
cancel it yourself) or a permission / ask dialog opens and waits for
you, CLAT plays a **system notification sound** (2026-08-21, replacing
the terminal bell — which most terminals render too quietly to notice):
macOS plays `Funk.aiff` through the built-in `afplay` with a slight
volume boost, Linux tries `paplay` / `aplay` on the freedesktop or ALSA
sample sounds, Windows uses PowerShell's `SoundPlayer` with a system
notification wav. No audio library is linked for this — it is always a
detached system player. When the player or sound file is missing (a
minimal Linux, say), CLAT falls back to the terminal bell (`\x07`), and
what that sounds like is your terminal emulator's bell setting.

For full control set `CLAT_BELL_COMMAND` to any shell command — it runs
detached when a notification fires (macOS:
`afplay -v 3 /System/Library/Sounds/Submarine.aiff`; Linux:
`paplay /usr/share/sounds/…`). `CLAT_NO_BELL=1` silences notifications
entirely.

Notifications are focus-aware (2026-08-23): CLAT enables terminal focus
reporting (DECSET 1004), so while the terminal is focused — you are
watching the dialog or the run finish — the bell stays silent. It rings
when the terminal is unfocused (the AFK case the bell exists for). If
your terminal or multiplexer doesn't report focus changes (tmux doesn't
forward them by default), CLAT can't tell and keeps ringing — missing a
notification hurts more than an extra ring; silence it explicitly with
`CLAT_NO_BELL=1` if that's your setup.

## Markdown rendering

Assistant messages go through a small built-in renderer:

- fenced code blocks — full-width highlighted background
- inline code, **bold**, *italic*
- `#`–`###` headings, `-`/`1.` lists, `>` blockquotes
- `[label](url)` links, `---` rules

Anything unrecognized degrades to plain text; no external markdown
dependency is used.

## Input responsiveness

The UI is fully event-driven. A dedicated input thread blocks on terminal
events and forwards them the instant they arrive, so keystrokes and mouse
input have zero polling latency. When nothing happens, the main loop
suspends until the next event (or the next scheduled repaint, such as a
spinner frame or a status flash expiring) — an idle CLAT consumes no CPU.

## Project trust

The first time CLAT opens a directory you have not trusted before, it
shows a full-screen trust dialog instead of the chat UI. Until you press
`Enter`/`y` (trust and remember) or `Esc`/`n` (exit), no session is
created, no project file is read, and no MCP server is started — the
prompt is a security boundary, not a decoration.

## Headless runs (`clat exec`)

`clat exec` runs one agent turn without the TUI and exits — for scripts,
CI, git hooks, or piping a question into an answer:

```bash
clat exec "跑一遍 cargo test 并总结失败原因"
git diff | clat exec "review this diff"
clat exec --continue "继续，把第二个问题也修掉"
```

Conventions:

- **stdout carries only the assistant text** (streamed, plus a terminating
  newline); turn markers, tool activity, and token summaries go to stderr.
  Piping stdout is always safe. `--quiet` suppresses the stderr chatter.
  If writing to stdout fails (e.g. the downstream consumer closed early,
  like `clat exec … | head`), CLAT cancels the run and exits non-zero
  instead of pretending the truncated output succeeded.
- **JSON event stream** (`--json`): stdout switches from assistant text to
  an NDJSON event stream — one JSON object per line, in emission order:

  ```json
  {"v":1,"event":{"type":"run_started","project":"/repo","prompt":"hi"}}
  {"v":1,"event":{"type":"model_requested","turn":1,"provider":"glm","model":"glm-5.3"}}
  {"v":1,"event":{"type":"model_stream","turn":1,"event":{"type":"text_delta","delta":"你好"}}}
  {"v":1,"event":{"type":"run_completed","output":"…","turns":1,"usage":{"input_tokens":10,"output_tokens":5}}}
  {"v":1,"event":{"type":"exec_completed","exit_code":0}}
  ```

  The stream has two layers. Run events project the full `RunEvent`
  vocabulary (model turns, tool calls, permission decisions, retries);
  `run_completed` / `run_cancelled` / `run_failed` close the **run**
  lifecycle — exactly one per run. The **invocation** result is the last
  line: exactly one `exec_completed` (exit 0) or `exec_failed` (non-zero
  exit code plus message), emitted only after every exit-code-affecting
  step (run teardown, application close) has finished. Machine consumers
  treat the exec final as the authoritative result — a run terminal
  alone never implies the process exits zero, and completion is never
  guessed from EOF. The `v` field is a version commitment: within v1,
  existing event types may gain optional fields (tolerate unknown
  fields), while adding or changing an event **type** is a vocabulary
  change that ships as v2 — never by editing v1 lines.
  `run_started.project` is a UTF-8 display path; non-UTF-8 paths are
  transcribed lossily and explicitly marked with a
  `project_utf8_lossy` field, never silently. Exit codes, stderr status
  output, `--quiet`, and broken-pipe semantics are all unchanged.
  Permissions stay out of the stream in this phase: interactive prompts
  still go to stderr/stdin, so machine consumers either pass `--yes` or
  consume the `permission_denied` events that a non-interactive run
  produces. `--json` cannot be combined with `--command` (that path has
  its own plain-text stdout contract).
- **Exit codes**: `0` success, `1` failure, `2` usage error, `130`
  interrupted by Ctrl-C.
- **Commands**: `--command /xxx` runs one slash command through the same
  core registry as the TUI instead of a model run — no prompt argument, no
  stdin read. Query commands print their result on stdout
  (`--continue --command /compact` joins the compaction and reports on
  stderr; `--command /help` lists the catalog); commands whose continuation
  is an interactive picker (`/model`, `/resume`, `/perm`) exit `2` with a
  clear note.
- **Prompt** comes from the argument list, from stdin when piped, or from
  both: with a piped stdin the positional argument is the instruction and
  the piped input is attached as labelled context
  (`git diff | clat exec "review this diff"`). Piped input is capped at
  8 MiB. Use `--` before a prompt that itself starts with `-`.
- **Sessions**: each `exec` starts a fresh session by default
  (predictable for scripts); `--continue` resumes the project's most
  recent session, `--session <id>` a specific one (`<id>` is the UUID
  shown by the TUI `/resume` picker). Sessions created by `exec` show up
  in the TUI with auto-generated titles, like any other.
- **Permissions**: with a terminal stdin, side-effecting calls prompt
  inline with the full arguments shown; type `y` and press `Enter` to
  allow, `Esc` or any other answer denies. Input typed before a prompt
  appears is discarded, so a stale `y` can never approve a future call.
  When stdin is a pipe — the usual case in scripts and CI — side effects
  are **denied** with the model told why, so the run still completes.
  `--yes` approves everything (dangerous: includes `run_command`).
- **Trust**: an untrusted directory fails closed with a hint; pass
  `--trust` to accept non-interactively (equivalent to pressing `y` in
  the TUI dialog).
- **Model**: `exec` uses the persisted model configuration. If none is
  configured it exits with an error pointing at `/model` in the TUI.
- **Ctrl-C**: the first press cancels gracefully — including while a
  permission prompt is waiting, which resolves to deny and unwinds the
  run (partial output still persists, exit 130). A second press
  hard-exits.

## DSH client (`clat dsh`)

`clat dsh` opens the **same CLAT TUI** — identical header, input box,
status bar, dialogs, and key bindings — with dsh as the event source and
command backend for a local
[DSH](https://github.com/deepseek-ai/deepseek-harness) web host (the
terminal seat DSH itself retired). It is a fill-in, not a rival: it never
writes `~/.dsh`, and every action goes through the DSH API.

```bash
clat dsh                    # probe 127.0.0.1:3080, connect or start dsh web
clat dsh --port 3080        # explicit port
```

The flow is strictly online: probe → fingerprint (`host.describe`) →
connect, or start `dsh web` if nothing DSH-shaped is listening (a
non-DSH squatter is not trusted; if the default port is taken the child
retries with an OS-assigned port). No `dsh` executable and no `~/.dsh`
means a clear "not installed" error. The header carries the mode mark —
`CLAT ● dsh` (green dot while connected, red `○` while disconnected);
the second line and the status bar show the **open session's workspace** —
like the browser page, `clat dsh` is just another client of the host, so
where you ran the command from is irrelevant (the local directory only
matters for plain local `clat`). If a session's workspace was never
recorded, the host process directory is shown as a fallback.
On connect `clat dsh` reopens **its own last-open session** (remembered
in `~/.clat`, like the browser page's local memory); on first run — or
if that session no longer exists — the head of the host's list is
restored instead (newest first: the last session created or prompted,
including a fresh blank one; the host itself keeps no "last opened"
state, and merely viewing an old session elsewhere changes nothing —
use `/resume` to pick explicitly) (`session.history`); typing sends
(`session.prompt`), typing while a
run is active steers (with the same pending-echo badge as local), `Esc`
cancels (no steering recall — the host queue has no recall API). A
turn that finishes while you are away rings the same focus-aware bell
as local runs (a turn you cancel yourself does not).
Approvals and questions raised by the host pop the **same dialogs** as
local runs (the question dialog prefixes `(1/N)` when a host question
set has several entries) and are answered through the API. A dropped
connection shows a top notice and **reconnects automatically** — if the
host process itself died, clat restarts it (`dsh web`) as part of
reconnecting, and takes a host it started itself back down when you quit
(a host you started by hand is never touched). There is no offline mode. The status bar
hides the wallet segment (that monitors local keys) and projects
cache/context from the host's usage events instead — segments whose
denominator the host has not reported stay hidden rather than invented.

Commands: `/new` `/resume` `/model` `/perm` `/rename` `/clear` `/help`
`/quit`. `/resume` reads `~/.dsh/storages` directly (read-only) and shows every
workspace as a group (header rows, all workspaces always listed), opening
at the current workspace's group; picking a session switches the host to
it (adoption protocol, so its events stream immediately).
`/model` shows the host's provider groups dynamically — including
web-defined custom groups and, honestly grayed out, providers that
failed to list; enter selects (the header label refreshes). `/perm`
offers the host's three presets (Read Only / Workspace Write / Full
Access) and applies through the host's own `/permission` command — the
input-box corner label mirrors the session's live preset. `/compact`
and `/mcp` answer "not available in clat dsh mode" — the DSH host owns
those concerns.

## Local API server (`clat serve`)

`clat serve` exposes the same agent over a local HTTP+SSE API — the
foundation for the browser client (PWA-4) and usable directly from
scripts and editors:

```bash
clat serve                 # binds 127.0.0.1, picks a free port, prints the URL
clat serve --port 8099 --token my-secret   # explicit values (scripts/CI)
```

**Security (fail-closed, loopback only)**: the listener binds
`127.0.0.1` only (there is no `--host` — widening the boundary is a
product decision, not a config option); every request must carry the
per-run token, which is generated at startup, printed once, never
written to disk or logs, and dies with the process. API calls
(`POST /api/…`) require the `Authorization: Bearer …` header — the
`?t=…` query form is accepted only for `GET` (the landing page and
SSE, where browser APIs cannot set headers), keeping the credential
out of URL-borne surfaces like history and referrers. Requests
carrying an `Origin` header are rejected unless it is exactly this
server's own origin (a malicious page in your browser cannot call the
API), and no CORS headers are ever sent. An untrusted project refuses
to start — trust it once from a terminal (`clat exec --trust`) first.
This is a **single-user** boundary: the token grants the whole API
(all sessions, all methods). Scoped tokens or per-client session
ownership are deliberately deferred until any non-local client exists
— that day re-opens the threat model, exactly like trusted-LAN.

**RPC methods** (`POST /api/<method>`, JSON body, JSON
`{"ok":true,"value":…}` / `{"ok":false,"error":{code,message}}`
replies): `session.list` / `session.info` / `session.new` /
`session.switch` / `session.rename`, `prompt.send` (starts a run;
busy runs are rejected with `busy` rather than queued),
`steer.send`, `run.cancel`, and `approval.respond` (answers an
approval request by its `rpcId`; first answer wins, late answers get
`not-pending`). New methods may appear without notice; unknown
methods are `bad-request`. The v1 error-code catalog is frozen at
`bad-request` / `unauthorized` / `forbidden` / `not-found` / `busy` /
`not-pending` / `internal`; new codes may be added (clients should
map unknown codes to `internal` behavior while keeping the raw
string for diagnostics).

**Event stream** (`GET /api/events`, SSE): on connect the server
replays the journal history (`replay` frames — the same source as
`/resume`), confirms with `subscribed`, replays the active run's
buffered events from the run's head if a run is mid-flight, then
streams live events. Live `event` frames carry the same versioned v1
RunEvent envelopes as `clat exec --json`, byte-for-byte; each
accepted prompt ends with exactly one `prompt.settled` frame
(completed/cancelled/failed — the invocation verdict; a run terminal
alone never implies the process is done). Approvals arrive as
`approval.requested` frames — answer them via `approval.respond`;
while an approval is pending, cancelling the run, disconnecting every
subscriber, or the 10-minute timeout each deny the call
(fail-closed). Application notices (compaction, monitor, title, MCP
startup) arrive as `notice` frames; ignore unknown notice kinds.
Reconnecting replays everything from the journal — there is no
resume cursor. A slow consumer is disconnected (bounded queues never
block the agent) and simply reconnects. The built-in landing page at
`/` shows quick examples; the browser UI itself is the next release
(v0.9.0).

`clat serve` adds no new dependencies — the HTTP layer is a
hand-written minimal subset, and the server shares the exact
permission model, session journal, and single-run semantics of the
TUI and `clat exec`. Ctrl-C stops it gracefully (cancel active run,
close the application, flush the journal).

### Web client & PWA

The URL `clat serve` prints opens a built-in web client (served from
the same binary — no Node, no build step). It is a pure projection of
the event stream: refresh or reconnect always rebuilds the view from
the journal replay, session content is never stored in the browser,
and model output is rendered text-only (no HTML). The client supports
the full loop: streaming answers, tool cards, approval prompts
(answer from any tab — see below), session list/new/switch/rename, and
in-run steering (pressing Enter while a run is active queues a
steering message).

**Multi-client semantics** (several tabs on the same serve):
observation is per-tab (each subscribes independently and sees the
same events); approval is *first-answer-wins* — any tab holding the
token may answer, the winner executes, later answers are told
`not-pending`; cancellation is idempotent from any tab. Opening the
page without a valid token shows a landing state where you can paste
a `clat serve` URL.

**PWA install**: the app declares a manifest and icons, so Chromium
browsers offer installation (it launches like a native app at its own
window). The per-run token travels inside the manifest/icon URLs, so
installs are bound to the running serve process — a freshly restarted
`clat serve` generates a new token and an installed app opens in the
landing state until reconnected. There is deliberately no offline
shell: without a running serve there is no data. e2e tests for the
client live under `web/e2e` (Playwright, developer-side only — run
`npm test` there; the shipped assets stay zero-build).

## Title bar and status line

The title bar carries the model name followed by the current thinking
level (`CLAT v… ready · GLM 5.3 · Thinking · High`); on narrow terminals
it degrades step by step (dropping spacing, then the model name) so the
level stays visible. The bottom-right status line shows the model
telemetry; **Cache and Context are always present** for known-vendor
presets (`--%` / `0/1M` until data exists — the layout never jumps),
and their values restore from the journal at startup or `/resume`, so
long sessions show real numbers immediately. The balance/quota segment
(`Wallet`/`Token`) exists for DeepSeek, GLM, and Kimi Coding (Kimi
shows the 5-hour-window remaining percentage, like GLM). Qwen Token
Plan has no public balance API — its status line carries Cache/Context
alone:

- **DeepSeek**: `Wallet: ￥89.35 · Cache: 99.99% · Context: 120k/1M` —
  account balance, session cache-hit ratio, context usage.
- **GLM Coding Plan**: `Token: 87% · Cache: 99.99% · Context: 120k/1M` —
  the slot shows the 5-hour window quota instead of the balance.

When the terminal is narrow the right side yields by priority
(`Context` first, then `Cache`) so regular status messages on the left
keep a minimum width. A background monitor refreshes balance/quota
every five minutes and immediately after each run or model switch.
`Context` current value is the input+output tokens of the most recent
model request (the approximate starting point of the next one); the
denominator is the official context window (1M for both DeepSeek V4
and GLM 5.3). The telemetry belongs to the current session: switching
or starting a conversation restores the target session's journal
statistics (a brand-new conversation starts from `--%` / `0/1M`), and
a successful compaction clears the context reading (a failed one
leaves it intact). During a run the cache ratio and context update
live from each response's usage report, not only at the end.

## Thinking level (`Shift+Tab`)

`Shift+Tab` cycles the vendor's effective reasoning levels and the
choice is persisted with the model configuration, so it
survives restarts and applies to headless `clat exec` runs too. It
takes effect on the next run (a running turn is not interrupted):

- DeepSeek V4, GLM 5.3, and Kimi K3 cycle `Low → High → Max → Low`
  (Kimi's official default is `max`).
- Qwen3.8-Max also cycles three tiers; CLAT's Low/High/Max map onto the
  official `low`/`medium`/`xhigh` ladder (its default is `xhigh`, i.e.
  CLAT's Max).
- DeepSeek V4 also has an official non-thinking mode; CLAT deliberately
  keeps it out of the shortcut cycle (it remains available through the
  raw extra body).
- GLM 5.3 cannot disable thinking at all — the official API rejects
  `thinking.type: "disabled"` outright.
- Presets default to the middle tier (`high`, or `medium` on Qwen's
  ladder); GLM's and Kimi's official default is the top tier, so pick
  `Max` if you want the vendor-recommended setting for coding.

Participation follows the active model's **endpoint domain**, not the
preset flag: a custom profile pointed at one of the four vendor domains
cycles the same way. On any other endpoint the keypress flashes a hint
and nothing changes. The editor remains the escape hatch for full
control, with a per-editor discipline: in the preset editor, committing
an edit to the raw extra body, model, endpoint, or protocol clears the
saved level — the raw configuration becomes the source of truth; the
profile editor has a visible Thinking row, so editing model, endpoint,
or protocol keeps it, and only a hand-written Extra Body clears it (the
row visibly flips to `off`). For example, writing
`thinking.type: "disabled"` into the extra body by hand hides the
title-bar indicator; the next `Shift+Tab` re-enables thinking at the
cycled level. While a profile is active, `Shift+Tab` adjusts the
running configuration only — see [the model editor](model-editor.md)
for how that interacts with the stored profile.

## The agent can edit and run commands

Since v0.3.4 CLAT is not read-only: for trusted projects the model has
`write_file`, `edit_file`, and `run_command`, so it can fix a bug, run
the tests, read the failure, and try again on its own. Two habits make
this safe:

- **Review before approving.** Every write/execute call opens a
  permission dialog: `edit_file` shows the old→new diff, `write_file`
  the full content, `run_command` the command with its working directory
  and timeout. Approval unlocks only after the whole preview has been
  scrolled through — a dangerous tail cannot hide below the fold.
- **Work on a clean tree.** The natural undo for anything the agent does
  is `git checkout .` / `git clean`. Start dogfooding sessions from a
  committed state so a bad edit is always one command away from gone.

`run_command` output is capped (32 KiB per stream) but the command
itself is never killed by the cap; timeouts and `Esc` terminate the
whole process tree, not just the shell.

Long tasks are never cut off by a turn count. The agent loop has no
turn budget (the same design as DeepSeek Harness, Claude Code, and
opencode): a run ends only when the model finishes, you cancel it, or
something fails. Watch the status line for token/context usage, press
`Esc` to stop at any point, and let `/compact` (or the automatic
context budget) absorb context pressure.

What the loop does have is a **spend guardrail**: each run tracks its
`input+output` tokens against a budget (10M by default, per model
configuration — raise, lower, or disable with `0` via `/model` →
Spend Budget). Crossing 50% and 90% journals a durable warning event;
crossing the cap stops the run with an error that names the used and
cap numbers and how to raise them. A new run restarts the count. The
meter is provider-independent: every model request first reserves a
conservative estimate (input estimate + output limit) and the
provider-reported usage, when available, reconciles it — so endpoints
that omit usage in their responses are still bounded by the guardrail.

## MCP tools

Tools exposed by configured MCP servers (`~/.clat/mcp.json`) appear with
an `mcp_{server}_{tool}` name. Servers can be local subprocesses
(`command`/`args`/`env`) or remote Streamable HTTP endpoints
(`url` + `headers`). Their untrusted annotations refine the permission
classification: read-only annotated tools ask under Read Only but run
freely under Project Write, while destructive ones (the default when a
server omits annotations) always ask outside Full Access. MCP
servers are global: their subprocesses run with `~/.clat` as the working
directory, never inside the project. Pressing `Esc` during a call also
propagates cancellation to the MCP request.

With the GLM Coding Plan model active (and an API key configured),
CLAT additionally mounts the four GLM-exclusive MCP servers at startup:
web search (`webSearchPrime`), web reader, repository docs (`zread`),
and the vision suite (needs Node.js for its local helper). They appear
in the `mcp: N server(s) connected` startup note as `glm-search` /
`glm-reader` / `glm-zread` / `glm-vision`. Define a same-named entry in
`~/.clat/mcp.json` to replace or disable any of them.

While a remote tool is running, the server may talk back to CLAT
(2026-08-21):

- **A model-call request** (MCP sampling) opens the permission dialog
  naming the server, its token budget, and a message preview. Approve,
  and CLAT runs the configured model on the server's behalf — the
  tokens are counted in the run's usage like any other model call.
  Deny, and the server's tool fails with a clear error. These requests
  are never auto-approved outside Full Access mode.
- **A question to you** (MCP elicitation) opens the same dialog
  `ask_user` uses, one field at a time — booleans as yes/no, choice
  lists as options, free text otherwise. `Esc` cancels the form;
  declining answers the server with "declined". In headless `clat exec`
  there is no one to ask: the server receives an error instead.

Your answer time on such a question never kills the tool call — its
timeout extends while the question is on screen. Requests outside an
active run (an idle connection asking for model calls) are refused.

## WASM plugins

`~/.clat/plugins.json` adds in-process WebAssembly plugins (2026-08-21,
Phase 2a) — single-file, no Node, sandboxed with zero granted
capabilities:

```json
{
  "digest": { "path": "~/.clat/plugins/digest.wasm" },
  "greeter": {
    "path": "~/.clat/plugins/greeter.wasm",
    "config": { "greeting": "Hola", "upper": true }
  }
}
```

An entry's `config` object is the plugin's own settings — the plugin
reads it through the SDK's `plugin_config()` helper (an unconfigured
plugin that expects config fails loudly).

Their tools appear as `wasm_{plugin}_{tool}` (the pilot `digest` plugin
provides sha256 / base64, and the `read` plugin ports the native read
tools — see `plugins/` in the repository), and the plugins show up in
`/mcp` with transport `wasm`. A plugin may ask for model calls or user
questions through the same dialogs MCP servers use. File access follows
the permission mode: reads of the project root always work, writes only
under Project Write / Full Access (extra directories listed in
`plugins.json` under `dirs` open up under Full Access only); switching
`/perm` applies to the very next tool call. Write access is separately
approved: the first use of a write-capable plugin asks once, showing
the component hash and the directory list (a rebuild or a directory
change asks again; grants persist in `~/.clat/plugin-grants.json`).
See [WASM plugins](wasm.md) for the authoring side.

## Storage layout

Session facts live in append-only DSH-compatible JSONL logs (zstd-framed)
under `~/.clat/sessions/<project>/<session-id>/session.jsonl.zstd` — one
authoritative log per session, replayed through projections on open.
Control-plane state is a small JSON file family beside them:
`~/.clat/settings.json` (model configuration, profiles),
`~/.clat/credentials.json` (remembered per-vendor API keys),
`~/.clat/trust.json` (trusted projects), and
`~/.clat/storages/workspace.json` — a multi-project workspace registry
that remembers each project's current session, so reopening a project
restores its own conversation. `~/.clat/config.json` is the control-plane
version sentinel.

One CLAT process at a time holds the storage root (a kernel-level lease);
a second process exits with a clear error until the first one closes.

Sessions are materialized lazily: `/new` writes nothing until the first
prompt, and a session with no content never appears on disk. Input recall
(`↑`/`↓` after a restart) comes from the conversation transcript itself.

### Upgrading across the storage rewrite

Older control-plane formats are **not migrated**. A v0.4–v0.8 SQLite
`clat.db` is renamed aside automatically (`clat.db.bak-<date>`, kept —
never deleted) the first time the new build mounts, and the control plane
starts fresh: approve trust once per project and re-enter the model
config once. Conversations survive untouched — they live in the session
logs, and each project re-adopts its sessions when reopened
(`/resume` lists them). Pre-0.6 storage (SQLite `sessions`/`messages`
tables) is refused with removal instructions instead.
