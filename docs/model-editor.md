# Model editor (`/model`)

The model editor opens on a short form: **Preset**, **Model**,
**Endpoint**, and **API Key** — the four things a normal user needs.

Press `Enter` (or click, or just start typing) on any field to open a
small input box — `Enter` confirms, `Esc` cancels. `Ctrl+S` or the
`[ Save ]` row saves; `Esc` cancels the editor.

## Presets

Presets ship with official provider parameters. `←` / `→` or `Enter` on
the Preset row cycles through Custom → the built-ins → Custom:

| Preset | Model | Endpoint |
|---|---|---|
| DeepSeek V4.0 Flash | `deepseek-v4-flash` (V4-Flash-0731) | `https://api.deepseek.com` |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` (V4-Pro-0813) | `https://api.deepseek.com` |

Both use the official OpenAI-compatible API with a 384K output limit and
`reasoning_effort: high` (the official default; DeepSeek thinking mode
ignores `temperature`, so presets leave it unset). Picking a preset fills
Model, Endpoint, and the request parameters; the API Key is never
touched.

Editing Model, Endpoint, or Protocol by hand marks the configuration as
Custom, so the preset label never lies about what is active.

## DeepSeek reasoning content

DeepSeek's chain of thought (`reasoning_content`) is handled explicitly:
CLAT captures it from the stream, persists it with the conversation, and
replays it on assistant messages that carry tool calls — the exact shape
DeepSeek requires for multi-turn tool use — while dropping it on plain
answer turns, where the API ignores it and sending it would only waste
tokens. See [providers](providers.md) for the wire-level details.

## Advanced fields

The `[ Advanced ]` row reveals fields for custom setups only when needed:

- Protocol — `OpenAI Compatible` (default) or `OpenAI Responses`
- Request Path (default `/chat/completions` for compatible providers,
  `/responses` for Responses)
- Auth Header (default `Authorization`)
- Auth Prefix (default `Bearer `)
- Extra Headers JSON, for example `{"X-Tenant":"acme"}`
- Extra Body JSON, for example `{"top_p":0.9}`
- Max Output Tokens
- Temperature
- Parallel Tool Calls

`OpenAI Compatible` is the default for a fresh installation so CLAT is
not tied to the official OpenAI endpoint.
