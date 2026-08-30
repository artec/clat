# Providers

Model vendors are adapters behind CLAT-owned interfaces. This document is for
maintainers and advanced users who need request, streaming, retry, or preset
details. Normal setup should begin with the [Model editor](model-editor.md).

## Protocol adapters

CLAT ships two provider-neutral protocol implementations.

### OpenAI Compatible

`OpenAiCompatibleModel` targets streaming `/chat/completions`-style APIs and is
the default for new custom profiles. Endpoint, request path, authentication,
headers, and body additions are configurable.

The adapter protects fields owned by the runtime—`model`, `messages`, `tools`,
and `stream`—from Extra Body overrides. It assembles incremental text,
reasoning, tool-call arguments, finish reason, and usage into typed
`ModelEvent`s.

### OpenAI Responses

`OpenAiModel` targets the `/responses` SSE API.

- Tool schemas carry their declared `strict` setting.
- Provider reasoning items are retained as opaque `provider_state` and replayed
  on later turns without leaking the wire format into `Run`.
- Managed request fields such as `model`, `stream`, `instructions`, and input
  items cannot be replaced by provider options.

Both adapters receive the same `ModelRequest`, tool definitions, cancellation
token, output limit, and event sink. The agent loop does not branch on vendor.

## Built-in presets

The preset catalog configures the OpenAI-compatible adapter:

| Vendor/preset | Model | Context | Max output | Reasoning default |
|---|---|---:|---:|---|
| DeepSeek V4.0 Flash | `deepseek-v4-flash` | 1M | 384K | `high` |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` | 1M | 384K | `high` |
| DeepSeek V4.0 Flash Vision (Exp) | `deepseek-v4-flash-vision-exp` | 1M | 384K | `high` |
| GLM 5.3 Coding Plan | `glm-5.3` | 1M | 128K | `high` |
| GLM 5.3 Flash | `glm-5.3-flash` | 1M | 128K | `high` |
| Qwen3.8 Max Token Plan | `qwen3.8-max` | 1M | 128K | `medium` |
| Kimi K3 Coding Plan | `kimi-k3` | 1M | 128K | `high` |

The context value also seeds automatic compaction. The output value bounds
request configuration and the aggregated response budget. User edits convert a
preset to Custom so the label cannot outlive its owned parameters.

### DeepSeek

The three presets use `https://api.deepseek.com` and explicitly send:

```json
{
  "thinking": { "type": "enabled" },
  "reasoning_effort": "high",
  "stream_options": { "include_usage": true }
}
```

Thinking mode ignores sampling fields such as temperature, so the presets leave
them unset. Usage parsing recognizes DeepSeek's cache-hit token field. The
Vision experimental preset uses the same protocol with native image input.

DeepSeek streams chain-of-thought as `delta.reasoning_content`. CLAT accumulates
it and attaches it only to an assistant item that made tool calls. On the next
request, the adapter emits one assistant message containing its text,
reasoning, and tool calls, followed by the matching tool results. Plain answer
turns drop reasoning replay because the API ignores it there.

### GLM Coding Plan

The preset uses the dedicated endpoint
`https://open.bigmodel.cn/api/coding/paas/v4`, not the generic platform URL. It
sends enabled preserved thinking:

```json
{
  "thinking": { "type": "enabled", "clear_thinking": false },
  "reasoning_effort": "high"
}
```

GLM 5.3 does not accept disabled reasoning. The status monitor reports Coding
Plan quota when the provider endpoint makes it available.

The GLM 5.3 Flash preset uses the same Coding endpoint with the
probe-verified text+image route, a 131,072-token output limit, and a 1M context
window. Its request policy sends at most five PNG/JPEG images of at most
5,000,000 bytes each. A newly submitted message outside that frozen policy is
rejected before provider I/O rather than partially sending its images. Across
history, core also limits one request to 12 image blocks and 20,000,000
normalized image bytes. When context pressure requires it, only older images
are replaced, in stable oldest-first order, with a fixed path-free notice; the
latest user turn is never silently degraded. Final adapter projection applies
the same boundary to storage/integrity failures: an unavailable current image
or current-turn tool image fails before network I/O with a path-free error,
while an unavailable historical image remains a visible path-free notice so a
damaged old session can still continue. Because this route also verified
direct image tool results, visual runs expose CLAT's fenced `view_image` tool;
text-only and unverified presets do not.

