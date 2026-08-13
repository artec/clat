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
