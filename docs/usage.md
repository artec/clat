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
  rotating spinner plus a "Thinking…" label that breathes (the whole
  label fades in and out at a fixed period, so the rhythm never changes
  with the label length) with an elapsed counter, in the style of the
  DeepSeek Harness.

## Keyboard

| Key | Action |
|---|---|
| `Enter` | submit; while a run is active, submit steering — the message joins the run at the next step boundary (`steering·N` in the status line until claimed) |
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
| `Esc` | cancel a running request (partial text is kept); otherwise clear the input |
| `Ctrl+C` | re-copy the current selection if one exists; otherwise quit |

Releasing a drag copies the selection immediately and overwrites the
system clipboard; `Ctrl+C` re-copies it (or quits when nothing is
selected — most terminals intercept `Cmd+C` while mouse reporting is
on, so `Ctrl+C` is the reliable retry path). Hold `Shift` while
dragging to use the terminal's own selection instead.

While a permission dialog is open, `Esc` denies the pending tool call
instead of cancelling the run.

## Commands

- `/model` — configure the active model/provider
- `/new` — start a new persisted conversation (an empty conversation is
  dropped automatically when you leave it)
- `/clear` — alias for `/new`
- `/compact` — summarize this conversation's earlier turns into a compact
  marker to free context; runs in the background (Esc cancels) and the
  original history stays on disk
- `/model` — configure the active model/provider. The advanced rows include
  a context-window field: once set, history that exceeds the window is
  compacted automatically at the start of the next run (compaction inherits
  the previous summary, so earlier facts survive repeated compactions).
  Without a context window, automatic compaction stays off and only
  `/compact` triggers it
- `/resume` — pick a previous conversation of this project and continue
  it; the list shows title and message count, with the current
  conversation marked. Entering a conversation (even read-only) makes
  it the startup conversation for the next launch
- `/help` — open the help dialog (commands and keys; `↑`/`↓`,
  `PgUp`/`PgDn` scroll, `Esc` closes). Like every popup it is sized to its
  content and keeps margins on all four sides
- `/mcp` — inspect MCP servers: connection overview, per-server transport,
  negotiated protocol and server versions, registered tool counts, and
  mount failures (including the server's stderr tail, which is what npx
  errors show up in). `↑`/`↓`/`PgUp`/`PgDn` scroll, `r` re-reads the
  status, `Esc` closes
- `/quit` or `/exit` — leave CLAT

Sessions get their title from your first message; after a successful run
CLAT may replace that default once with a shorter model-generated title
(manual renames always win). The model can also maintain a per-session
todo list via the `todo_write` tool — it only touches CLAT's own session
state and needs no approval.

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
- **Exit codes**: `0` success, `1` failure, `2` usage error, `130`
  interrupted by Ctrl-C.
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

Custom (non-preset) endpoints don't participate: the keypress flashes a
hint and nothing changes. The advanced model editor is the escape hatch
for full control: committing an edit to the raw extra body, model,
endpoint, or protocol clears the saved level — the raw configuration
becomes the source of truth. For example, writing
`thinking.type: "disabled"` into the extra body by hand hides the
title-bar indicator; the next `Shift+Tab` re-enables thinking at the
cycled level.

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

## MCP tools

Tools exposed by configured MCP servers (`~/.clat/mcp.json`) appear with
an `mcp_{server}_{tool}` name. Servers can be local subprocesses
(`command`/`args`/`env`) or remote Streamable HTTP endpoints
(`url` + `headers`). Their untrusted annotations refine the
permission label, but every call still opens a permission dialog. MCP
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

## Storage layout

Session facts live in append-only DSH-compatible JSONL logs (zstd-framed)
under `~/.clat/sessions/<project>/<session-id>/session.jsonl.zstd` — one
authoritative log per session, replayed through projections on open.
`~/.clat/clat.db` keeps only control-plane state (model configuration,
profiles, trusted projects, the per-project "current session" pointer);
`~/.clat/config.json` is the control-plane version sentinel.

One CLAT process at a time holds the storage root (a kernel-level lease);
a second process exits with a clear error until the first one closes.

Sessions are materialized lazily: `/new` writes nothing until the first
prompt, and a session with no content never appears on disk. Input recall
(`↑`/`↓` after a restart) comes from the conversation transcript itself.

### Upgrading across the storage rewrite

Pre-0.6 storage (SQLite `sessions`/`messages` tables) is **not migrated**:
CLAT refuses to start with instructions to remove the old files, because
only pre-release builds ever wrote them. Deleting `~/.clat/config.json`,
`~/.clat/clat.db`, and `~/.clat/sessions/` (keep `mcp.json`) starts fresh;
old conversations cannot be carried over.
