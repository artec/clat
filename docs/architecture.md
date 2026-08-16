# Architecture

## Core

The runtime owns the execution loop and core abstractions. Model providers
and external protocols such as MCP remain adapters around that core.

```text
Run
├── Project
├── Model
├── ToolRegistry
├── PermissionPolicy
└── EventSink
```

- `Run` drives the streaming model → tool → model loop, capped at a
  configurable number of turns.
- Every observable step (`ModelRequested`, `ModelStream`, `ToolRequested`,
  `ToolFinished`, `PermissionChecked`, `RunCompleted`, …) is emitted through
  an `EventSink`, so CLI, TUI, IDE, desktop, or remote clients consume the
  same runtime events.
- Tool execution failures are returned to the model as structured error
  results instead of aborting the run, so the agent can recover.
- Cancellation is cooperative: a shared `CancelToken` is polled between
  turns, before each tool call, between SSE chunks, and — via
  `Tool::invoke` — inside long-running tool waits (an in-flight MCP
  request aborts within ~25 ms and notifies the server).

## Agentic loop (v0.3.4)

CLAT is now a **minimal viable agent**: the read → change → verify loop is
closed. What the agent can do autonomously:

```text
inspect (list/read/search)
   → locate a problem
      → change it (edit_file / write_file)
         → verify (run_command: build, test)
            → read the failure → change again → until green
```

Current boundaries of that capability, and what is deliberately **not**
built yet:

| Capability | State |
|---|---|
| Multi-turn run with cancellation and failure persistence | done |
| Read tools (list/read/search), project-scoped | done |
| Write/edit tools with capability-relative path binding | done |
| Command execution with process-tree ownership, timeout, output budget | done |
| Permission review for every side-effecting call | done |
| Project context injection (CLAT.md, git state into the system prompt) | not built — the system prompt is a fixed string today |
| Context management (compaction/truncation for long sessions) | not built — `message_items` grows unbounded |
| Turn budget configurability | not built — fixed 32 turns, exceeding it fails the run |
| Headless mode (`clat exec "prompt"`) | not built — the core is client-neutral but only the TUI drives it |
| Subagents, image input, multi-agent orchestration | deferred by constitution |

The intended growth order is: project context injection → context
management → headless mode, each driven by real dogfood need.

## Model Protocol v0.1

CLAT models exchange typed, provider-neutral data:

```text
ModelRequest
├── instructions
├── ModelItem[]
├── ToolDefinition[]   (JSON Schema)
├── ModelOptions
└── CancelToken
       │
       ▼
Model::stream(...)
       │
       ├── TextDelta
       ├── ToolArgumentsDelta
       ├── ToolCallCompleted
       ├── ReasoningDelta
       ├── ReasoningSummaryDelta
       ├── Usage
       └── ResponseCompleted
       │
       ▼
ModelResponse
├── text
├── tool_calls[]
├── finish_reason
├── usage
├── provider_state
└── reasoning
```

Provider-specific state can be preserved for subsequent turns without
leaking provider wire formats into `Run` (the OpenAI Responses reasoning
items travel through `provider_state`, for example).

## Conversation model

A conversation is an ordered list of `ModelItem`s:

- `User` / `Assistant` — text turns; assistant items may carry reasoning
  (chain-of-thought) that providers requiring replay can read back
- `ToolCall` / `ToolResult` — tool interactions, paired by call id
- `ProviderState` — opaque provider data preserved across turns

The item list is the source of truth for model context and is persisted
verbatim (see [storage](storage.md)).

## Native tools

CLAT ships project-scoped read tools:

- `list_files` — inspect repository structure with depth and entry limits
- `read_file` — read UTF-8 files by line range with byte limits
- `search` — search project text files and return path/line/text matches

and, for trusted projects, three side-effecting tools that close the
agentic loop (read → change → verify):

- `write_file` — create or overwrite files atomically (temp file +
  rename; a failed write never leaves partial content); existing file
  permissions are preserved
- `edit_file` — replace one exact, unique text snippet; ambiguous or
  missing matches error without touching the file; the commit
  re-verifies the read snapshot under a parent-directory lock, so
  cooperating CLAT writers conflict instead of overwriting each other
- `run_command` — run a shell command in the project root; termination
  covers the whole process tree (Unix process group TERM → unconditional
  KILL after a fixed grace; Windows Job Object), output past the 32 KiB
  cap is drained without changing command semantics; `exit_code` /
  `signal` / `timed_out` form the result contract

All paths are constrained to the current project root. Absolute paths and
`..` traversal are rejected. Reads reject paths that resolve outside the
project; writes perform parent creation, inspection, temporary-file creation,
and rename relative to opened capability directory handles, so a symlink or
concurrent directory replacement cannot retarget later I/O outside the root.
Common generated and dependency directories such as `.git`, `node_modules`,
`target`, `dist`, and `build` are skipped by default. Side-effecting
tools pass through the permission model on every call — see
[permissions](permissions.md) and the audit trail under
[docs/audit](audit/) for the adversarial review these guarantees went
through.

## Project trust

The trust gate is part of the startup state machine, not a UI overlay:

```text
App::new (minimal)
├── open global storage, query trust table
└── untrusted → render trust dialog; no session, no project reads,
                no MCP subprocesses
       │ Enter/y
       ▼
initialize_project
├── session, messages, input history, model config
└── MCP servers (cwd fixed to ~/.clat)
```

Trust is persisted per canonical directory path. `session_id` is an
`Option` until initialization succeeds, so every conversation path is
structurally gated on trust.

## MCP adapter

MCP servers are adapters around the core: a `StdioSession` (single
reader thread with id-routed responses, bounded writer queue, per-call
deadlines) hosts the subprocess; `McpTool` adapts each remote tool into
the core `Tool` trait. See [MCP integration](mcp.md) for the protocol
posture, naming rules, and resource limits.

## TUI event loop

The TUI is fully event-driven: a dedicated input thread forwards
terminal events, model workers report through the same channel, and a
balance monitor thread refreshes DeepSeek/GLM quotas every five minutes.
The main loop suspends on a bounded receive with the next repaint
deadline (spinner frame, status flash expiry) and wakes only on real
work — no polling intervals anywhere.
