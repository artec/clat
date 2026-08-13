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
- `/new` — start a new persisted conversation
- `/clear` — alias for `/new`
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

The input loop uses adaptive polling: it polls every 60 ms while idle and
switches to 16 ms as soon as any input arrives, drifting back after 6
seconds of quiet. Scrolling and typing therefore feel immediate without
paying for constant wake-ups while nothing happens.
