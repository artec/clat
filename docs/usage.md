# Using CLAT

This guide is for people running CLAT. It covers the terminal UI, headless
runner, local web workbench, native desktop, and DSH client. Configuration internals belong in
[Model editor](model-editor.md), security semantics in
[Permissions](permissions.md), and persistence details in
[Persistent state](storage.md).

The CLAT desktop client provides a self-contained macOS/Windows installer, shares the
existing Host and data, and updates independently of the CLI. See
[desktop installation, background behavior and updates](desktop.md).

## Choose an interface

`clat standalone` (also `clat --standalone`) explicitly opens the traditional
in-process TUI without starting a background host. It retains the normal trust
prompt and exclusive storage lease; it cannot run alongside a host using that
same storage root.

Running `clat` without arguments now discovers or starts a background host, then
opens the traditional TUI as its client. An untrusted project requires an explicit
`y` confirmation before startup; declining writes nothing. Closing the TUI leaves
the host running. Scripts should use `clat exec`; default startup requires a TTY.
The attached TUI title uses `CLAT ● host` in green while connected and
`CLAT ○ host` in red while connecting or offline. Standalone remains bare `CLAT`;
the DSH client retains its corresponding `CLAT ●/○ dsh` marker.
`clat host start [--trust]` performs the same spawn-or-attach without opening a TUI.
The explicit `--trust` authorizes the current project; omit it for already trusted
projects. The host alone persists this authorization after storage preflight.

`clat host status [--port <n>]` checks an existing local host. Without `--port`,
it discovers the port from the storage root's private `host-endpoint.json` file;
an explicit port bypasses discovery.
`clat host stop [--port <n>]` explicitly requests graceful shutdown of that host,
including all its projects; every connected frontend will disconnect. Stop reports
acceptance, not completed shutdown. These commands authenticate using the current
storage root's persistent web token and verify protocol/storage identity. They
never spawn a host or retry a failed stop. Hosts started with a temporary `--token`
need matching credentials; these commands do not accept tokens on the command line.
Exiting an attached TUI with `/quit` remains client-only and does not stop the host.

Discovery also verifies the recorded instance ID against the authenticated host.
The file contains only a port and instance ID, never credentials. It may remain
after shutdown or a crash; a stale, missing or invalid record fails closed, without
falling back to another port. A newly started `serve` replaces it while holding the
exclusive storage lease. At initial startup only, a free root lease permits one
background launch on the stable `127.0.0.1:2691` address; concurrent launchers
converge on the winning host. If another process already owns port 2691, startup
fails loudly instead of hopping to a random port. Explicit `clat serve --port 0`
or another custom port remains available for tests and intentional multi-instance
use. An occupied lease never permits another writer. Startup failure reports an
error, never falls back to standalone; reconnect does not spawn.
`clat serve --trust` explicitly authorizes its current project when needed.
The host snapshots a path-independent build fingerprint and advertises its
human-readable product version before accepting clients. If a later native CLAT
build finds the same live, same-protocol host with a different fingerprint,
startup compares semantic product versions. A strictly newer client asks an idle
old host to yield, waits for the storage-root lease and stable `127.0.0.1:2691`
address to be released, then starts the replacement there; existing browser pages
can reconnect to the same address. Active work refuses automatic takeover and
leaves the old host running. A same-version different build or an older client
also retains the explicit `clat host stop` path instead of replacing the host.
Explicit `host status` and `host stop` ignore only build differences so the old
host remains manageable. Hosts that predate the additive version/fingerprint
fields retain the existing rollout-compatibility behavior.

| Need | Command | State owner |
|---|---|---|
| Interactive work in the current repository | `clat` | background CLAT host |
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
`clat serve` for the same local installation. In the model picker and title
bar, `⧉` means **accepts images**; the `✓` immediately before an entry's
name marks the currently configured item.

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

`write_file` creates new files by default. If the path already exists, it
returns an error and preserves its contents. Use `edit_file` to append or
change a section; intentional whole-file replacement requires
`overwrite: true` together with the complete replacement content. Creation
uses atomic no-replace publication; filesystems without hard-link support
return an error rather than replacing an existing path.

`edit_file` accepts either `old_str` / `new_str` or an `edits` array of
1–64 such pairs for one file. Every pair matches the **original snapshot**
exactly once (including overlapping occurrences); replacement ranges may not
overlap. All pairs are validated before a single snapshot-checked atomic
write. A missing/ambiguous pair, overlap, cancellation or size error preserves
the file. Empty/no-op pairs, mixed forms and unknown keys are rejected;
combined old/new text is limited to 2 MiB and the resulting file to 1 MiB.
For example:

```json
{"path":"src/config.rs","edits":[{"old_str":"first = 1","new_str":"first = 2"},{"old_str":"second = 3","new_str":"second = 4"}]}
```

A match failure may include `edit_recovery`: `applied: false`, a zero-based
`edit_index`, a stable `not_found` / `ambiguous` code, and up to three candidate
`read_file` windows (`start_line` / `end_line`). Not-found hints use a short
prefix and may be irrelevant or empty; they never authorize fuzzy replacement.
The locator examines at most 32 occurrences and reports omitted candidates.
Hints contain no file snippets, make no extra reads, and are only produced
after an authorized read. Re-read the same file, include unique context and
preserve exact bytes, including CRLF; the hints do not normalize line endings.
Argument rejection can include `argument_recovery`. Invalid read ranges report
`operation_started: false` and the supplied numeric bounds, so the agent can
check the intended range without automatically swapping bounds or widening a
read. Invalid edit forms report `applied: false`, up to eight structural issues
with zero-based `edit_index` (null for the request), and `issues_omitted`.
Allowed field names are supplied; unexpected keys, old/new text and file
contents are not echoed. Correct the whole rejected batch before resubmitting:
no entry was applied, ignored, or automatically retried. Permission denials,
I/O failures and cancellation do not acquire these argument-validation hints.
Permissions and path fences apply to single and batch edits alike.

The native `export_patch` tool freezes all current workspace changes against
Git HEAD, including non-ignored untracked files, into a standard binary-safe
Git patch and an adjacent `.manifest.json` containing source and patch SHA256
hashes. For example, `{"path":".tmp/delivery.patch"}` requires both output
paths to be new and Git ignored. It runs at the repository root, does not stage
or commit, and includes pre-existing edits rather than attributing them to the
Agent. The patch limit is 1 MiB, with at most 128 changed files, 1 MiB per source
and 8 MiB aggregate sources. Symlinks, submodules, unresolved merges, byte-changing
Git encoding/EOL conversions, limits and observed source drift are refused.
The manifest is written last: errors or cancellation can leave an incomplete
patch without a completion manifest; choose fresh names for a retry. Export
artifacts are separate from the code-edit FileReview ledger. Hashes let a
reviewer check reconstruction; before/after checks do not lock the repository.

For non-interactive setup, `clat exec --trust "..."` accepts trust for the
current project. `clat serve --trust` explicitly authorizes its current project
after storage preflight. Without `--trust`, serve requires existing trust;
open `clat` once or run `clat exec --trust` first.

## Upgrade

An installed binary can check or install the newest GitHub release:

```bash
clat upgrade --check   # report availability without changing the binary
clat upgrade           # verify and install the newest release
```

The command prints live phase lines for checking, version discovery, downloading,
signature/checksum verification, extraction (archives only), and installation.
For example (illustrative versions):

```text
⟳ Checking for upgrade... (current version 2.1.0)
✓ Found new version v2.2.0.
⟳ Downloading upgrade...
⟳ Verifying signature and checksum...
⟳ Extracting files...
✓ Verification successful.
⟳ Installing upgrade...
✓ Upgrade successful! CLAT v2.2.0 is installed.
Restart running CLAT processes to use the new version.
```

