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

## Cancellation

Providers poll the shared `CancelToken` between SSE chunks and stop
promptly when it is set; a cancelled compatible stream discards
half-received tool-call arguments instead of failing on partial JSON.
