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
| Hy 4 Preview · Hy Token Plan | `hy4-preview` | 1M | 64K | — (always on) |

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

### Tencent Hy Token Plan

The preset uses `https://api.lkeap.cloud.tencent.com/plan/v3` with `hy4-preview`
— the subscription endpoint for Tencent's own Hunyuan models (the separate
"general" Token Plan aggregating third-party open models is deliberately not
covered; those vendors already have direct presets). Generate the key in the
TokenHub console; the same key and URL serve both plans and deduct by model id.

All parameters are pinned by the TC-0 live probe
(`docs/research/tc0-probe/`, 2026-09-02):

- Thinking is always on server-side: every response carries
  `reasoning_content`, and a `thinking: {"type":"disabled"}` request is silently
  ignored. `reasoning_effort` shows no reproducible effect (invalid values are
  also accepted without error), so the preset sends neither field and offers no
  `Shift+Tab` ladder.
- `max_tokens` is enforced (a 64-token request finishes with `length` at
  exactly 64) but has no schema upper bound; the longest observed natural
  completion is 44,240 tokens, so the output limit is pinned at 65,536.
- The context window follows the official 1M total-window figure (the same
  pinning style as GLM). The interface decomposes it into roughly 960K input
  plus 64K output, which is exactly what the probe observed: prompts of
  929,775 and 958,177 tokens were served while a ~1M-token prompt alone fails
  with engine error `20057` rather than a clean context rejection. Pinning
  rule (owner ruling, TC-2): official documentation wins; probes only verify
  surprises — a single-sample anomaly must not override a documented claim.
- The preset's vendor level reads "Hy Token Plan", matching the plan-name
  pattern of the other subscription presets; vendor detection and the key
  slot still resolve to Tencent via the endpoint domain.
- Streaming usage rides every SSE chunk (real values on the final one), so no
  `stream_options` is sent. CLAT drops the all-zero intermediate usage
  payloads (`prompt_tokens == 0 && completion_tokens == 0` carries no
  information and would overwrite the status bar's context watermark during
  streaming); a single real usage or a partial-zero one still passes through.
- Tool calling follows the OpenAI shape, including the `index` field on
  streamed `tool_calls`.
- The model is text-only, and the endpoint **silently drops** image content
  parts — it answers blind with HTTP 200 instead of rejecting. CLAT's
  text-only capability gate is therefore the enforcing side; do not bypass it
  via a custom profile expecting image support.
- Error signatures: bad key → `401 not_authorized`; unknown model →
  `400 code 20033`.

**Terms-of-use note**: the Hy Token Plan key is, per Tencent's usage terms,
restricted to *interactive use inside AI tools*; automated scripting and
custom application backends are outside the terms and risk key revocation.
Interactive CLAT sessions (TUI, workbench) fit that intent. Headless `clat
exec` in CI or scripted automation is a gray zone — you alone are responsible
for complying with the plan's terms.

**Plan balance**: Tencent does expose `DescribeTokenPlan`
(`tokenhub.tencentcloudapi.com`), which reports the plan's remaining quota —
but it is a Tencent Cloud API 3.0 control-plane call that requires an
account-level TC3 signature (SecretId/SecretKey) plus the plan's TeamId. The
plan's Bearer API key cannot call it (probe-pinned
`AuthFailure.InvalidAuthorization`), and the `/plan/v3` data plane has no
balance route. CLAT therefore shows no quota segment for this vendor rather
than fabricating one; the status surface keeps the usage-derived cache and
context facts. Check remaining points in the
[TokenHub console](https://console.cloud.tencent.com/tokenhub/tokenplan).
Concurrency limits scale with the plan tier and may tighten at peak hours.

## Reasoning control

`ThinkingLevel` is provider-aware:

| CLAT level | DeepSeek / GLM / Kimi | Qwen | Tencent Hy |
|---|---|---|---|
| Low | `low` | `low` | — |
| High | `high` | `medium` | — |
| Max | `max` | `xhigh` | — |

Tencent Hy exposes no effective reasoning control (see above); the ladder is
empty, `Shift+Tab` has no effect on that vendor, and the title bar shows
`Thinking · Server` so the always-on state stays visible.

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
