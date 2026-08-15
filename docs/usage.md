# Using the TUI

Running `clat` with no arguments opens the terminal UI in the current
project directory.

## Panels

- **Conversation** — persisted user/assistant messages for the latest
  conversation in this project. User messages render as a soft dark
  block with a bright yellow `❯` marker; assistant messages are plain
  text with a quiet `⏺` marker. A vertical scrollbar shows the position.
- **Message input** — cursor- and Unicode-aware, grows up to eight rows
  for multi-line messages.
- **Status line** — model/tool status, kept separate so status messages
  never pollute chat history. While the model reasons, it shows a
  rotating spinner plus a "Thinking…" text-shimmer with an elapsed
  counter, in the style of the DeepSeek Harness.

## Keyboard

| Key | Action |
|---|---|
| `Enter` | submit |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | insert a line break |
| `←` / `→`, `Home`, `End` | move the cursor |
| `Backspace` / `Delete` | edit |
| `↑` / `↓` | recall input history; with no history, scroll the conversation |
| `PageUp` / `PageDown` | scroll the conversation |
| mouse wheel | scroll the conversation (2 rows per notch) |
| `Esc` | cancel a running request (partial text is kept); otherwise clear the input |
| `Ctrl+C` | quit |

While a permission dialog is open, `Esc` denies the pending tool call
instead of cancelling the run.

## Commands

- `/model` — configure the active model/provider
- `/new` — start a new persisted conversation (an empty conversation is
  dropped automatically when you leave it)
- `/clear` — alias for `/new`
- `/resume` — pick a previous conversation of this project and continue
  it; the list shows title and message count, with the current
  conversation marked. Entering a conversation (even read-only) makes
  it the startup conversation for the next launch
- `/help` — show commands
- `/quit` or `/exit` — leave CLAT

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

## Status line extras

- DeepSeek endpoints show the account balance; GLM Coding Plan endpoints
  show the 5-hour window quota. A background monitor refreshes both every
  five minutes and immediately after each run or model switch.
- A cache-hit percentage is shown once the provider reports cached input
  tokens.

## MCP tools

Tools exposed by configured MCP servers (`~/.clat/mcp.json`) appear with
an `mcp_{server}_{tool}` name. Their untrusted annotations refine the
permission label, but every call still opens a permission dialog. MCP
servers are global: their subprocesses run with `~/.clat` as the working
directory, never inside the project. Pressing `Esc` during a call also
propagates cancellation to the MCP request.
