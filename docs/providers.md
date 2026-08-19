# Providers

Model vendors are adapters behind CLAT-owned interfaces; no provider
defines the core runtime.

## OpenAI Responses

`OpenAiModel` targets the `/responses` endpoint over SSE. Notable
details:

- Tool schemas are sent with `strict` as declared by the tool.
- Reasoning items are preserved across turns through `provider_state`,
  so the provider wire format never leaks into `Run`.
- Managed request fields (`model`, `stream`, `instructions`, …) are
  protected: provider options that would override them are rejected.

## OpenAI Compatible

`OpenAiCompatibleModel` targets streaming `/chat/completions`-style APIs
and is the default protocol, so CLAT works with DeepSeek, local
gateways, and other OpenAI-shaped providers.

- Endpoint, request path, auth header/prefix, extra headers, and extra
  body are all configurable through `/model`'s advanced fields.
- Managed body fields (`model`, `messages`, `tools`, `stream`) are
  protected against overrides.

### Built-in presets (2026-08)

Four vendors ship official presets (pick by name in `/model`; only an
API key is left to fill):

- **DeepSeek** (`api.deepseek.com`): `thinking {type: enabled}` +
  `reasoning_effort`, `stream_options.include_usage` for live cache
  stats. Implicit prompt caching; usage reports
  `prompt_cache_hit_tokens`.
- **GLM Coding Plan** (`open.bigmodel.cn/api/coding/paas/v4`): as
  DeepSeek plus preserved thinking (`clear_thinking: false`); usage
  rides the stream by default.
- **Qwen Token Plan**
  (`token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`,
  dedicated subscription key): top-level `reasoning_effort` with the
  official ladder `low`/`medium`/`xhigh` (CLAT's Low/High/Max tiers map
  onto them; no `thinking` object). Context caching is implicit and
  cannot be disabled (~20% hit discount); `include_usage` brings back
  `prompt_tokens_details.cached_tokens` (some Singapore deployments
  report the top-level `cached_tokens` transitional field — both are
  parsed). Explicit `cache_control` marking is deliberately not sent:
  its availability on the Singapore Token Plan endpoint is not
  documented and misses cost 125% of input price.
- **Kimi Coding Plan** (`api.kimi.com/coding/v1`, subscription
  membership): top-level `reasoning_effort` (`low`/`high`/`max`, no
  `thinking` object); context caching is fully automatic (≥256-token
  prefix). The endpoint gates traffic by a User-Agent whitelist
  (Kimi CLI / Claude Code / Roo Code / Kilo Code …) and returns 403
  for unknown clients, so the preset injects the field-verified
  whitelisted UA `claude-cli/2.1.161` (cc-switch PR #3671's fallback
  constant; their measured whitelist: `claude-cli/*`, `claude-code/*`,
  and `Kilo-Code/*` pass, while `codex-cli/*`, `kimi-cli/*`, and
  `openclaw/*` are rejected). This sits in a gray zone of the
  subscription terms — override it via `/model` → Extra Headers if you
  want a different choice. The status bar's `Token` segment shows the 5-hour-window
  remaining percentage from `GET /coding/v1/usages` (same endpoint as
  cc-switch's Kimi quota query; the request carries the whitelisted
  UA). Qwen Token Plan has no public balance API (credits are visible
  only in the console's subscription page), so its status line carries
  Cache/Context without a Token segment.

### Reasoning replay (DeepSeek)

DeepSeek thinking mode streams chain-of-thought as
`delta.reasoning_content` and requires it to be replayed on assistant
messages that carry tool calls. CLAT:

1. captures `reasoning_content` chunks from the stream,
2. attaches the accumulated reasoning to the turn's assistant item when
   (and only when) the turn makes tool calls — plain answer turns drop
   it, since the API ignores it there and sending it would waste tokens,
3. groups a turn's assistant text, reasoning, and all of its tool calls
   into one assistant message followed by the turn's tool results — the
   official replay shape.

## Retry and deadlines

Every model call the agent makes goes through a factory-backed `RetryModel`
wrapper. Each attempt builds a fresh model instance from the provider
factory, so no connection state is reused across attempts.

Retry decisions come from the typed `ModelError` classification:

- `Transport`, `RateLimited`, `Server` are retriable.
- `Decode`, `Auth`, `Request`, `Other` fail immediately — retrying a decode
  or auth error can only produce the same failure.
- Factory (constructor) failures follow the same rule: transient typed
  errors retry, everything else surfaces.
- **Event safety**: once an attempt has emitted any stream event to the
  caller, the failure is never retried — a resend would duplicate model
  output the user already saw.

`Retry-After` hints from 429/503 responses are honored, capped at 30s.

**Total deadlines are request capabilities, not just backoff budgets.**
When a retry policy carries a `total_deadline` (internal consumers always
do), the deadline is attached to the request's `CancelToken`:

- the token reports itself cancelled at expiry, so SSE body polling stops
  even when the server goes silent after sending headers;
- providers clamp their HTTP timeouts (`timeout_global`, connect, and
  response-header wait) to the remaining time, so connection setup and
  header waits are bounded by the same deadline — the provider's default
  30s/60s timeouts never override a shorter internal deadline;
- a parent cancellation (user `Esc`, run cancel, scope close) short-circuits
  immediately; it never waits out the deadline.

A second policy field, `total_attempt_cap`, bounds the number of underlying
HTTP/factory attempts across the wrapper's whole lifetime — map/reduce
style consumers share one wrapper so the cap covers every request.

Internal consumers:

| Consumer | Deadline | Attempt cap | Notes |
|---|---|---|---|
| Session titles | 15s | 2 | background worker; cancelled on close |
| Compaction summaries | 60s | 8 | one shared wrapper across map and reduce rounds |

Normal agent turns run without a total deadline and remain bounded only by
the user's cancellation token.

## Cancellation

Providers poll the shared `CancelToken` between SSE chunks and stop
promptly when it is set; a cancelled compatible stream discards
half-received tool-call arguments instead of failing on partial JSON.