`--check` stops after discovery and prints the install command. An up-to-date
installation gets its own confirmation. Failure prints an error and no upgrade
success line. Output uses ordinary newline-separated text, so redirected logs
contain no spinner control codes. This command updates the CLI executable;
Desktop retains its independent updater.

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
clat plugin pack ./plugin --output ./plugin.clatpkg
clat plugin update ./plugin-v2
clat plugin rollback dev.example.plugin
clat plugin disable dev.example.plugin
clat plugin uninstall dev.example.plugin
```

Package mutation fails with a busy diagnostic while another CLAT process owns
the storage root. Restart CLAT after a successful activation change; mounted
project scopes intentionally keep their frozen tool/prompt surface until
restart. See [CLAT plugins](plugins.md) for transactions, signatures and trust
labels. The signed market command family is `clat plugin market` with
`list`, `search`, `info`, `audit`, `install` and `update`; see
[signed remote market](plugins.md#signed-remote-market) for examples and options.

## Terminal UI

Running `clat` without a subcommand opens the TUI. The previous session is
restored in the background; input stays disabled until replay finishes, then
the conversation becomes interactive.

The TUI materializes the newest 50 messages first for both local sessions and
`clat dsh`. When you scroll to within roughly one screen of the loaded top, it
loads one earlier message-aligned page and keeps the current reading line in
place. A slim line above the transcript shows when earlier messages remain or
are being loaded.

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
| `Ctrl+Q` | explicitly queue text for the next ordinary turn while a CLAT run is active |
| `Alt+Q` | recall the newest next-turn item into the composer, preserving existing text |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | insert a line break |
| `←` / `→`, `Home`, `End` | move the input cursor |
| `Backspace` / `Delete` | edit input |
| `↑` / `↓` | recall input history; if none, scroll the conversation |
| `PageUp` / `PageDown` | scroll the conversation |
| `Shift+Tab` | cycle the active vendor's reasoning level |
| `Ctrl+R` | expand or collapse Think details in the conversation |
| `Ctrl+O` | cycle tool cards through collapsed, expanded and hidden states |
| `Ctrl+F` | find original user/agent body text in the loaded conversation window; query limit 256 characters |
| `Ctrl+G` (also `Alt+S`) | request one manual prompt suggestion when the utility policy allows it |
| mouse wheel | scroll the conversation |
| mouse drag | select text and copy it with OSC 52 on release |
| `Cmd+C` / `Ctrl+Shift+C` | copy the current selection again |
| `Cmd+X` / `Ctrl+Shift+X` | cut the input selection |
| `Esc` | recall the newest queued steering; otherwise cancel the active run; when idle, clear input |
| `Ctrl+C` | copy an active selection; otherwise quit |

Input history is the **current session's committed prompts** in both standalone
and attached (default host) mode. In host mode an accepted `prompt.send`
submission joins history at its admission receipt, and every
`workbench.info` snapshot replaces the list with the session's journal-derived
`recent_inputs` (bounded to the most recent 50, oldest first, including prompts
committed from other clients such as the web workbench). Queued steering joins
only after the host's durable claim; slash commands, UI drafts, and rejected
submissions never enter history. Browsing preserves the untouched draft:
the first `↑` stashes it and the `↓` past the newest entry restores it exactly,
including CJK and line breaks. Session switching or `/resume` reloads the list
from the newly selected session only. While browsing, duplicate snapshots keep
the cursor in place; genuinely newer commits migrate the cursor by its recalled
value, and the stashed draft remains recoverable past the newest entry.

Reasoning is shown inline in both local and `clat dsh` conversations. While a
model is thinking, the collapsed `Think` row follows its latest non-empty line;
after settlement it keeps the first line as the summary. Press `Ctrl+R` to
expand or collapse the complete reasoning text.

Pasting multi-paragraph text into the Message composer preserves the paragraph
structure. Terminal bracketed paste and the explicit `Ctrl+V` text path both
normalize line breaks the terminal delivers as `CR` or `CRLF` (plus Unicode
line/paragraph separators) to ordinary line breaks, so pasted paragraphs and
blank lines stay separate rows in the draft and in the submitted message. A
paste never submits: embedded newlines edit the draft only — press `Enter`
yourself to send.

When a run ends, the transcript explains the stop in place: `completed`,
`cancelled`, or — for a failed run — `error: <reason>` with the host's real
failure detail (bounded to one line; when an older host sends no reason, an
explicit no-reason marker is shown instead). The same text survives session
switches, reconnection, and journal replay on every client, so a failure can
still be diagnosed after the transient status line expires. Credential-shaped
values inside failure text (`Bearer …` tokens, `sk-…` keys, `api_key=…`
assignments) are masked as `[REDACTED]` on every surface before display —
status codes, URLs, and messages stay diagnosable.

While a permission dialog is open, `Esc` denies that tool call instead of
cancelling the run. Hold `Shift` while dragging if you want the terminal's own
selection behavior instead of CLAT's mouse handling.

Typing `/` opens commands and a separate Skills section from the current host's
read-only catalog. Narrow the name or description, use ↑/↓, and select with
Tab/Enter. Tab always fills editable input; Enter fills an incomplete choice
without executing it. If the input already exactly matches the selected
available command or its alias (for example `/quit` or `/exit`), Enter submits
that input directly. Skill selection only fills input. Press Enter after
editing to submit, or Esc to dismiss. A trailing space
also dismisses discovery, so `/help ` followed by Enter directly runs `/help`.
Skill rows show their source and `requires-execution` constraint; unavailable
rows explain why. Pasted paths and input-history recall do not open discovery.
In `clat dsh`, `/` stays ordinary input and does not open a candidate panel:
structured discovery is not exposed by that host. Use `/help` for the existing
DSH-mode command list, or type a complete command and press Enter.

In conversation find, Enter/Shift+Enter visits the next/previous match, Alt+L
loads one earlier page, and Esc closes find without changing the composer.
The panel distinguishes the loaded window from all-loaded history, excludes
Think/tool output, and preserves the selected message across prefix loading
and new tokens. Raw Markdown matches that are not visible in rendered text
anchor the message body and appear in the source preview. `/resume` supports
title/ID filtering: press `/`, type or paste a filter, Enter to browse results,
and Esc to clear the filter before closing the picker. Empty results are explicit.

### Slash commands

The core command table is ordered by task flow (conversation → context →
model → safety → extensions → experiments → meta):

| Command | Purpose |
|---|---|
| `/new`, `/clear` | start a fresh, lazily materialized conversation |
| `/resume` | switch to a prior conversation in this project |
| `/update` | shown only in a selected legacy local session: retain the original generation, upgrade supported older formats to the current writable format (V4); disappears after success. A current V4 session with unsupported developer context or preset is read-only and cannot use `/update` |
| `/rename` | replace the current conversation title |
| `/compact` | summarize older context in the background; original history remains on disk |
| `/context` | inspect a one-shot estimated model-context breakdown |
| `/model` | choose a provider preset or manage named custom profiles |
| `/perm`, `/permission` | switch Read Only, Project Write, or Full Access |
| `/plan [on|off]` | toggle durable Plan Mode; explicit on/off is idempotent at an idle boundary |
| `/plugin` | manage installed plugins and browse the signed market in the host TUI; open Plugin Index in the PWA |
| `/mcp` | inspect MCP/WASM connection state, tools, and isolated failures |
| `/skill`, `/skills` | list cataloged skills, or arm one for your next message |
| `/mem`, `/memory ...` | open a read-only memory dialog; add/edit/delete confirms in the status bar |
| `/goal ...` | open the current goal dialog, or explicitly control a bounded goal |
| `/sub`, `/subagents on|off` | open the subagent status dialog, or enable/disable the experiment |
| `/vision-probe` | run the one-shot vision probe against the current custom model |
| `/help` | open the command and key reference |
| `/quit`, `/exit` | close CLAT cleanly |

Renamed primaries keep their old inputs working: `/memory` and `/subagents`
still dispatch unchanged.

`/vision-probe` (VP-1) is the only capability probe CLAT runs, and it never
runs on its own: built-in presets carry officially-declared capabilities that
are hardcoded (no probing), so the command applies to custom model
configurations only. It is deliberately not listed in the in-app `/help`
(internal one-shot tool); this manual is its documentation. It generates a
small image containing a random four-digit
code, sends it with the current endpoint, model, and key, and grades the
answer. A pass writes the vision capability override into that custom
configuration (persisted; the fail-closed default is unchanged for everything
else) and appends a JSON evidence line to `vision-probe-log.jsonl` under the
storage root. A 4xx rejection and a 200 answer that cannot read the code are
reported separately; network, authentication, and server failures are
inconclusive — rerun the probe. `clat exec --command /vision-probe` joins the
probe and prints the verdict; serve clients receive it as a `vision_probe`
notice event.

These image-draft commands are TUI-local (they edit the terminal composer
without touching a session or model) and appear in the TUI help under
"Composer" (the `Ctrl+V` chord is listed under "Keys"):

| Command | Purpose |
|---|---|
| `/attach PATH...` | add project-relative or absolute image paths; quotes group paths with spaces |
| `@ PATH...` | keyboard-short alias for `/attach` (the required space avoids consuming `@mentions`) |
| `/pi, /paste-image` | explicitly read one image from the system clipboard on a bounded worker |
| `Ctrl+V` | paste a clipboard image through `/pi`, or insert clipboard text |
| `/image remove ID` | remove a stable `[Image #ID]` from the current draft |
| `/image move ID POSITION` | move that stable image to a one-based position |
| `/ac, /attach-clear` | clear the current structured image draft |

The retired `/attachments` spelling (formerly `/attachments clear`) answers
with a one-line migration hint pointing at `/ac` instead of dispatching or
being sent as a message.

The core command catalog is shared with `clat exec --command`. Commands that
require an interactive picker, such as `/model`, `/resume`, and `/perm`,
report a usage error in headless mode instead of inventing a selection.

### Manage plugins inside the TUI

Run `/plugin` in the default host-attached TUI. The five views use the shared
yellow dialog border, muted navigation hints and content-driven height. The window uses the same host
control plane as the PWA and shows installed versions, enabled state, runtime,
publisher and package health. Tab switches between installed packages and the
signed market; arrows select, `/` edits the search filter, Enter finishes search,
R refreshes, and V switches market manifest generation v1/v2. Browse and search
use the host's verified index; a usable signed cache remains available offline.
An unavailable or expired index reports an error while local management remains
available.

Enter or I installs the selected market package. In the installed list, E enables,
U updates, C configures, B rolls back, D disables and X uninstalls. Installation,
update, enable, configure and rollback first open a verified permission review.
Read all pages with arrows/PgDown, press Space to explicitly approve, then Enter
to activate. Resizing resets review progress and consent. Disable and uninstall
require Y confirmation. Active runs or compaction can block a change; finish them
and refresh rather than retrying blindly.

During install, update or configure review, C opens a masked JSON editor. Paste
an object keyed by package ID, for example `{"dev.example.plugin":{"key":"..."}}`.
All entered values stay hidden. Enter keeps the draft, Shift+Enter adds a newline
on supporting terminals, Ctrl+U clears it, and Esc discards it. A blank update
retains existing configuration; Configure requires the package's complete object
(use `{"dev.example.plugin":{}}` to clear its fields). Existing values are never
read back. Drafts do not enter conversation input history and are cleared on
submission, cancellation, expiry, disconnection or closing the window.

Esc cancels review or confirmation and returns to the installed list; Esc in a
list closes the window. Reviews expire after 15 minutes. Late responses are
cancelled rather than attached to a reopened window. A lost activation reply is
not retried automatically: refresh the authoritative installed list and prepare
again if needed. Changes apply to all projects mounted in the host without a
restart. Runtime warnings direct you to `/mcp` for connection diagnostics.
Standalone shows a notice to use default `clat` or `clat attach` for shared
plugin management. DSH-attached terminals retain the DSH host command surface;
use the default CLAT host TUI to manage CLAT packages.

### Plan Mode, skills, LSP, and context inspection

`/plan` toggles durable Plan Mode on or off; `/plan on` and `/plan off` set the
state explicitly. Switching is refused while a run or compaction is active.
When Plan Mode is active, the next run receives a planning policy and a
filtered tool catalog: only `Pure`/`Read` tools plus `exit_plan_mode` remain
model-callable. The same frozen policy guards the model schema, durable
`request/header`, direct dispatch, permission construction, and plugin-host
tool calls. When the model submits `exit_plan_mode`, approval saves the plan and
ends that run; implementation tools return only on the next user run. `/plan
off` leaves the mode at an idle boundary. Plan Mode is an agent authority guard,
not an operating-system sandbox.

The TUI composer title shows **Plan** in amber (`#f0bd5a`) before the permission
label. An armed goal shows **Goal** in green (`#5cf07d`) in the same location;
paused or otherwise disarmed goals have no marker. These markers reflect core
state and are absent in the DSH frontend. Full Access keeps its existing warning
yellow.

Active Plan Mode and an armed Goal are mutually exclusive: `/goal run` and
`/goal create --run` require leaving Plan Mode first; entering Plan Mode requires
disarming the goal, for example with `/goal pause`. Neither operation silently
switches the other mode. An **approved** plan can coexist with a Goal: after
approval, `/goal run` confirms that the approved plan will be injected into the
goal round. `/goal resume` restores eligibility but does not arm continuation.

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

`/skill` (alias `/skills`) works on the same catalog from the human side:

- `/skill` lists every cataloged skill with its source layer
  (bundled/user/project) and `requires-execution` marking, plus discovery
  diagnostics. The TUI shows it as a dialog; `clat exec --command /skill`
  prints the same facts as text.
- `/skill <name>` arms that skill for your next message: the next run's
  system instructions carry the skill body verbatim, witnessed by
  `request/header.invokedSkill`, and the arm is consumed by exactly that one
  run. Session switching and `/new` clear an armed skill; restart does not
  restore it. This is prompt-side guidance only — it grants no new
  authority, and `requires-execution` skills still run their scripts solely
  through the ordinary sandboxed Execute path.
- An unknown name fails with the catalog's candidate list.

Five skills ship compiled-in: `code-review`, `bug-diagnosis`,
`change-verification`, `docs-sync`, and `grill-me` (interrogate a plan before
implementation begins). User and project layers can override any of them by
name.

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
policy, skill catalog, an armed `/skill` invocation (zero when none is armed),
tool schemas, history/compaction view, output reserve,
and total, plus skill discovery diagnostics. For multimodal history it also
reports the image count, normalized image bytes, visual-token estimate, and the
explicit safety factor. Typed tool-result images are counted in provider
projection order, not only images attached to user messages. Visual tokens are
a conservative local estimate included in the history total, not a claim about
exact provider billing. The numbers come from the same estimator used by
model-request budgeting and compaction; the command does not call a model,
write a session event, or start a live monitor.

### Memory, goals, and read-only subagents

Memory is explicit local knowledge, not an automatic transcript extractor.
Only the user-facing command/Application control plane can write it
(`/mem` is the primary name; `/memory` still dispatches):

```text
/mem list [all|project|user]
/mem show <id>
/mem add <project|user> <content> [--source file:path]
/mem edit <id> <revision> <content>
/mem delete <id> <revision>
```

`/mem` (or `list`) and `show <id>` open a read-only, scrollable TUI dialog.
Lists show up to 64 entries with 1 KiB UTF-8 content previews and explicit
omission markers; `show <id>` displays the full record. `/goal` (or `show`) and
`/sub` (or `status`) use the same dialog family. Use arrow keys or PgUp/PgDn to
scroll and Esc to close. Operation confirmations keep the four-second status
flash. With `clat exec --command`, these content queries print to stdout;
the PWA presents them as multiline text notices.

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

`/goal clear` is an alias of `/goal cancel`: both clear and disarm the current
goal. `--run` and `/goal run` are the only operations that arm continuation. Restart,
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

`/sub on` (alias `/subagents on`) exposes `delegate_readonly` for the current
session and process;
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

Cancellation is acknowledged immediately and actively propagated. The status
line switches to `Cancelling…` on the keypress in both the standalone and
attached TUI, without waiting for the host round-trip, and the provider
layer races the cancel token against the blocking connect and
response-header wait — cancelling during a slow first byte stops the run
instead of waiting for the endpoint to respond (bounded by the request
timeouts). Child processes of a cancelled run are terminated with the
process group. A request whose cancel was already set is never sent, and a
cancelled run schedules no further model or tool steps.

The agent loop has no fixed turn count. A run ends when the model completes or
refuses, the user cancels, a failure occurs, or the token spend budget **you
set** is exhausted. There is no default cumulative budget: if you never set
one, CLAT never stops a long run on a local token cap (the former silent
10M default was removed on 2026-10-15). Set one in `/model` if you want a
hard stop; `0` disables an existing guardrail. Warnings are journaled at 50%
and 90% — and only for budgets you set. A run without a local cap is still
bounded by context compaction, per-request response limits, and your real
provider quota.

Context pressure is handled separately. Presets seed a context window, and
automatic compaction checks run both at run/Goal-round startup and before every
model request inside a long task, near the shared 80% request-pressure line
(including instructions, visible tools, and output reserve). A single user turn
can therefore continue through many model/tool exchanges without waiting for a
new user message to reclaim context. Without a configured or preset context
window, CLAT does not invent an automatic compaction threshold.

In-run compaction keeps genuine user messages and steering inputs verbatim,
retains the newest complete tool exchange, and summarizes older complete
exchanges without splitting a model response's tool-call/result pairs. Earlier
checkpoints made during that run can be summarized again. Summary input is
split into bounded, lossless text chunks rather than taking just the first
2,000 characters of each item. The handoff asks for decisions, observed results,
evidence references, unresolved issues, and next steps. Summarization remains
lossy; original observations are available in the journal. Images in summarized
exchanges are represented by image notices, not re-sent to the summary model.

Internal summary attempts share the run's cancellation signal and cumulative
token budget, including retries. Missing provider usage is charged using the
conservative request estimate and output reserve. Each request boundary allows
at most four shrinking checkpoint passes; each pass retains the existing
60-second deadline and eight-attempt summary limit. An exchange too large for
that bounded summary work is left intact. If no safe reduction fits the frozen
request budget, CLAT stops before the next provider request with an explanation.
A summary failure leaves context intact; a persistence failure stops execution
without repeating tools already executed.

`/compact` still manually summarizes older user turns while no run is active.
Compaction appends summary and replacement events after durable admission; it
keeps the original journal events and human-readable transcript. Untouched live
tool results and image blocks retain their existing model projection.

### Attached terminal information dialogs

In the native host-attached TUI, bare `/help`, `/skill` (`/skills`), `/mem`
(`/memory`), `/goal`, and `/sub` (`/subagents`) open read-only, scrollable
dialogs populated by the host. Use Up/Down or PageUp/PageDown to scroll;
Esc or Enter closes the dialog. The composer draft is preserved. Commands
with arguments retain their existing command behavior.

`/help` combines the host's authoritative command catalog with the terminal's
local Composer and Keys sections, so attachment commands and keyboard shortcuts
remain available after the default TUI attaches to the background host.

Bare `/context` opens the host's one-shot context estimate, including prompt
layers, history, image projection and visual tokens, memory budget, tools,
skills, and diagnostics. The terminal does not recalculate these values.
Close and reopen for a new snapshot; malformed responses show an error rather
than silently substituting zero estimates.

Bare `/mcp` uses the same dialog for host MCP/WASM connection status. Press
`r` or `R` to read the latest status and reset scrolling; this does not restart
or reconnect any MCP server. A late reply from an older refresh is discarded.

Closing a dialog, opening another, switching sessions, or reconnecting prevents
late responses from replacing its contents. Errors remain in the dialog; close
and reopen to retry. When offline, close the dialog and use `/reconnect` first.
These views do not mount a local runtime or read session storage directly.

### Image attachments

Pasting or dragging exactly one existing absolute image path turns it into a
structured draft entry. `~` is accepted; bare relative names and mixed text are
not auto-detected. For explicit selection, `/attach` (or `@ `) accepts
project-relative and absolute paths, quoted paths with spaces, and multiple
paths in one command. PNG and JPEG sources may be up to 8 MiB each; one message
is capped at 8 images, 32 MiB of source bytes, and 16 MiB after normalization.

Each image has a stable `[Image #N]` label independent of editable message text.
The rail shows its filename, measured dimensions, source size, estimated visual
tokens, and draft totals. `/image remove` and `/image move` operate on that
stable id. Text-only model routes keep the draft visible but block sending;
switch to a vision-capable route or remove the images. Vision capability is
hardcoded per preset, never probed at runtime: six presets across four
vendors open image input — officially-declared for `deepseek-flash`,
`qwen3.8-max`, `qwen3.8-flash`, `kimi-k3`, and `kimi-for-coding`, probe-verified for
`glm-5.3-flash` — and, for custom configurations, it is unlocked only by a
passing `/vision-probe`.
Switching an image-bearing session to a text-only route fails the next run
before provider I/O with the same actionable guidance (switch back to a vision
route or start `/new`) instead of forwarding the vendor's 400 — except images
the visual-context budget already offloaded to notices, which do not block the
text-only run. Image-only prompts
are valid. Startup/admission failures restore the text and ordered draft, and
`Esc` recalls queued image steering with its original sources. If cancellation
or failure seals the run before a queued steering message is claimed, its exact
text and ordered images return to the composer; multiple recovered messages
surface one at a time so their individual image budgets are preserved.
Successful session switches and `/new` clear unsent images so drafts never
cross sessions.
For an initial image-bearing message, source reads, full decode, normalization,
and durable admission run on a bounded background worker. The terminal remains
repaintable while input is briefly gated; the first run event cannot pass its
handoff barrier until the application and run state are back on the frontend.
Text-only startup keeps its existing fast path.

`/pi` (alias `/paste-image`) and the TUI's explicit `Ctrl+V` chord are the only
system clipboard probes: ordinary bracketed paste never probes the system
clipboard. `Ctrl+V` uses the same image path as `/pi`, falling back to ordinary
composer insertion when the clipboard contains text and showing a status-line
message when it is empty or unreadable. CLAT reads RGBA dimensions and enforces
the 16M-pixel/byte-layout guard immediately, encodes PNG on a single bounded
worker, and places the result in core-owned process-local staging before the TUI
receives a path. Clipboard staging is capped at 128 MiB in aggregate and swept
after one hour. Removing or clearing a draft, completing initial-message
admission, or durably claiming an image steering message reclaims its raw staged
PNG; CLAT never deletes a user-selected `/attach` source. macOS uses the native
pasteboard, Windows uses the Win32 clipboard, and Linux supports X11 plus
compatible Wayland data-control compositors. WSLg can use those Linux paths;
WSL without a graphical clipboard, Android, and Termux report an actionable
unsupported error. CLAT deliberately does not concatenate or run a
PowerShell/shell fallback.

On send (including steering during an active run), CLAT normalizes the image
into the content-addressed attachment store before journaling the turn. The
journal stores an opaque attachment reference and metadata rather than image
bytes or a host path. Only routes with a
probe-verified image capability receive native image input; unreadable or
route-rejected parts degrade to a path-free notice.

On a route whose frozen capabilities also permit image tool results, the agent
receives the read-only `view_image` tool. It accepts one of an attachment ID
already reachable from the active session, a project-relative PNG/JPEG path,
or a core-minted current-run scratch reference. It never accepts an absolute
path, including in Full Access, and read-only subagents do not receive it. A
successful call sends the admitted image to the configured model provider,
then journals only the descriptor so the result remains replayable after a
restart.

In the local web workbench, use **Add images**, paste an image into the
composer, or drop PNG/JPEG files onto it. The rail supports multiple images,
ordering, removal, image-only messages, and retry after an upload failure.
Browser files first enter a short-lived server-minted draft scope; the browser
sends only opaque upload IDs, never a local path or Base64 body. A successful
send clears the browser draft; switching or creating a session clears and
revokes it as well. While a run is active, an image draft is queued as steering
and becomes durable only when that run claims it.

History thumbnails are fetched on demand through the authenticated local
attachment endpoint and displayed with temporary `blob:` URLs. Attachment IDs
must be reachable from the active session; the endpoint never reveals a disk
path, and the pairing token is never placed in an image URL. The response is
fixed-length and streams from a verified no-follow file handle in 64KiB chunks;
at most four attachment downloads may occupy the slow-reader budget at once.
Each write may idle for at most 15 seconds and the full response is capped at
60 seconds.

### Rendering, copy, and notifications

Assistant Markdown supports fenced code, inline code, emphasis, headings,
lists, blockquotes, links, and horizontal rules. Unsupported syntax degrades
to plain text; model output is never interpreted as terminal control input.
Tool cards stay visually secondary: their status heading and successful output
text are dim, only the successful checkmark is green, and failures remain red.

CLAT plays a focus-aware notification when an unattended run finishes or a
permission/question dialog needs attention. This covers standalone, DSH, and
host-attached sessions alike: a run settling on the background host — even one
driven from another client such as the web workbench — rings once in an
unfocused terminal, while runs you cancel yourself stay silent. Set
`CLAT_NO_BELL=1` to disable it, or `CLAT_BELL_COMMAND` to a detached shell
command that should provide the sound. When native system audio is unavailable,
CLAT falls back to the terminal bell.

### Model and usage status

The header shows the active model and reasoning level. `Shift+Tab` changes the
level for the next run and persists the effective model configuration:

- DeepSeek, GLM, and Kimi use CLAT's Low / High / Max ladder.
- Qwen maps Low / High / Max to its `low` / `medium` / `xhigh` values.
- Tencent Hy always thinks server-side; the endpoint ignores `reasoning_effort`
  (probe-verified), so no ladder is offered and `Shift+Tab` has no effect —
  the title bar shows `Thinking · Server` instead of a level.
- Custom profiles on a known vendor domain use that vendor's mapping.
- Unknown endpoints do not receive an inferred reasoning parameter; configure
  it explicitly in Extra Body.

Cache and context values are session facts restored from journal usage events.
Where supported, DeepSeek shows wallet balance and GLM/Kimi show remaining
plan quota. Missing provider data is shown as unknown rather than fabricated.
The host-attached TUI receives these values from the host, including when
attaching to existing history; it does not run a second quota monitor.
`workbench.info` exposes this as the additive `model.vendor` and `telemetry`
fields (`monitor`, current-route `route_usage`, and `last_request_usage`).
These contain display data only, never endpoint credentials.

### Run terminal states and unfinished work

A run's real outcome stays on the status bar after the run ends — `completed ·
N model turns`, `cancelled · …`, or `run failed after …: reason` — instead of
silently reverting to the project directory when a transient hint expires.
The next run clears it when it starts, so an active run is never mislabeled as
finished or failed by a leftover status. The transcript keeps the same
outcome line (`completed`, `cancelled`, or `error: …`), and a tool failure
(`✗ Tool / …`) remains a distinct card: a failed tool call does not fail the
run, and the model may continue after it.

`RunCompleted` means the run terminated, not that your task was verified
complete. When the session has observable unfinished work — todos that are
still `pending` or `in_progress` — the transcript adds an honest accounting
line (`run completed — 2 of 4 todos unfinished · task completion not
verified: …`) and the persistent status names the unfinished count. With no
observable unfinished items CLAT does not invent a completion score;
continuation is always your decision: send the next message to continue in
the same session, and the next run sees the same journal context without
re-running completed work.

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
stdin, a side-effecting tool displays its full arguments and waits for `y` or
`yes` (case-insensitive) + Enter; `Esc` or any other answer denies it. Input
typed before the prompt is discarded. With piped stdin there is nobody to ask, so side effects are denied
and returned to the model as tool errors. `--yes` approves every side effect,
including command execution. On macOS the native command tools still default
to workspace-write Seatbelt confinement; Windows reports partial ACL write
confinement with unrestricted network and reads. Linux has no graduated
provider, so unattended `--yes` should run only inside an
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

For disposable scripts, use the private scratch directory supplied in the model
context. In Project Write (and classic headless execution), native write/edit/
patch tools accept absolute paths inside it. The directory is outside the
workspace, so these files do not appear in workspace changes or FileReview.
Scratch is removed at run completion after child processes are reaped; do not
put results you need to retain there. Read Only approval does not open scratch
writes, and Full Access retains its existing unrestricted write behavior.

- `exec_command` starts a command in the project, a project-relative
  workdir, or an absolute directory inside the active run scratch root. It waits up to `yield_time_ms`; a still-running command returns a
  numeric `session_id`. Command text is capped at 64 KiB.
  Specify exactly one of `cmd` (shell text) or `argv` (an executable followed
  by literal arguments). `argv` accepts 1–128 strings, at most 64 KiB of UTF-8
  bytes in total, with no NUL; empty arguments after the program are preserved.
  Shell operators in an argument are literal text. Windows batch files require
  explicit shell execution through `cmd`.
  Non-PTY commands default to closed stdin (immediate EOF), so batch tests
  cannot wait on an unused input pipe. Set `"stdin":"pipe"` when input will
  later be sent through `write_stdin`. PTYs retain terminal input by default;
  `stdin: "closed"` requires `tty: false`.
- `write_stdin` writes characters, closes stdin, polls for output, or
  terminates that same-run id. An empty `chars` value is a poll. Raw characters
  are redacted from the durable tool-call journal; the model tool rejects
  `sensitive: true` rather than pretending to protect a secret already sent to
  the model. One write is capped at 256 KiB; five seconds of write backpressure
  terminates the session instead of starving cancellation/TTL teardown.
- `run_command` is the one-shot API over the same ProcessService. Supply
  exactly one of `command` (legacy shell text) or `argv` (literal executable and
  arguments). `workdir` defaults to the project root and accepts project-relative
  directories or the active scratch directory. Direct `argv` also accepts the
  same `verification` specification as `exec_command`; it waits until completion
  or timeout with stdin closed, without requiring a polling session. The result
  reports the resolved `cwd`, and `execution` / `exit_scope` distinguish a shell
  exit from a direct child's exit. No shell pipeline is rewritten automatically.

Known pre-execution command admission errors can include `invocation_recovery`.
An absolute workdir outside active scratch reports the bound project root and
the project-relative alternative; a conflicting shell/argv request reports how
to choose one form while retaining literal verification. A verification request in shell or PTY form also reports the required literal
`argv` / `tty=false` form, retaining the verification requirement instead of
silently dropping it. These rejected calls start no command. Recovery is guidance for another explicitly authorized call,
never automatic execution, a widened write root, a silent target change or a
fallback to a shell. Permission denials and runtime failures do not acquire this
validation-only label.

- `terminal_snapshot` reads current text cells of a current-run PTY created with
  `exec_command` and `tty: true`. Supply its `session_id`, optional `start_row`
  (default 0) and `row_count` (default 12, maximum 24); follow `next_row` when
  `has_more_rows` is true. The VT projection folds cursor, erase and alternate
  screen updates from capture start, without consuming raw output, writing input
  or extending process lifetime. Check `projection_complete` and
  `text_truncated`: capture stops after 4 MiB or an unsupported screen size
  (maximum 100 rows × 240 columns), and incomplete projections return no text.
  PTY handle/lifecycle metadata survives shared result pruning even when raw
  diagnostics require shortening. Each response contains at most 3000 text
  characters. This is a bounded VT text
  projection, with no scrollback, font/IME/image emulation or terminal query
  responses; long combining sequences may be shortened by its cell model.
  `projection_complete` describes capture/size bounds, not exact emulation.
  It is not proof of application or persistent state and cannot
  inspect a nested PTY hidden inside a non-PTY script. Screen text is omitted
  from the durable journal. Windows PTY execution remains unavailable.
- `verify_observation` evaluates explicit checks against a project-relative JSON
  observation file (maximum 256 KiB, regular files only, no symlink traversal).
  It returns the SHA-256 of the bytes read and per-check `passed`, `failed` or
  `inconclusive`, without returning the artifact data. Supply 1–16 `checks`, each
  with a unique ASCII `id`, an `op`, and a JSON `pointer`. `is_true` requires an
  observed boolean; `equals` compares an explicit `expected` JSON value, useful
  for a complete sequence of intermediate observations; `all_before` requires
  every `before` label to precede every `after` label in a string trace, with
  both groups present. Missing pointers, wrong types and missing order groups
  remain inconclusive. All checks must pass for overall success. Contracts
  (including expected literals) are hashed and omitted from the journal.
  This Read tool does not authenticate observers, freshness, source identity,
  coverage or the correctness of the declared contract. Use independent truth,
  normal controls and meaningful counterexamples; matching two wrong results
  or checking only a final value does not prove the intermediate behavior.
  Both new Read tools use the shared permission/cancellation path, are available
  in Plan Mode, and are outside the fixed subagent tool allowlist.
- `session_info` reads the conversation ID bound to the active run and its
  project root. It uses in-memory ownership, without listing session directories
  or inspecting timestamps, credentials or other conversations. The ID can resume
  the conversation; it is separate from an `exec_command` process ID. There is no
  result outside an active run. This Read tool is available in Plan Mode, but is
  outside the fixed read-only subagent allowlist.
  If a platform path cannot be represented as UTF-8, metadata reports
  `project_root: null` rather than inventing a lossy path or failing serialization;
  project-relative command addressing remains the appropriate interface.

To preserve a first failure before a rerun, `run_command` accepts explicit local
evidence recording in a writable project mode:

```json
{
  "argv": ["target/debug/deps/my-contract-test", "contract::denies_invalid", "--exact"],
  "evidence": {
    "directory": ".tmp/verification",
    "files": [".tmp/last-test.log"],
    "bind_files": ["src/contract.rs", "target/debug/deps/my-contract-test"]
  }
}
```

Each invocation creates a distinct `command-UUID.json` using create-only
publication. The result and durable tool-result metadata retain its relative
path and SHA-256; use `read_file` to inspect it later. A successful rerun never
replaces an earlier failure record. Raw retained stdout/stderr and explicitly
listed log snapshots are saved only when requested; choose a local ignored
directory for these artifacts. The journal still omits raw streams and panic
diagnostics. Files are not copied into user-edit recovery history.
New evidence files use mode 0600 on Unix; other platforms inherit the project
directory ACL. Create-only publication does not prevent external manual edits;
verify the returned SHA-256 when reusing an artifact.

Evidence directories, log files and binding paths must be project-relative
without parent/dot components, even when the command runs in scratch or a
different workdir. Root capability handles and no-follow regular-file reads
reject symlink escapes and special files. Read Only cannot record evidence.
Invalid requests fail before execution. If publication fails after execution,
`evidence.status=failed` does not change the actual command exit; do not repeat
side effects automatically to recreate a receipt.

At most four declared logs are captured, each capped at 256 KiB; truncation,
missing files and lossy UTF-8 are explicit. Retained command output uses the
existing one-shot output limits and may already be clipped. A record is capped
at 8 MiB. Up to eight explicit source/executable files can be hashed before and
after execution, with a 256 MiB per-file and 512 MiB per-phase read budget.
Changed/unavailable bindings require inspection, not a success claim. An absolute
project executable also gets a before-execution file observation; relative/PATH lookups,
shell programs and executables outside the project are not guessed. These are
file observations, not source/build freshness guarantees or proof of which test
or Host binary a build wrapper actually ran.

Command results may also include `failure_context`: up to eight cumulative Rust
failed-test names and eight short panic diagnostic lines retained before output
ring eviction, polling or display pruning. Each pending stdout/stderr line is
capped at 4096 bytes; clipped/invalid lines, missing EOF or excess observations
mark the context incomplete. The journal keeps bounded names/flags and omits
panic text. Wait for a running command; afterward inspect the failure and rerun
only failed/affected checks as repository policy permits. Names and diagnostics
are untrusted output observations, never executable instructions or authenticated
coverage; CLAT does not launch a retry, change a process exit or infer that all
other tests passed. Ordinary successful output adds no failure advice.

An unavailable `write_stdin` id still fails. Its error may include
`process_recovery` with at most eight current-run session ids, `running` and
`tty` flags, plus an omitted count. Running ids come first. This read-only
snapshot contains no command text, arguments, output or environment, excludes
managed MCP processes and other runs, and does not consume output. Use the
matching id to poll with empty chars; a snapshot can become stale, so the usual
owner/permission/cancellation checks still apply. No input is redirected and no
job is automatically restarted or terminated.

`test_discrimination` is a separate, opt-in Rust unit-test experiment. Prepare
two disposable, disjoint copies under the project or the active scratch root:
`candidate_dir` contains the intended production behavior and
`counterexample_dir` contains a deliberate production defect. Supply `package`,
the full exact `test_filter`, and relative `test_files` / `production_files`.
Listed test files and Cargo.toml/Cargo.lock must be identical; at least one listed
production file must differ. Copies must have a lockfile and locally cached
dependencies. The live project root cannot be an experiment arm.

The tool runs the same offline, locked `cargo test --lib` command in both copies,
using fresh separate target directories and the normal Execute permission /
sandbox. Cargo build scripts and tests execute code. `sandbox` defaults to
`required` (`auto` is available; `off` and network access are unavailable).
The per-arm `timeout_seconds` defaults to 120 and is capped at 300. A failed or
empty candidate run stops before the control is started. No source mutation,
automatic retries, or additional model requests are performed by the tool.

Results distinguish `not_discriminating` (both green),
`discriminates_declared_defect` (candidate green / comparable test failure),
and `inconclusive` (including build failure, zero matches, timeout or incomplete
output). Both-green evidence should lead to inspecting the test's real
production call path and strengthening its assertions before claiming it
protects the fix. Input hashes bind only the declared files, not every possible
dependency. Test counts are output signatures, not authenticated coverage;
callers must review the failure reason, completeness of the declared inputs,
and whether the defect models the intended contract. Raw experiment stdout /
stderr is transient and omitted from the durable journal.

Supply optional `expected_failure` with a literal assertion marker derived from
the intended invariant (nonblank, at most 512 UTF-8 bytes). A comparable red
control qualifies only when its retained Rust panic diagnostic contains that
marker. Printing it elsewhere in test stdout is insufficient. A different or
unavailable panic reason yields `inconclusive` and directs the agent to inspect
the actual failure before claiming discrimination. The check runs before the
transient diagnostic excerpt is shortened; only `matched`, `mismatched`, or
`unavailable` is added to the control observation and journal. It performs no
extra executions or model requests. Omission preserves the count-based result
and explicitly leaves the failure reason unverified. A marker match is an
output observation, not proof of source coverage or semantic relevance.

For Playwright experiments, supply `playwright` instead of `package` and
`test_filter`. This extends the same tool and permission path; the default Cargo
experiment is unchanged. Both disposable roots run the same literal `argv`
with stdin closed. Configure Playwright's JSON reporter and zero retries;
include every relevant test/helper in `test_files`, package manifests,
lockfiles and configuration in `build_files`, and production differences in
`production_files`. `build_files` must include a `package.json` (nested paths
are allowed). For example, the additional runner object can be:

```json
{
  "argv": ["node", "node_modules/@playwright/test/cli.js", "test", "contract.spec.js", "--reporter=json", "--retries=0"],
  "build_files": ["package.json", "package-lock.json", "playwright.config.js"],
  "artifact_files": ["bin/product"]
}
```

`expected_failure` is required here. A qualifying control must exit 1, have the
same nonempty test set as the successful candidate, and report ordinary failed
assertions containing this marker in each failed test's JSON error messages.
A marker printed in stdout, setup errors, expected failures, skips, retries,
timeouts, different tests or incomplete reports yield `inconclusive`. Two
ordinary passing reports yield `not_discriminating`. No report can authenticate
that its producer used the intended binary or that a chosen defect is relevant.

Up to four optional `artifact_files` bind explicit source-built executables or
other runtime inputs by SHA-256 before and after each execution. Each artifact
is limited to 256 MiB; ordinary declared files to 1 MiB; each arm's hashing phase
to 512 MiB. Declared inputs must be regular, non-symlink files inside their copy root and
cannot change after
preparation or while either arm runs. Both copies are rechecked after each arm,
including when the candidate fails and the control is skipped. These checks
compare boundary snapshots; they cannot detect a file changed and restored
between checks. These digests are observations, not proof
of build freshness or an inventory of every dependency. The JSON report and
stderr must each fit the one-shot process capture (32 KiB); clipped/lossy data
is inconclusive. Install project dependencies yourself; the tool adds no Node
runtime or dependency installer to CLAT. There is no network opt-in or sandbox-off option. Effective restrictions still
follow the selected permission mode and sandbox facts (Full Access is unconfined). Execute approval still applies to the
supplied program and any code it launches.

The Playwright tool/call journal stores argument hashes/counts instead of raw
argv or assertion markers. Results retain only bounded counts, identities,
artifact digests, lifecycle facts and classification. Up to two transient assertion messages (240 characters each)
help inspect an unrelated failure; these messages are omitted from tool/result
journals. Provider streaming and
other existing session records retain their own policies: never put credentials
in model tool arguments. The tool does not mutate source, rebuild artifacts,
retry tests, or add model requests.

All three tools add a structured `command_outcome` to command results and
native invocation errors. It contains `kind`, `reason`, and `evidence`:

| `kind` | Meaning |
| --- | --- |
| `running` | The command has not reached a terminal status. |
| `success` | The outer shell exited zero (`shell_exit_zero`), or an argv process exited zero (`process_exit_zero`). |
| `program_failure` | An ordinary nonzero exit (`nonzero_exit`), including compiler or assertion failures. |
| `environment_failure` | A supervised timeout, sandbox diagnostic, launch/session limit error, or recognized execution/loading/resource diagnostic. |
| `interrupted` | CLAT observed cancellation or explicit termination. |
| `unknown_failure` | An unexplained signal, ambiguous 126/127 exit, or an unclassified tool error. |

For example, exit 126 with a permission-denied diagnostic yields
`{"kind":"environment_failure","reason":"execution_denied","evidence":"output_signature"}`;
a compiler's exit 101 yields
`{"kind":"program_failure","reason":"nonzero_exit","evidence":"exit_status"}`.
The original exit code, signal, stderr, timeout fields, and error message remain
unchanged. Nonzero command exits keep their existing tool-result semantics;
invocation errors still have `is_error=true`. The labels never change approvals,
sandbox enforcement, retries, or process lifecycle.

`evidence=lifecycle` describes CLAT-observed supervision;
`exit_status` describes the reported outer process status.
`sandbox_diagnostic`, `output_signature`, and `operation_context` are diagnostic
classifications, not proof of an OS cause. Output signatures use the existing
bounded stderr/PTY rings, independently of the current output budget and
consumption cursor; old diagnostics can disappear if the ring overflows.
Bare `SIGKILL` is not labeled OOM, and ambiguous 126/127 exits remain unknown.
Missing executable/format, permission-denied, memory/allocation, disk-space,
and descriptor-limit messages are hints; an application can print those words.
`success` does not prove every command in a shell sequence or pipeline succeeded:
`false; echo ok` still reports the outer shell's zero exit. Check the actual test
output and match count before claiming verification.

Command results also carry `verification_witness`. It reports bounded,
unauthenticated observations from the **currently consumed bounded output bytes**,
before JSON-string escaping can truncate their display, separately
from the outer process status. Rust and Playwright list/line summary formats
are recognized separately. Complete Rust test summaries include `passed`,
`failed`, `ignored`, `filtered_out`, and `matched` (passed + failed); zero matches
are explicit. Cargo compile-failure messages have an unknown match count. Other
formats, incomplete lines and empty polls cannot prove a verification ran.
Every observation carries a stream and format; the envelope states
`evidence=output_signature`, `scope=consumed_output_bytes`, `authenticated=false`, and
`target_exit_code=null`. Programs can print these signatures themselves.

Up to eight observations are retained, including earlier failures before later
successes. Their ordering is per stream, not a merged stdout/stderr timeline.
`observations_truncated` reports an observation cap; `output_incomplete` reports
truncated/lossy bytes, a missing line prefix, or a fragment without its terminating
newline. A retained preceding newline proves the start of a polled line; a lost
prefix cannot. Polling does not replay
previously consumed summaries. The witness does not certify source identity,
assertion coverage or unseen pipeline stages; `status=unknown` remains unknown.
Invocation errors have an empty witness. This metadata never changes permissions
or command execution.

Without a cumulative receipt, a completed zero-exit command whose currently
consumed output contains a failed/compile-failed or zero-match signature also
returns `verification_guidance`. It reports `failure_observed` or `inconclusive`
and recommends inspecting output and, when needed, verifying the actual test
executable with literal `argv` and a receipt. It preserves the real outer exit
and `command_outcome`; `target_exit_code` stays null and `authenticated` false.
It never executes a retry or changes permissions. Normal successful output,
nonzero/interrupted/running results and existing receipts do not receive this
extra guidance. Missing, truncated or previously consumed signatures may be
unavailable; this is not cumulative verification. The bounded guidance survives
model-result pruning and journal/replay while raw streams remain transient.

The durable tool-result journal retains `command_outcome` and the bounded witness
counts and fixed labels while omitting raw
process streams. Older journal results without this field remain readable;
replay preserves recorded labels rather than recomputing them from missing output.

For verification, `exec_command` accepts an optional non-PTY `argv` mode:

```json
{
  "argv": ["cargo", "test", "--offline", "--lib", "contract", "--", "--exact"],
  "verification": {"format": "rust-test-summary", "minimum_executed": 1},
  "yield_time_ms": 1000
}
```

The returned `verification_receipt` is cumulative for that run-owned process,
including subsequent `write_stdin` polls. It reports `running`, `failed`,
`inconclusive`, or `passed`, the direct child's exit/lifecycle facts, an argv/cwd
fingerprint and bounded format-specific summary counts. Poll while running. Nonzero exits,
observed failures and compilation failures cannot pass; interruption, missing
evidence, zero/ignored-only matches or fewer than `minimum_executed` tests remain
inconclusive. The minimum must be 1–1,000,000. A failed test is a normal process
result, so inspect the receipt even when `is_error` is false.

For Playwright list/line reporters, select `"format":"playwright-summary"`.
For example, a one-shot invocation can use `argv: ["node",
"node_modules/@playwright/test/cli.js", "test", "contract"]` with
`workdir: "web/e2e"` and `verification.minimum_executed: 1`. On Windows use the
actual executable/Node entrypoint rather than a `.cmd` wrapper. The receipt
counts passed, failed, skipped/did-not-run (`ignored`) and flaky totals. Rust
summaries printed by a supporting host are not added to browser totals.
Skipped-only output cannot pass; any flaky total remains inconclusive and
requires inspection. These are unauthenticated reporter totals, not a proof of
coverage or that a retry was legitimate. HTML/JSON/dot/custom reporter output is
not inferred as list/line totals. Neither a wrapper's zero exit nor a printed
summary authenticates a hidden child exit. Summary receipts explicitly report
`expected_failure_visibility: "unavailable"`; list/line totals cannot distinguish
Playwright `test.fail` from ordinary successful assertions.

For per-test expectation accounting, use `"format":"playwright-json"` with
Playwright's `--reporter=json` and `tty: false`. The entire stdout must be one
JSON report, with `suites` and `errors`; do not mix a list reporter or arbitrary
stdout into it. The cumulative capture is capped at 512 KiB, 4096 tests and
32 nested suite levels. Malformed, partial or oversized reports cannot pass.
The receipt separates ordinary `passed` tests from `expected_failures`,
`unexpected_passes`, `ignored` and `flaky`/retried tests. An expected failure
with a zero process exit remains **inconclusive**, not a business pass;
unexpected passes and report errors fail. Real process exit, cancellation and
timeout facts still take precedence. These are unauthenticated report contents,
not independent proof of the observer or tested source.

The scanner joins partial lines before parsing, retains at most 64 summary rows
as aggregate counts, and buffers at most 4096 bytes per pending stdout/stderr
line. ANSI SGR color/style sequences are ignored when recognizing signatures;
raw output is unchanged and other terminal controls are not interpreted.
Long or invalid lines, counter overflow, missing EOF and excess summaries
make the scan incomplete. Summary counts remain unauthenticated output
signatures: a program can print them. `exit_scope=direct_child` does not recover
hidden exits inside an explicitly launched wrapper or shell, certify a test
binary/source version, or prove that the assertions cover the intended behavior.

Verification results reserve room for the receipt and shorten diagnostics based
on the actual serialized JSON size, before the shared result pruner runs. The
receipt therefore survives long or escape-heavy output. Display truncation and
scanner completeness are separate facts; a bounded excerpt is not the full log.
Empty polls keep cumulative evidence without rerunning the process. The durable
journal retains receipt metadata while omitting raw streams. There are no
automatic retries, source edits or extra model calls, and the normal Execute,
sandbox, network, cancellation and run-ownership policies still apply. Legacy
`cmd` results keep their existing fragment-scoped witness behavior.

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

In Project Write, native commands bound to macOS Seatbelt or the Windows ACL
runner run without a prompt. Explicit `network: true`, Linux fallback and
third-party Execute tools still ask. `write_stdin` uses the same-run command's
actual sandbox facts. Missing enforcement never grants automatic approval;
provider launch failures fail closed. Read Only and classic `clat exec` approval
behavior are unchanged.

- macOS `auto`/`required` uses a functionally probed Seatbelt profile outside
  Full Access. Network is denied unless explicitly requested. `off` requires
  Full Access. Account-readable files and inherited environment variables stay
  visible; Workspace Write also permits `/tmp` and the process temporary
  directory.
- Windows `auto`/`required` uses the built-in ACL restricted-token runner with
  `provider=windows-acl, enforcement=partial`. Reads and network are unrestricted;
  NTFS hard-link aliases remain a limitation. TMP/TEMP point at private run
  scratch. Workspace ACL grants persist after shutdown; see [Permissions](permissions.md).
- Linux `auto` reports `provider=none, enforcement=none`;
  `required` fails closed. This is lifecycle supervision, not OS confinement.

Process completion also appears as an Application notice in TUI/PWA. It
contains only id and terminal metadata, never the command or raw output.

## Local server and web workbench (`clat serve`)

Start the loopback server from a trusted project:

```bash
clat serve
# listening on http://127.0.0.1:2691/
# your default browser opens directly into the workbench
clat serve --no-open             # print the URL without opening a browser

clat serve --rotate-token
clat serve --port 8099 --token <token>
clat serve --port 0 --token <token>   # OS-assigned test port
```

`--rotate-token` and `--token` are mutually exclusive. `--token` is a
process-only override; it neither reads nor changes the persistent token.

### Companion utility and manual suggestions

The Workbench settings panel exposes a host-wide companion utility policy.
Continuous session naming is on by default; it coalesces duplicate notifications
for the same ended turn and never changes a user-owned title. Generated titles
follow a bounded contract: the model is asked for a short script-aware title
(about 6 words in non-CJK languages, 12 CJK characters), and whatever it
returns is accepted only within a hard cap of **48 display columns** (24 CJK
characters / 48 Latin letters). Compliant titles are stored whole — never cut.
An over-long title is kept only when a provably safe cut exists: a whole-word
or grapheme boundary (emoji ZWJ sequences are never split, and unspaced CJK
runs count as one unbreakable word). Terminal control and bidirectional
spoofing characters are stripped, and when no safe prefix exists — a single
64-character token, or a 25-character unspaced CJK phrase — the previous title
is kept instead of saving a mid-word fragment. The TUI conversation header and
the PWA header/sidebar ellipsize long titles visually within the available
width; what is stored stays whole and manual renames (bounded separately at
60 characters) always take precedence.
Manual prompt suggestions
are off by default. In the PWA, the sparkle button at the upper-right of the
composer is muted and disabled until the policy, session, and idle state allow
a request; when enabled, press it to request one next-message hint. In the TUI,
press **Ctrl+G** (Control, not Command, on macOS) or enter `/suggest`.
This shortcut does not require configuring Option as Meta in the terminal;
**Alt+S** remains a compatible alias. The composer border shows the faint shortcut,
then a generating or ready state; **Ctrl+Y** adopts the preview. The hint is a
preview only: it is never submitted or written to the session journal until you
adopt it and send it as a normal prompt. In the TUI, **Esc** ignores it without
changing your text. Generation does not lock editing, and each terminal allows
only one outstanding suggestion request.
Switching sessions, starting a run, or editing the composer
invalidates an in-flight result. The utility profile may be the primary model
or an explicitly saved model profile; credentials remain write-only.

Neither naming nor manual suggestions has a per-session call quota. Naming
can update after every new completed turn, without a five-turn or five-minute
cooldown. Duplicate notifications for an already attempted turn do not issue
another request; failures can retry on the next completed turn. Suggestions
do not read or write the old attempt-budget file. Requests use short excerpts
of recent messages, so a long assistant reply cannot crowd out the user's
recent intent. Cancellation, request deadlines, and output-size bounds remain.

Naming and suggestion requests are one-shot utility requests with their own
shape, independent of the main agent loop's thinking settings: they never
send a `thinking` object, they lower an existing `reasoning_effort` to `low`,
and they carry an output budget sized for brief thinking plus one line of
text. Built-in presets run as reasoning models per their official parameters,
so these bounds keep the answer from being crowded out under the controlled
reproduction; the exact vendor-side accounting of reasoning tokens against
`max_tokens` is not claimed for every provider.

When a naming or suggestion request fails, one bounded evidence line is
appended to `~/.clat/utility-log.jsonl` and the frontends show the same
failure text. That text is a CLAT-owned category code — for example
`request failed (server)`, `request failed (transport)`,
`empty response (finish max-tokens)`, or `cancelled` — never the provider's
error body: response bodies from the network are untrusted input, so they are
classified by CLAT's own stable error taxonomy instead of being copied,
truncated, or echoed. The log records no credentials and no conversation
text, is serialized across writers, and rotates to `.1` once it exceeds
256 KiB; if rotation itself fails the line is dropped so the log stays
bounded. Naming never blocks the run and never retries in the background.

### WeChat remote control

The optional WeChat frontend uses the official iLink bot API from the same
`clat serve` process. It is disabled by default. Stop any running CLAT process,
then bind the machine, authorize one WeChat user, and start the host explicitly:

```bash
clat wechat bind                 # scan and confirm the QR code in WeChat
clat wechat pair                 # prints a one-time six-digit code
# In the bot chat, send the standalone message: /pair <code>
clat wechat status
clat serve --im wechat
```

Binding stores the bot credential locally in `~/.clat/im.json` with private
file permissions, but authorizes no sender. The pairing code expires after 60
minutes, succeeds once, and rate-limits repeated failures. You can replace a
confirmed binding with `clat wechat bind --replace`. To revoke the credential,
every paired user, chat mapping, cursor, and delivery receipt together, stop
serve and run `clat wechat unbind --confirm`. The Workbench settings surface
offers the same binding, pairing, status, and unbind lifecycle.

After pairing, use these standalone chat commands:

- `/new` arms a new CLAT session; the following text or image message
  materializes it and creates the durable chat-to-session mapping.
- `/status` reports whether the chat is mapped and whether a run is active.
- `/stop` cancels only the active run owned by this mapped chat.
- `/help` lists the commands.

Ordinary text enters the same typed prompt and journal-admission path as the
TUI and Workbench. A message arriving during this chat's run uses the same
in-run steering semantics. PNG and JPEG images are downloaded only from an
official Weixin HTTPS media host, decrypted, validated, staged as short-lived
input, and then pass through the normal attachment admission limits. Text that
looks like a path or URL grants no file authority.

Replies, selected run status, typing state, and permission requests are
projected back to the paired chat. The queue retains at most 128 already
Unicode-safe chunks; under backpressure it keeps the newest chunks and the next
text reply reports how many older status chunks were dropped. Chat mappings,
the poll cursor, and replay receipts survive a graceful restart. An invalid
iLink credential cancels the active run, clears the binding fail-closed, and
requires a new QR binding. See [Permissions](permissions.md#wechat-approval)
before approving tools from a phone.

### Running serve continuously

`clat serve` owns one project and the local `~/.clat` storage root for its
complete lifetime. A second CLAT process using that storage root fails with
`another CLAT process holds this storage root; close it first`, even when the
two processes request different ports. Keep the working directory and `HOME`
stable across restarts so the process reopens the intended project, sessions,
credentials, and browser token.

Ctrl-C and, on Unix, SIGTERM/SIGHUP all enter the same graceful shutdown path:
the server stops accepting requests, cancels the active run, drains bounded
connection/work queues, closes the Application, and flushes its journal before
exiting. A second signal is an emergency hard exit and does not promise those
steps. Service managers should therefore send SIGTERM and allow a shutdown
grace period instead of using SIGKILL.

For a simple foreground session that survives a terminal disconnect:

```bash
tmux new-session -s clat-serve 'cd /absolute/project && exec clat serve'
# Reattach later with: tmux attach -t clat-serve
```

Append `--im wechat` to the tmux, launchd, or systemd command only after the
binding and user pairing above are complete.

On macOS, save a user LaunchAgent such as
`~/Library/LaunchAgents/io.artec.clat.serve.plist` with absolute paths:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>io.artec.clat.serve</string>
  <key>ProgramArguments</key><array>
    <string>/absolute/path/to/clat</string><string>serve</string>
  </array>
  <key>WorkingDirectory</key><string>/absolute/project</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>/absolute/log/clat-serve.log</string>
  <key>StandardErrorPath</key><string>/absolute/log/clat-serve.err</string>
</dict></plist>
```

Load it with `launchctl bootstrap gui/$(id -u)
~/Library/LaunchAgents/io.artec.clat.serve.plist`. Stop it gracefully with
`launchctl kill SIGTERM gui/$(id -u)/io.artec.clat.serve` before changing the
project or configuration.

On Linux, a user service at `~/.config/systemd/user/clat-serve.service` can use:

```ini
[Unit]
Description=CLAT local serve host

[Service]
Type=simple
WorkingDirectory=/absolute/project
ExecStart=/absolute/path/to/clat serve
Restart=on-failure
KillSignal=SIGTERM
TimeoutStopSec=30

[Install]
WantedBy=default.target
```

Run `systemctl --user daemon-reload && systemctl --user enable --now
clat-serve.service`; inspect it with `systemctl --user status
clat-serve.service`. These examples intentionally keep daemonization outside
CLAT: the binary remains a foreground process with one explicit lifecycle
owner.

### Security boundary

- The listener is IPv4 loopback-only. There is no `--host` option.
- The default token is created once at `~/.clat/web-token`, stored as a normal
  `0600` file, and reused across restarts.
- `--rotate-token` atomically replaces it and revokes previously paired
  clients after the new server starts listening.
- Long-lived credentials never appear in URL queries, cookies, manifests,
  assets, logs, journals, or new browser storage. Script clients retain
  `Authorization: Bearer <token>` support.
- Local access is automatic when Host is exactly `127.0.0.1:<bound-port>` or
  `localhost:<bound-port>`, Origin is absent or equals `http://<Host>`, and
  Sec-Fetch-Site is `same-origin`, `same-site`, or `none`. Missing Fetch Metadata
  is accepted only when Origin is absent (local command-line clients).
  Cross-site requests and mismatched Origins are rejected even with credentials.
- Other authorities require a valid Bearer credential or an unexpired signed
  browser session bound to that exact authority. This reserves a remote access
  gate; it does not add a remote listener or a `--host` option. An SSH-forwarded
  loopback connection is treated as local; only forward it to trusted users.
- No CORS headers are emitted. The shell CSP permits the public catalog at
  `https://pi.at.cn`; catalog requests omit credentials.
- The credential grants the entire API. This is a single-user boundary;
  local processes are trusted, rather than authenticated as separate users.

`POST /auth` silently establishes a local browser session. Remote pairing
accepts an operator-supplied credential in Authorization and exchanges it for
an authority-bound, 30-day HMAC-SHA256 session cookie: HttpOnly, SameSite=Strict,
host-only, and separately named per port. The cookie contains no original
credential. Rotation revokes remote sessions; local access recovers silently.
Old browser localStorage credentials migrate once and are removed after a
successful exchange. Explicit `invalid_credentials` errors clear legacy
credentials; an ordinary unauthorized/policy error retains them and retries.
Credentials for remote access must be delivered privately out of band, never
in a URL. Remote TLS/Secure-cookie deployment remains a separate feature.

Static navigation through localhost redirects to 127.0.0.1, preserving the
workspace selector and discarding credential query parameters. Both spellings
enter automatically. `clat serve` opens the clean URL in the system default
browser; `clat host start [--trust] [--no-open]` opens only when it starts a new
host. Automatic TUI host startup does not open a browser. SSH, CI, and detected
non-graphical sessions suppress the handoff; opener failure falls back to the
printed URL. Use `--no-open` for supervised services.

### Web workbench

The settings window groups Appearance, Projects, Models, Companion,
Permissions, and WeChat into separate categories. The composer permission
control opens Permissions directly; Manage model profiles opens Models.
Session rows provide a more-actions button and a right-click menu, also
accessible with Shift+F10. The current session can be renamed from that menu.
When the desktop sidebar is collapsed, a full 44 px **Open sessions** tile
replaces individual session rows. Clicking it expands the named list and focuses
search without switching sessions or clearing the draft. Three dots remain a
separate, centered more-actions button only in the expanded list. Phone drawers
always show session titles, even if this browser last used a collapsed desktop rail.
If a background snapshot closes a stale session menu, keyboard focus returns
to that session's current row rather than a detached element.
The composer model picker exposes the current route's host-reported thinking
intensities; models without adjustable thinking show a disabled control.
Its clickable width follows the model/intensity label, bounded on narrow screens
with an ellipsis so it stays separate from Send and Stop. On phones, the menu
opens above the composer and scrolls within the available viewport height.
The details sidebar offers refresh, context inspection, and model-profile
management. Context inspection adds a read-only breakdown to the conversation
without sending the composer draft or making a model request.

The composer recalls the **current session's committed prompts** with the arrow
keys. `↑` steps from newest to older, `↓` steps back, and the `↓` past the
newest entry restores the untouched draft exactly (including CJK and line
breaks). The list comes from the same journal-derived
`workbench.info → session.recent_inputs` authority as the terminal (bounded to
the most recent 50, oldest first), so prompts committed from the TUI are
recallable here and vice versa after the next snapshot. Boundaries follow the
caret's **visual line** — soft-wrapped rows count: arrows move within the body
first, and history triggers only when the caret sits on the first visual line
(`↑`) or the last visual line (`↓`) with no active selection; IME composition
and the open command/skills picker always keep the keys. While browsing, a
duplicate snapshot never disturbs the cursor, and genuinely newer commits
arrive by value-anchoring the cursor so the pre-browse draft stays recoverable
past the newest entry. Queued steering joins the list only after the host's
durable claim, slash commands and failed submissions never do, and nothing is
written to browser storage.

The conversation is the default reading space: details start closed in a new
browser, while an existing desktop open/closed preference is respected. Phone
drawers start closed. Neutral controls and a restrained warm-gray light / dark
palette keep answers prominent; body text is 16 px, tables 15 px, and fenced
code 14 px. Wide tables and code scroll within their own containers.
The permission control remains visible next to the composer tools; Plan, Goal,
run status, attachments and cancellation retain their existing behavior.

Appearance offers **User bubbles** (default) and **Single column** for comparing
role layouts on the same conversation. **Work summaries** (default) collapse
adjacent ordinary process records; expand a summary, then a tool, to inspect its
arguments or output. **All steps** starts those summaries expanded. Changing
these views keeps the current draft and existing tool nodes intact. Record
counts include separate requests/results, not inferred tool-call or turn counts.
Answers, pending approvals/questions, failed tools, permission decisions other
than Allow, retries, command output and cancellation/failure outcomes remain
outside the collapsed summaries.
Non-completed replay turn endings likewise remain visible, including blocked,
aborted, error, token-limit and interrupted endings. Grouping follows visible message and
interactive/error boundaries, including history-page boundaries; it does not
change runtime events, permission authority or replay content. The layout and
process-display choices are browser-only preferences, not session policy.

The embedded zero-build PWA provides a session sidebar, conversation surface,
and project/model/run/MCP inspector. On narrow screens the side surfaces
become drawers. Browser storage is limited to presentation preferences and a
private HttpOnly session cookie; session content, run state, permission mode, model state,
and MCP facts are rebuilt from authenticated snapshots, journal replay, and
live events.
Message bodies render a safe Markdown subset (headings, lists, tables, quotes,
fenced and inline code, emphasis, links, and rules) after a response settles; active
streams remain plain text. Raw HTML and remote Markdown images are never
interpreted, and only HTTP(S) links become clickable.

Unsent PWA text stays with its project/session in this page's memory; switching
back restores it. Each **New conversation** starts a separate draft, and the
first accepted message transfers its identity to the materialized session.
Reloading or closing the page discards these drafts. Draft images are not
restored across sessions: changing sessions asks before releasing them, and
cancelling preserves both text and images. A submitted attachment must finish
its admission receipt before switching. Late completions cannot clear a newer
draft or restore already-acknowledged text as an unsent message.

Settled replies and fenced code have **Copy reply** / **Copy code** actions
that copy their original text, including Markdown/code line breaks. Selecting
body text exposes **Quote selection**, which appends an editable quote without
replacing the current draft. Clipboard failures are reported on the button.

The PWA `/` picker shares the structured command/skill catalog and fill-only
keyboard behavior above. Model, permission, resume, rename and compact entries
instead navigate to their existing controls; choosing them does not perform
the action. IME composition and pasted paths do not submit a candidate.

Use the conversation's Find button or Ctrl/Cmd+F to search original user/agent
body text. Enter/Shift+Enter and the arrow buttons visit matches; **Earlier**
reads one older page, and Close/Esc cancels find. The scope label says whether
earlier messages remain unloaded. Prefix pages and streaming tokens preserve
the selected message; the preview shows the exact source occurrence. This is
not cross-session full-text search. Sidebar search only filters session titles
and IDs, not their message content. Both frontends display up to 2,000 matches.

The header **Tools** menu groups Workspace changes, Captured operations, Files,
Plan / Goal, Tasks and Help without crowding the reading surface.

**Workspace changes** opens a read-only file list and loads a selected file's
staged/unstaged diff on demand. It shows current project working-tree facts,
including pre-existing user edits and changes left after failed/cancelled runs;
it does not attribute them to this agent or turn. Refresh after further edits.
Git is optional: missing Git, a non-repository project and a failed query are
explicit states, not “no changes.” This view has no stage/commit/revert/push.
The list is capped at 500 files (with a truncation label); Git output is bounded
to 512 KiB for status and 128 KiB per patch, with a three-second child-process
deadline and only one review query in progress. Untracked text previews use the
same 128-KiB limit; binary/conflict/submodule and oversized results explain the
fallback. Symlink untracked previews and paths outside the project are refused.
External diff, textconv, fsmonitor and configured clean/process filters are
disabled for review, as is lazy object fetching; incomplete repositories may
therefore report an error rather than trigger a fetch.

**Captured operations** is a separate, session/turn-owned native-file ledger.
It distinguishes the pre-operation baseline (including your existing dirty
edits), captured native result, and current disk identity/content check. It
covers relative-path `write_file`, `edit_file` and native patch commits only:
shell/MCP writes, absolute paths, directory creation and external effects are
not undone or attributed. Empty capture does not prove no workspace changes.
Records survive failed/cancelled runs and restart. Files are limited to 1 MiB
of UTF-8 text per baseline/result; binary baselines are refused. An unfinished
follow-up write preserves the last confirmed result separately from its intent.
There are limits of 100 files and 16 MiB serialized record per turn; capture
persistence failure refuses the native commit rather than proceeding without evidence.

**Restore captured files…** requires confirmation and an idle host, and is
unavailable in Read Only mode. The host rechecks the ledger revision, path,
identity, metadata and content before each restoration. Conflicts stop the batch;
partial results are persisted, and retries skip already restored entries.
Recovery never overwrites a new occupant: it detaches and checks the captured
file, then publishes into an absent name. Private `.clat-restore-*` directories
retain recovery bytes; no automatic cleanup deletes them. Interrupted or racing
non-cooperating edits may require manual inspection of that reported directory.
This is not an atomic multi-file transaction or a general undo of a run.

**Files** searches the current project (100 results; at most 10,000 entries,
depth 32 and a 200-ms scan budget). Preview is UTF-8 text, up to a 256-KiB prefix
and 200 lines. Binary/non-UTF-8 files use an explicit unsupported state; images
retain the existing attachment workflow. All symlink components, traversal,
the actual configured CLAT storage tree (even if nested inside the project),
private recovery paths and common secrets such as `.env`, `.pem`, `.key`
and SSH keys are refused. This exclusion list is not comprehensive secret
detection: check file contents yourself before quoting them.
The preview carries a project-relative path, range, read time and version.
**Read / recheck** refreshes it; **Quote snapshot to draft** rechecks the version
and appends an editable fixed text snapshot (at most 32 KiB) without replacing
your draft. Later file changes do not update that quote at send time. Narrow the
range for larger files; PDF/Office/video parsing is not provided.

Session menus provide **Pin**, explicit **Archive**, and **Restore**. Organization
is saved by the core for the workspace and shared with TUI; it is not a browser
hide preference. **Archived sessions** opens the recoverable list. Archiving
retains chat and leaves the selected conversation selected; the current row
stays visible. Organization is unavailable while a run/compaction owns the
session. TUI `/resume`: P toggles pin, A twice confirms archive, F2 switches the
archived list, and A restores an archived entry. `/` filtering takes precedence.

**Tasks** reads only the selected current run generation's ProcessService
commands, running/completed status, exit code and bounded stdout/stderr/PTY
tails (16 KiB each). Refresh explicitly; consumed entries may disappear on the
next spawn, and run cleanup/restart removes all entries. No cross-run history,
background jobs, stdin or process-specific cancel controls are added. Viewing
logs does not consume the tool's output cursor or extend a process lifetime.

**Help** explains current shortcuts and optional online page notifications.
Notifications are off by default and require a user click plus browser permission;
completion, failure, approval and questions come only from live host facts and
are deduplicated in this page. Focused pages use inline status; notifications do
not include prompt/file text. There is no Web Push or closed-browser delivery,
and mobile/background browser restrictions apply. **Latest messages** returns
from older text to the bottom; Find remains available on mobile. The project-name
area links directly to project selection. Browser Ctrl/Cmd+L and IME keys are
not repurposed.

The active model is chosen from the compact picker immediately before **Send**;
built-in presets and saved profiles share that next-run menu, while write-only
preset keys may be supplied there. Workbench settings retain profile creation,
editing, and deletion, but no longer hide the everyday model switch among
configuration controls. Running requests keep the route they started with.

The workbench supports streaming responses, tool cards, approvals, new/switch/
rename session actions, permission-mode changes, cancellation, in-run steering,
and manual history compaction. The Session inspector's **Compact history**
button starts the same core compactor as `/compact`; while it is active the
composer and session mutations are locked, and the button becomes a cancellation
control. Completion is journal-backed and therefore visible again after reload.
Full Access requires both a UI warning acknowledgement and the protocol
confirmation described in [Permissions](permissions.md).

Tool cards pair by call id exactly like the TUI: one card per tool call,
opened at the request position and settled in place when the result arrives
(arguments and output on the same card; `Failed`/`Denied` states and the
permission notice stay visible). Duplicate or late request frames reuse the
same card instead of opening a second one, and a result that arrives without
a usable call id (or before its request was replayed) still shows a fallback
result card at the finish position — results are never silently dropped.
Successful cards group into the collapsible work summary, while a failed call
moves its card out of the summary so errors are never hidden; a tool failure
never ends the run by itself. Reloads and reconnects rebuild the same
one-card-per-call view from the journal, including during an active run.

When a run settles with observable unfinished todos, the transcript shows the
same honest accounting line as the TUI (`… todos unfinished · task completion
not verified · send a message to continue`); it reappears after a reload, and
the run state under the composer keeps the real terminal state instead of
going quiet. Duplicate terminal notices are deduplicated, and a settled frame
only owns the terminal when no new run has started — a late completion notice
after you already sent the next message never overwrites the active run, and
accounting replies that come back after you switched sessions are dropped
rather than leaking into the new session's transcript.

Conversation history opens on the newest message. The server sends a
message-aligned tail window (50 messages by default), and the workbench loads
earlier pages automatically when scrolling within 512 px of the top. Prepending
keeps the current reading line fixed. A slim map on the right represents every
user and assistant message in the session; hover or keyboard focus shows a
bounded turn preview without Markdown punctuation, and selecting an unloaded
marker fetches earlier pages before jumping. The preview distinguishes your
message in primary text from the agent reply in softer gray. For long sessions it renders only
the visible marker window, while retaining the full scroll range and per-message
positions; the focused map can be browsed with arrow, Page, Home, and End keys.
The map hides on narrow mobile layouts. The composer floats
above the transcript while measured bottom clearance keeps the last message
fully readable.

Informational slash commands use the same core command catalog. `/context`
renders its estimate as a readable multi-line breakdown, including plan, goal,
and memory injection state. Composer badges show **Plan** and an armed **Goal**
from the core workbench snapshot, including after reload or session selection.
The browser does not persist workflow authority in localStorage or infer it
from command text. The command bridge adds `memory`, `goal`, and `subagent_status`
result kinds with read-only DTOs and a text `message`; `goal_run` also includes
its confirmation message. `workbench.info` adds `plan_mode_active` and
`goal_armed` booleans. RunEvent and durable event vocabularies are unchanged.

Click either badge or **Tools → Plan / Goal** for structured details: Plan Mode
policy, approved plan text, model-maintained todos and durable bounded Goal are
four distinct sections. Todo shows at most 100 entries, with a truncation label.
Goal details include acceptance, budgets, phase and stop reason.
Exit Plan Mode, start bounded continuation, resume state (does not
run) and clear Goal reuse existing core commands, require confirmation and are
disabled while busy. Goal actions check both target ID and revision; stale
details cannot operate on a recreated goal. TUI `/help` advertises `/plan` and
`/goal`; `/goal` includes the same budgets, acceptance and action guidance.

Model lifecycle and reasoning traces use human-readable labels in the visual
surface (`Model request started`, `Reasoning summary`, and so on). Reasoning is
shown as a compact **Think** disclosure row: while streaming it follows the
latest line with a reduced-motion-aware sweep; once settled it summarizes with
the first line, and expanding it reveals the full trace. The stable
wire event id remains available as diagnostic metadata; the frontend does not
rename or mutate the underlying `RunEvent` protocol. The workbench uses one
consistent inline SVG icon language for actions, tools, traces and panel
navigation, including keyboard and reduced-motion accessibility.

The sidebar's Plugin Index shows a searchable projection of the public
[pi.at.cn](https://pi.at.cn) catalog. The public catalog is a display surface;
its cross-origin GET explicitly omits credentials, cookies, referrer and the
local Bearer token. When the catalog is unavailable, the PWA shows a clearly
labelled built-in preview and retains the external link. The panel installs,
updates and manages packages through the authenticated local host, which owns
signature verification and permission review. The public website cannot install
or remove packages. See [PWA and TUI plugin control](plugins.md#install-and-manage-from-the-pwa-or-tui)
for panel controls and [the signed market workflow](plugins.md#signed-remote-market)
for CLI operations.

The manifest and all asset URLs are credential-free. With the default stable
port and persistent token, an installed PWA survives normal server restarts.
Local access survives credential rotation silently; remote sessions need a new credential exchange. There is no offline data
mode; without a running server the shell cannot load conversations.

Changing the port changes the browser origin. An app installed from the old
port keeps opening that old origin; it is not redirected automatically. Open
the new `http://127.0.0.1:<port>/` in a browser, connect automatically, and
install that origin as a separate app if a standalone window is desired. The
old app can then be removed independently.

### RPC and event contracts

RPC uses `POST /api/<method>` and returns either
`{"ok":true,"value":...}` or
`{"ok":false,"error":{"code":"...","message":"..."}}`. Admission
errors for client-keyed messages add `error.receipt`; clients must follow its
`state` and `retryable` fields instead of guessing whether to resend.

Current methods are:

- `workbench.info`
- `session.list`, `session.info`, `session.new`, `session.switch`,
  `session.history`, `session.rename`, `session.compact`
- `model.overrides.set`, `model.settings.get`, `model.utility.get`,
  `model.utility.set`, `model.profile.get`, `model.profile.save`,
  `model.profile.activate`, `model.profile.delete`,
  `model.preset.select`, `model.thinking.cycle`
- `permission.set`
- `wechat.binding.status`, `wechat.binding.start`, `wechat.binding.poll`,
  `wechat.binding.unbind`, `wechat.pairing.create`, `wechat.user.allow`,
  `wechat.user.remove`
- `draft.open`
- `prompt.suggest`, `command.run`, `prompt.send`, `steer.send`, `run.cancel`
- `approval.respond`
- `question.respond`

The host-level `host.*` and `workspace.*` methods are listed under
[Multiple projects in the web workbench](#multiple-projects-in-the-web-workbench)
below.

`workbench.info` is lightweight: it returns project, active-session, model
(including all five typed override states), permission, MCP, capability,
active-run, and active-compaction summaries without loading a transcript or
returning credentials. `session.recent_inputs` inside it is the active
session's bounded (50, oldest first) committed user prompts — the shared
input-recall authority for the TUI and PWA composers; it never includes slash
commands, unadmitted drafts, or another session's text.
`session.compact` accepts `{"action":"start"}` (the
default) or `{"action":"cancel"}`. It is mutually exclusive with a run and a
second compaction, and reports progress through `notice` frames rather than
holding the RPC request open.
`model.overrides.set` changes exactly one allowlisted override with
`{"field":"output_limit","state":"inherit|set|clear","value":...}`;
`value` is required only for `set`, credentials are never accepted, and an
active run returns `busy`. Only one run is active at a time; `prompt.send`
reports `busy` rather than queueing another.

A client-keyed `prompt.send` acceptance and its `prompt.settled` frame carry a
Committed receipt. `steer.send` returns Reserved while queued, the live
`steering_applied` event upgrades it to Committed after journal flush, and a
recall/not-running result is RolledBack and retryable. Reconnect replay of a
keyed user message also carries the rebuilt Committed receipt.

`draft.open` returns a random, short-lived scope bound to the current session
selection. Raw `POST /api/drafts/<scope>/images` accepts one bounded PNG or
JPEG after Bearer authentication; `prompt.send` and `steer.send` reference its
opaque upload IDs. `GET /api/attachments/<attachment-id>` is likewise Bearer
protected and reads only a descriptor reachable from the active session through
a fixed-length 64KiB stream (with a separate four-download slow-reader limit,
15-second write-idle timeout, and 60-second whole-response timeout).

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

Two host generations are supported. Legacy DSH (0.1.1-rc.2 era) is detected
through the old `host.describe` fingerprint. DSH 0.1.2+ (including
0.2.0-rc.2, where `host.describe` is absent) speaks the Typert
Gateway (`/api/remote.mux` with browser-session authentication); for those
hosts CLAT authenticates through one of two paths:

- If a host is already running and your `~/.dsh/.credentials.yaml` carries
  the browser-session record (it does once `dsh web` has run on this
  machine), CLAT reads that single record and mints its own short-lived
  session cookie — no token ceremony. CLAT only ever reads this one
  record and never writes anything under `~/.dsh`.
- If no host is running and the `dsh` executable is installed, CLAT starts
  `dsh web` itself (retrying on an OS-assigned port when the preferred one
  is taken) and reads the launch token from the startup line — again no
  extra input needed. A host CLAT started is stopped when CLAT exits.
- If the cookie mint is not possible (no credentials record yet, or the
  host rejects it), pass the host's startup URL (the token is printed once
  in the host's stdout and cannot be probed):

  ```bash
  clat dsh --url 'http://127.0.0.1:3080/?token=<launch-token>'
  # or: CLAT_DSH_URL='http://127.0.0.1:3080/?token=…' clat dsh
  ```

  Only loopback URLs are accepted. This is DSH's security model: the
  browser-session gate applies to every API call and the stream upgrade.

It probes and fingerprints the host; a host that answers with
"credentials required" is connected rather than replaced — CLAT never
spawns onto a port an authenticated host already owns. A non-DSH process
occupying the requested port is rejected.

DSH owns sessions, providers, permissions, tools, and execution. CLAT renders
the host's events and never writes `~/.dsh`. It remembers only the last opened
DSH session id in `~/.clat/dsh-last-session`; missing or invalid memory falls
back to the newest host session.

The shared commands are `/new`, `/resume`, `/model`, `/perm`, `/rename`,
`/clear`, `/help`, and `/quit` (alias `/exit`); `/perm` also accepts
`/permission`. `/compact` and `/mcp` are unavailable because
those concerns belong to the host. The DSH client reconnects after transport
loss or a temporary host-service failure, but stops retrying on a protocol or
request error and shows the host error code. It forwards
prompts/steering/cancellation/approval answers through the API and shows
host-reported usage without inventing local wallet data. A prompt whose
receipt was lost during disconnection is never sent automatically again.

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
  Windows ACL confines writes partially; it does not filter reads, network or
  inherited secrets. On Linux use `--yes` only inside a container, sandbox, disposable
  checkout, or similarly explicit containment boundary.
- Inspect the returned `sandbox.provider`, `enforcement`, `policy_digest`,
  fallback and denial facts. Process-tree cleanup proves lifecycle ownership;
  it does not prove OS confinement on a fallback platform.
- Treat MCP servers and WASM components as code you are choosing to run; read
  [MCP security](mcp.md#security-posture) and
  [WASM write grants](wasm.md#filesystem-write-grants) before adding
  third-party extensions.
## Multiple projects in the web workbench

Workbench settings includes a Projects section. Enter an absolute local project
directory; a previously untrusted project requires the explicit trust checkbox.
Open its link in a new tab. Projects share the host's model configuration and
credentials, but keep separate sessions, permissions, drafts and event streams.
Closing a browser tab does not stop the host. Up to 16 projects may remain
mounted at once. When capacity is reached, the host reclaims an idle additional
project runtime; connected event streams, active operations and live drafts
prevent reclamation. Session history and trust remain on disk, and the host
keeps its storage-root lease. If every project is active, opening another
project fails with a capacity error rather than disrupting existing work.

Authenticated clients can call `host.describe`, `workspace.list`,
`workspace.open` (`root`, optional boolean `trust`), and `host.stop`.
`host.takeover` additionally requires the exact instance id and a strictly
newer product version; it serves the native startup upgrade flow described
earlier and is rejected for same-version or older clients.
`workspace.open` returns an opaque `id` and `api_prefix`; prepend that prefix
to the existing `/api/...` paths, including events, image uploads and image
reads. Project identifiers are temporary; reopen the project after an idle
runtime is reclaimed or after a host restart. `host.describe` reports instance,
wire, journal and additive build-fingerprint identities so native clients can
reject incompatible or stale builds. Browser/raw HTTP clients do not present or
compare a CLAT executable identity and continue to attach normally.
Automatic startup rejects a live, authenticated, identity-matched incompatible
host or different advertised build immediately. It never stops that host or
starts a replacement writer; stop the old host explicitly before upgrading or
testing a newly rebuilt development binary.

`clat attach --port 2691` connects the terminal to an already running local host
and opens the current project; add `--trust` to explicitly authorize that
project. It verifies the storage root, instance, protocol versions and any
advertised build fingerprint, then reads the host's private saved token (an
explicit serve token is not saved).
This initial client supports text and PNG/JPEG submission, streaming, cancellation, session
commands, shared approvals and ask-user dialogs. `/reconnect` rebuilds a disconnected view;
uncertain submissions are not automatically resent. Closing the terminal
disconnects only that client, not the host.
`/quit` and `/exit` also detach locally, even while offline or a host run is
active. `/compact` starts host-owned history compaction; `/compact cancel`
requests cancellation. Start/cancel results and completion notices appear in
the status line. `/cancel` still cancels a run, not compaction. These operations
do not delete the original history or automatically retry uncertain requests.
`/perm` (`/permission`) opens the traditional permission selector using the
host's current mode. Full Access requires a second confirmation, including
when the last received snapshot already shows Full Access. A disconnected
client cannot change permissions; a session change invalidates an open picker.
`/resume` opens the traditional keyboard/mouse session picker using the host's
session list. `/rename` opens the existing editable title dialog; a selection
change while it is open prevents a stale rename from reaching another session.
Once you rename a session, model-generated titles cannot replace your title,
including after the session is reopened.
Automatic naming follows recent conversation after a successful run. It may
update a model-generated title after each newly completed turn, without lifetime
quotas or time-based cooldowns. The scheduling sidecar prevents duplicate calls
for the same turn after reopening. Each request uses at most 12 recent
user/assistant messages, 1000 characters per message and 6000 characters total,
without tools. An unreadable or damaged scheduling file pauses automatic naming,
but does not disable manual suggestions.
Built-in DeepSeek, GLM, Qwen, Kimi, and Hy routes use the table-declared
companion for naming (for example, `kimi-k3` uses `kimi-for-coding` and
`hy4-preview` uses `hy3`); other models and customized routes use the primary
model. Companion requests
keep the same endpoint and credentials, use the companion's model settings,
and record the actual model in the title's source. Like suggestions, they
are shaped as one-shot low-effort requests without a `thinking` object
(see *Companion utility and manual suggestions*).
If an error carries a committed admission receipt, the submitted text is not
restored as a new draft; any newer input is preserved. Without a valid receipt,
the text is kept with a warning to check host history before sending it again.

Use `/attach path/to/image.png` (or `@ path/to/image.png`) to prepare images;
Enter sends them with the text, including an image-only message. Existing image
list/remove/reorder/clear commands remain local. Files are uploaded to a scoped
host draft; their local paths are never sent as attachment references. In-run
image steering waits for durable admission before consuming the draft. Failed
uploads retain images and text without clearing newer edits; cancellation
remains available while waiting. Live and replayed messages show image labels.

`Ctrl+V` captures clipboard images or falls back to text; `/pi` and
`/paste-image` capture images only. Bracketed paste never probes the clipboard.
Captured images live in a private, client-owned temporary draft: unsuccessful
submissions retain them, while committed submissions and draft removal release
only those temporary sources. User-selected source files are never deleted.

`/model` opens the traditional two-level picker using host profile summaries.
Preset selection, existing-profile activation and confirmed deletion go through
the host; credentials stay there and changes apply to the next run. Failed
requests keep the picker open; check host settings before retrying an uncertain
result. Basic profile creation/editing, write-only API-key entry and Shift+Tab
thinking levels are supported; see [Model editor](model-editor.md#attached-terminal-support)
for the save-versus-activate distinction. Advanced profile fields remain pending.

Host ask-user questions open the traditional terminal dialog: arrows and Enter
select an option, `c` opens custom input when allowed, and Esc backs out of custom
input or declines the question. Questions without options open directly in text
input mode. Multiple questions queue without replacing the current answer draft.
Every attached client can see pending questions; the first valid answer wins and
closes that question on other clients. A newly attached client receives unanswered
questions. Closing one terminal does not decline on behalf of other clients.
Cancellation, loss of all subscribers, or a ten-minute timeout declines the
question; failed answer requests retain the local answer and are not retried
automatically. Questions and answers remain ordinary tool history, not new
journal event types. PWA shows the same pending questions as inline cards: click
an offered option, enter a custom answer when allowed (or when no options exist),
or explicitly Decline. All controls are disabled during submission. Request
failures retain the answer for manual retry; a host resolution closes only its
matching card. Disconnecting disables old cards without declining; reconnecting
rebuilds unanswered questions from the host (unsent browser answer drafts are not
persisted across reconnects or reloads).

For other clients, `question.respond` accepts `rpcId` and `answer`: either
`{"kind":"selected","value":"exact option label"}`, `{"kind":"custom","value":"text"}`,
or `{"kind":"declined"}`. It is scoped to the target project and uses normal host
authentication. Invalid answers leave the question pending; duplicate, cancelled
or expired answers return `not-pending`. Custom text must be nonblank and at most
64 KiB. The host admits at most 32 concurrent questions, each at most 64 KiB of
serialized question data. Live `notice` payloads use `question_requested` (with
`rpc_id` and `question`) and `question_resolved` (with `rpc_id`).

Full terminal dialog/command parity is still pending;
this is not yet a replacement for the traditional TUI. Plain `clat` opens this
attached mode by default — it discovers or starts the background host before
showing the TUI — while `clat standalone` keeps the traditional in-process TUI.

### Next-turn queue

On CLAT standalone and attached terminals, `Ctrl+Q` queues the composer text;
`Alt+Q` recalls the newest queued item. The status bar shows the count and first
item. In the PWA, use **Queue next turn** and **Recall last**; the queue above the
composer shows each pending message. Enter during a run still sends steering to
the next model-request boundary. The next-turn queue accepts text only.

Each successful run dispatches one FIFO item as an ordinary new run; successive
successful runs continue draining. Cancellation or failure pauses the queue and
retains every undispatched item. Recall items to edit them, or submit a normal
human prompt: its successful completion resumes draining. Admission failure
before durable commit retains the item; a message already committed is never
resent. Recall prepends text instead of overwriting an existing draft. The PWA
also restores unclaimed plain-text steering when its run ends. A lost recall
response can be retried with the same request id; after a session switch, its
text returns to the original session draft. Recall receipts are bounded and
process-local.

Queues belong to a mounted project and its current session, exist only in the
host process, and disappear on process exit/restart. Session new/switch refuses
while items remain; recall them first. Opening another workspace keeps the old
project's queue in that mounted project; it is never transferred to the new
workspace. Non-empty queues prevent automatic host takeover and idle project
reclamation. DSH terminals and the WeChat channel have no new queue entry point.

## Durable task progress and explicit recovery (`/task`)

A task is an explicitly created objective and progress record in the selected
session. Ordinary conversations remain ordinary conversations. A task is separate
from Todo, compaction summaries and the optional Goal driver: finish/clear a
separate Goal and exit Plan Mode before creating or continuing a task. Task
and unfinished Goal workflows exclude each other in both directions: complete
or cancel the task before creating, resuming or arming a Goal, even when Goal
creation requests no immediate execution. Restored conflicting state grants no
execution authority. Task
continuation starts **one new Run**, with the current permission policy and
approver; it never restores an old approval, process, PTY, tool handle or armed
Goal. The user can declare completion or explicitly authorize acceptance of a
model completion candidate under a confirmed contract (see below). Cancellation
remains a user operation. A model checkpoint is a progress claim.

Create a task in the TUI or PWA command composer:

```text
/task create {"objective":"Read local materials, prepare a table and report","acceptance":["Review the report totals"],"budget":{"max_tokens":1000000,"max_runs":8,"max_seconds":3600,"run_tokens":200000,"run_seconds":600}}
/task show
/task check
```

All five budget fields are explicit positive limits: total tokens, total Runs,
total active seconds, and tokens/seconds for each continuation. At most 32 Runs
and 86,400 total seconds are accepted. A configured model run-token cap can
further reduce a continuation's cap. Normal completion, failure, retry, sampling
and internal compaction share the existing conservative spend ledger. Pausing
or reopening never clears consumption. Before a Run starts, its token/time
slice is reserved durably; settlement replaces it with measured conservative
usage and monotonic elapsed time. If the process dies or settlement fails, the
reservation remains charged and is marked unknown consumption. An exhausted
task does not execute again. Start a new task only as a new explicit decision.

The reservation has its own unique Run ID and no session turn until the normal
message admission commits. A failure before admission leaves no user message;
retrying uses a new Run ID and preserves the prior budget record. A crash before
settlement keeps the reserved slice charged. After admission, the actual session
turn is bound durably before any model request or task effect.

`/task check` is read-only and does not call a model or execute a saved next step.
It returns the task identity, revision, current permission mode, remaining budget,
artifact observations, unresolved operations, known required-tool availability,
and a `digest` binding those observations. Reopening only makes saved progress
available; it does not start work. To continue, copy the identity, revision and
check digest from the report:

```text
/task continue <task-id> <revision> <check-digest>
```

Application checks the observations again at start. A changed revision, file
hash, permission mode, model configuration or job observation requires a new check and explicit
continuation. Changed, missing, restricted artifacts or unresolved job
observations also require `--accept-changes` after reviewing the report; this
acknowledgement does not resolve an uncertain operation or grant tool permission.
Execution still checks each actual tool invocation under the current policy.
Required-tool availability uses the same Plan, subagent and verified model
capability policy as the actual Run; image tools require verified image input
and image tool-result support. Registered tools are not automatically usable.
The model receives the saved objective/progress, recovery observations, pending
questions and confirmed operation references through the normal context owner.

During a task Run, `read_task` exposes the current revision and operation
references. `update_task` saves a complete bounded semantic checkpoint using
`task_id`, `expected_revision` and `progress`. Progress supports `summary`,
`steps`, `questions`, `next`, `required_tools`, `artifacts` and `jobs`; omitted
fields use empty defaults. Artifact records contain a project-relative `path`,
`summary`, SHA-256 `digest` and `evidence` operation IDs. Saving checks the actual
bytes and the evidence's committed identity. Prior artifact/job references are
retained when omitted; adding a reference does not certify its business content.
User checkpoints use the same format:

```text
/task save <task-id> <revision> {"summary":"Table saved; report remains","steps":["Read materials","Prepare table"],"questions":["Confirm totals"],"next":"Write report"}
/task pause
/task complete <task-id> <revision>
/task cancel <task-id> <revision>
```

In the TUI, use Esc to cancel a running task, then `/task pause` when idle;
slash commands retain the existing idle-only rule. The Application/PWA pause
command cancels and joins a live task Run, preserving its last committed semantic
checkpoint and execution facts. Normal exit does the same through the existing
run shutdown. It does not promise to recover uncommitted model thoughts.

An execution intent is committed after permission and before invocation. A
returned result becomes confirmed only after its outcome commits; a confirmed
error is still an error, with possible side effects. Cancellation before actual
invocation is recorded as `not_executed`; cancellation during/after invocation,
or a crash between intent and outcome, remains `unknown`. Result-persistence
failure stops further task effects, including nested host-tool calls. Recovery
never replays saved tool arguments. An identical confirmed side-effecting tool
invocation from an earlier Run is refused; this fingerprint fence cannot prove
semantic equivalence of different requests or provide external exactly-once.
Investigate unknown results in the affected system first. Only an explicit user
decision can resolve one, with a note:

```text
/task resolve <task-id> <revision> <operation-id> confirmed <what was checked>
/task resolve <task-id> <revision> <operation-id> not_executed <basis for retry>
```

Unknown operations block continuation, completion and ordinary prompts in the
same session. Use a separate session for unrelated work while investigating.
A resolution is a human
statement, not automatic verification. Any later execution still passes normal
approval. Do not classify an operation from the mere presence of `tool/call`,
the absence of `tool/result`, or an error message.

Task recovery pins the original project-directory identity; moving/replacing
a root under the same name requires a new explicit task/session rather than
automatically rebinding old work. Checks read only the recorded project-relative regular files, reject symlink
components and read at most 8 MiB per file/32 MiB per checkpoint or check.
`unchanged`, `changed`, `missing` and `restricted` are separate observations;
an equal hash only describes bytes read during that check. State is bounded to
128 KiB, 64 artifacts, 32 external-job references and 128 recorded operations.
Capacity exhaustion refuses further continuation rather than dropping evidence.
Job references contain provider, scope, stable ID, non-secret result reference
and last observation. This version supplies an injected read-only query port
with shared cancellation/deadline, tested with a local fake adapter. No provider
adapter is bundled; without one, jobs remain `Unavailable` and need review.
CLAT never restarts an external job or adopts an old local process.

For headless use, create once with `clat exec --command '/task create ...'`, then
use `--continue --command '/task check'` and
`--continue --command '/task continue <id> <revision> <digest>'`. `--session`
can select a specific session instead. Check/show/save do not require a model;
continuation uses the normal headless event, cancellation and approval paths.
Non-interactive Ask decisions remain denied unless the user supplies `--yes`.
TUI and PWA use the same Application/command/event paths.


### Confirmed acceptance and evidence-backed completion

Structured acceptance is optional. Without it, `/task complete <id> <revision>`
records `human_declaration`, and older text conditions remain human conditions.
With a contract, that command requires verified completion or an explicit user
`override` with a note. An override records `human_override`; failed/inconclusive
reports keep their original results. The model cannot activate contracts, attest
human conditions, authorize rechecks or call an override.

Activate a contract while idle, using the current task ID/revision from
`/task show`. The first contract has `version:1`; replacement must advance by
exactly one, clears current human confirmations/candidates/recheck grants, and
retains historical reports. Copy each original text condition into a required
`human` clause named `legacy-1`, `legacy-2`, etc., with version 1, source `human`
and `parameters:{"text":"<original condition>"}`. These cannot silently become
machine checks. Example for a task created with an empty text acceptance list:

```text
/task acceptance <id> <revision> {"version":1,"allow_model_completion":false,"clauses":[{"id":"sections","required":true,"checker":"text","checker_version":1,"source":"project_files","files":["report.md"],"parameters":{"path":"report.md","contains":["Results","Total: 5"]}}]}
/task verify
/task verify-save <id> <revision>
/task verification-status
/task complete-verified <id> <revision> <saved-report-id>
```

`verify` only calculates; `verification-status [report-id]` only views the saved
report and recomputes its current applicability. Neither saves results, invokes
a model nor starts a command. `verify-save` explicitly persists a new report and
returns a new revision. Use that revision for the next write. Save and completion
are separate commits: completion must reference a previously durable report.
The headless form is `clat exec --continue --command '/task verify'` (or select
`--session`). TUI and PWA use the same commands and results.

Each clause has a stable `id`, `required`, `checker`, `checker_version`, `source`,
`files` and `parameters`. There must be at least one required clause. Installed
version-1 checkers are:

| Checker / source | Parameters and checked meaning |
| --- | --- |
| `human` / `human` | User description; `/task attest <id> <revision> <clause-id> <note>` confirms it explicitly. |
| `text` / `project_files` | `{"path":"report.md","contains":["Results"]}`; bounded UTF-8 text must contain every required literal. |
| `json` / `project_files` | `{"path":"result.json","checks":[{"op":"equals","id":"count","pointer":"/count","expected":2}]}`; the existing `verify_observation` JSON-pointer assertions (`equals`, `is_true`, `all_before`). |
| `records` / `project_files` | `{"input":"input.json","table":"table.json","report":"report.md","value_field":"amount","total_label":"Total:"}`; independently computes source row count and exact signed 64-bit sum. |
| `command` / `command_operation` | `{"argv":["cargo","test","selected_test"],"cwd":".","format":"rust-test-summary","minimum_executed":1}`; runtime-linked, complete command observation with declared input identities. |

For local checkers, `files` must exactly name the parameter paths. `records`
expects a source JSON array, a table object with `rows` equal to that entire
array, `row_count` equal to its length, and `total` equal to the independently
computed sum. Each source record must contain an integer `value_field` within
i64; fractional values or overflow are inconclusive. The UTF-8 report needs
exactly one line beginning with `total_label`, whose trimmed whole line equals
`<label> <computed sum>`. Add separate text/JSON clauses for additional sections
or fields. This does not infer requirements or certify source completeness.

Command clauses declare a nonempty `files` input scope. Obtain a receipt through
the normal native **`run_command` or non-PTY `exec_command`** argv invocation, with its existing
`verification:{"format":"…","minimum_executed":1}` option. Formats are
`rust-test-summary`, `playwright-summary` and `playwright-json`. The same argv,
project-relative `workdir` (or `.`), format and minimum must match the contract.
Pass an explicit operation map when checking/saving:

```text
/task verify {"tests":"op-<actual-operation-id>"}
/task verify-save <id> <revision> {"tests":"op-<actual-operation-id>"}
/task recheck <id> <revision> tests
```

Use the actual operation ID from `/task show` or `read_task`; no “latest receipt”
is guessed. Arbitrary JSON is not a command receipt. The gate binds the actual
process/cwd/argv, Task/contract/operation identity, exit status, cancellation,
EOF/complete scan, nonzero minimum, and declared file hashes before/after the
command and at checking time. Running, interrupted, incomplete or zero-test
observations cannot pass. Failed exit/test observations remain failed;
flaky/expected-failure or otherwise ambiguous observations remain inconclusive.
Ignored/skipped and other receipt metadata are retained. A Cargo-shaped stdout
signature remains an output observation: it cannot authenticate test coverage,
observer correctness or build freshness. Declare all relevant inputs and review
the command/conditions yourself. An `exec_command` result is eligible only when that invocation already returned
terminal, fully scanned output. A running launch remains inconclusive; later
`write_stdin` polls cannot replace its saved Task command fact or act as a new
producer. Use an explicitly authorized new invocation to obtain fresh complete
evidence. PTY adoption is not an acceptance source.

The prior-Run duplicate-effect fence remains active. Native commands record the execution identity from frozen preparation: actual
argv or shell text and resolved cwd, shared by `run_command` and `exec_command`.
Changing wrappers, timeout/format/output options, or equivalent workdir spelling
does not authorize replay. Shell requests share identity across the `command`
and `cmd` aliases, but shell text is not treated as semantically equivalent to
an arbitrary argv request or admitted as a receipt-checking rule. Earlier GA-2
relative-digest facts keep their fence through the matching confirmed contract;
if that old rule is unavailable, CLAT refuses to infer new execution authority.
Inspect and explicitly authorize a new declared check or start a new Task. After inspecting the prior effect, `recheck`
grants one new matching invocation; intent and grant consumption commit together.
Normal permission approval, unknown-operation fences and budget accounting still
apply. A failed, still-running or interrupted invocation does not restore a consumed
grant; polling it grants no new invocation.
Nothing automatically runs or retries a check.

During an explicitly continued Task Run, the model can use:

- `check_task_acceptance({"evidence":{...},"offset":0})`: read-only recomputation;
- `verify_task_acceptance({"task_id":"…","expected_revision":N,"evidence":{...}})`:
  compute and durably save a report;
- `propose_task_completion({"task_id":"…","expected_revision":N,"report_id":"…"})`:
  save only a pending candidate referencing a durable passing report.

Those tools are unavailable to ordinary Runs. `update_task` accepts progress
only. Large model results are bounded summaries (4 KiB); `read_task` accepts
`section:"operations"|"reports"|"contract"` and `offset` to page identities and
results. Follow `next_offset`; full confirmed rules remain in Task recovery
context and user `/task show`. Read-only check results also support clause pages.
Full durable reports are not replaced by the model summary.

Setting `allow_model_completion:true` is explicit user authorization for the
core to accept a candidate **after** the Task Run's journal, process cleanup,
budget settlement and Run-scope disposal succeed. The final gate rereads the
evidence and refuses stale or unresolved results. Cancellation, panic, cleanup,
settlement or persistence failure cannot leave a verified success. Reopening or
an unrelated ordinary Run never executes/promotes a saved candidate. With the
option disabled, the candidate stays pending for user confirmation. For Task Runs, exactly one final Run terminal agrees with the Application result:
final gates and required disposal run before it is published. An accepted Task
completion and the final `turn/end` share an atomic durable batch; a failed final
commit cannot publish success. Check Task provenance for the declared acceptance
scope; an ordinary completed Run without an accepted candidate still does not
claim the Task is complete.

Required failed clauses make the aggregate `failed`; otherwise any required
inconclusive clause makes it `inconclusive`. Missing/unsupported checkers and
versions keep their clauses and cannot pass. Human-only completion is labelled
`human_attested`, mixed human/machine completion `mixed_verified`, and machine
completion `verified`, with separate user/model authorization provenance. A
normal budget settlement changes the CAS revision without invalidating unchanged
evidence. Contract/attestation changes or changed input/output bytes make an old
report inapplicable. External changes after completion mark current evidence
stale; they do not rewrite historical results or reopen the completed Task.

Limits: 16 clauses, 8 files and 8 KiB parameters per clause, 32 KiB contract,
64 distinct files, 8 MiB per file and 32 MiB total reads including the second
stability read. Contract parameters have depth 32 / 4,096-node limits. JSON
assertion files are at most 256 KiB; records input/table/report at most
1 MiB / 2 MiB / 256 KiB, with 10,000 rows. Checks cooperatively observe cancellation
and a 250 ms deadline. Missing, restricted, changed-during-read or over-limit
sources yield inconclusive results. History holds at most 8 reports,
16 attestations and 32 single-use grants, within the overall 128 KiB Task cap;
exhaustion rejects a write without discarding history. Checks never follow
symlinks, read outside the held project capability, install software or use a
remote validation service.

## Inspect capabilities before choosing a tool

While idle, `/capabilities` lists registered tools. The same command works in
standalone/attached TUI, headless `clat exec --command`, and the PWA. Optional
JSON filters use the same core query as the model:

```text
/capabilities {"text":"测试","output":"command-receipt","scope":"local","limit":1}
/capabilities {"name":"read_file"}
/capabilities {"source":"mcp"}
/capabilities {"id":"<id returned by this snapshot>","schema_offset":0}
```

Models use `query_capabilities` and `describe_capability`. Their view contains
only the current Run's actual tool definitions, including Plan, modality, Task
and subagent fences. Read-only children keep their existing three-tool allowlist.
User diagnostics use the broader **idle-user** domain and are rejected during an
active Run. Viewing does not create a conversation, run tools, probe dependencies,
connect/reconnect services, install software, ask for approval or change a Task.
Normal model query calls/results still enter the ordinary conversation journal.

`name` is exact; `source` is an exact source kind, adapter or plugin owner.
`text` matches names, the first 16 KiB of descriptions and explicit labels:
ASCII case is ignored, whitespace-separated terms are ANDed, and Chinese terms
are literal substrings. There is no semantic search or automatic translation.
`input`, `output` and `scope` compare explicit declarations. An undeclared field
produces a candidate requiring confirmation; a declared mismatch excludes it.
These candidates carry reasons and missing conditions. They are suggestions,
with local/external/unknown boundaries preserved, never equivalent replacements,
execution evidence, authorization or a promise of eventual success. For example,
`write_file` can save supplied JSON/report text; it does not compute aggregates or
certify spreadsheet support. An external tool's unknown output is not PDF/XLSX
support. No confirmed path is a valid result.

Each item separates registration/visibility, handler availability, unknown
binary/dependency/configuration/login/health checks and invocation-time policy.
A native handler being available says nothing about a path or command. MCP
startup/handshake and WASM loading do not prove end-to-end readiness. Idle source
diagnostics show existing startup observations, including missing executable,
missing configuration, connecting or failure. They expose no raw startup command,
secret configuration or underlying error. Fix configuration/dependencies or login
explicitly, then use the existing plugin management reload; discovery performs
none of these actions. HTTP disconnects without an existing observation remain
unknown; this is not a health monitor.

Use `next_cursor` with the **same filters and limit** to continue. A different Run,
session selection, registry/lease generation or observed source state invalidates
old pages/ids; requery instead of treating historical text as live authority.
Actual calls still use normal preparation, approval, cancellation, budgets,
unknown-operation fences, one-use verification grants and acceptance gates.

Requests are at most 2,048 serialized JSON bytes; text is at most 256 UTF-8 bytes
and eight terms; filters/ids are at most 128 bytes; cursors at most 256. A page
contains at most four items (default two, possibly fewer to fit), and each reply
is at most 4,096 serialized JSON bytes. More than 4,096 registered visible entries
returns an explicit unsupported-size error. Source diagnostics show at most two
observations with `has_more`; filter by source/adapter to narrow them. Descriptions
are bounded previews with completeness flags; optional labels are bounded partial
declarations. Details return exact model input-schema JSON as text chunks with
`offset`, `next_offset`, `total_bytes` and `complete`; only an entire first chunk
can be `complete`. Each chunk is at most 1,024 bytes **after JSON escaping**;
schemas above 65,536 serialized bytes are explicitly omitted. Never execute using
a partial schema as though it were complete. Reissue list/detail after compaction
or cold reopen; no capability database or historical tool handle is restored.

Preview limits count JSON escaping too: name/owner/adapter 128 bytes,
source/scope 64, description/limitations 256, and each displayed label family
128 bytes in total. Label lists and limitations are explicitly partial previews;
use current model schema and producer documentation before a call. An observed
stdio reader EOF marks that adapter/channel blocked and expires old observation
ids. No new connectivity check is performed.

Schema annotations have recognized secrets removed before publication and
pagination. Model definitions and user/model details share that public schema;
validation constraints are preserved. A tool whose schema contains a recognized
secret in a property name or other structural data is refused at registration,
with a fixed diagnostic. Correct the producer definition and explicitly reload;
discovery cannot supply an altered schema for such a tool.
