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
  turns, before each tool call, and between SSE chunks.

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

## Native read tools

CLAT ships three project-scoped read tools:

- `list_files` — inspect repository structure with depth and entry limits
- `read_file` — read UTF-8 files by line range with byte limits
- `search` — search project text files and return path/line/text matches

All paths are constrained to the current project root. Absolute paths,
`..` traversal, and paths that resolve outside the project are rejected.
Common generated and dependency directories such as `.git`, `node_modules`,
`target`, `dist`, and `build` are skipped by default.

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