When this preset and its API key are active at project mount, the MCP adapter
also prepares the GLM Coding Plan server pack. See
[MCP integration](mcp.md#glm-coding-plan-pack).

### Qwen Token Plan

The preset uses the Singapore Token Plan endpoint and its subscription key. It
sends top-level `reasoning_effort` without a `thinking` object. CLAT maps its
Low / High / Max UI to Qwen's `low` / `medium` / `xhigh` ladder.

Context caching is implicit. Usage parsing accepts both
`prompt_tokens_details.cached_tokens` and the transitional top-level cached
token field. CLAT does not send explicit `cache_control`, whose availability
and miss pricing differ from this endpoint's documented implicit cache.

There is no public balance endpoint for the plan, so the status surface shows
cache and context but does not invent a Token segment.

### Kimi Coding Plan

The preset uses `https://api.kimi.com/coding/v1`, top-level
`reasoning_effort`, and automatic context caching. Low / High / Max map directly
to the vendor ladder.

The Coding membership endpoint currently filters clients by a coding-agent
User-Agent. The preset injects the field-verified compatible value
`claude-cli/2.1.161`; Extra Headers can override it. This is an interoperability
workaround rather than a core protocol requirement, and users should make
their own terms/compliance decision before relying on it.

The status monitor queries the provider's five-hour usage window and displays
the remaining percentage when available.

## Reasoning control

`ThinkingLevel` is provider-aware:

| CLAT level | DeepSeek / GLM / Kimi | Qwen |
|---|---|---|
| Low | `low` | `low` |
| High | `high` | `medium` |
| Max | `max` | `xhigh` |

Known endpoint domains receive this mapping even for a custom profile. Unknown
domains do not receive an inferred field. The raw Extra Body remains the
escape hatch for local gateways or other vendors.

Reasoning deltas are observable live, but private reasoning is not rendered as
ordinary assistant text. Provider replay state persists only when required for
correct subsequent requests.

## Response resource budgets

Transport and aggregation limits prevent a hostile or broken endpoint from
exhausting memory:

| Surface | Limit |
|---|---:|
| one SSE line | 1 MiB |
| provider error body | 64 KiB |
| aggregated response | output tokens × 64 bytes, clamped to 1–64 MiB |

The response ceiling scales with the configured output limit so legitimate
large outputs are not subject to an unrelated flat cap. Crossing any limit
returns a typed provider error.

Journal zstd frames have their own 64 MiB decoded budget; see
[Persistent state](storage.md#session-journals).

## Retry policy

Every model call is built through a `ModelFactory` and wrapped by `RetryModel`.
Each attempt creates a fresh adapter instance, so connection state is not reused.

Typed errors control retry:

| Error class | Retry? |
|---|---|
| transport | yes |
| rate limited | yes |
| server | yes |
| decode | no |
| authentication | no |
| invalid request | no |
| other | no |

`Retry-After` on 429/503 is honored up to 30 seconds. Once any stream event has
been emitted to the caller, the attempt is no longer retryable; resending would
duplicate output or tool calls the user already observed.

Factory failures follow the same typed classification.

## Deadlines and attempt caps

Normal interactive agent turns have no total deadline; the user cancellation
token is their outer lifetime. Internal model consumers use explicit budgets:

| Consumer | Total deadline | Underlying attempt cap |
|---|---:|---:|
| automatic session title | 15 s | 2 |
| compaction map/reduce | 60 s | 8 |

A deadline is attached to a child `CancelToken`, not only to retry sleep. It
therefore bounds connection setup, response headers, a silent SSE body, and
backoff together. Provider connect/header/global timeouts are clamped to the
remaining time.

Map/reduce consumers share one retry wrapper, so the attempt cap covers the
whole operation instead of resetting for each subrequest.

## Cancellation

Providers check the shared token between SSE reads and before decoding a
completed tool call. Cancelling a compatible stream discards incomplete tool
arguments instead of reporting misleading malformed JSON.

Parent cancellation always wins over an internal deadline. Run cancellation
also propagates to native tools, MCP requests, WASM execution, and plugin-host
sampling so the provider layer is not an isolated cancellation island.

## Usage accounting

Provider usage is normalized into input, output, optional cache-read, and
reasoning token counts. It drives:

- live frontend telemetry;
- per-run spend reconciliation;
- durable session statistics;
- context estimates used for compaction;
- usage attributed to extension sampling.

Malformed main token fields are treated as absent, leaving the conservative
pre-request reservation in place. Values are clamped to a sane numeric domain
before arithmetic, so a custom endpoint cannot wrap the ledger with an absurd
report.

## Adding a provider

A new provider should normally implement a `ModelFactory` and `Model` adapter,
then register it through the Provider registry. Keep vendor request/response
types inside the adapter. Do not add vendor matches to `Run`, frontends, or
session projections.

Before adding a preset, verify model id, endpoint, request path, context window,
output limit, reasoning parameters, usage shape, and authentication semantics.
The preset application path and editor must continue to share one catalog
entry so displayed and executed values cannot drift.
