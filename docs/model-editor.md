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
| DeepSeek V4.0 Flash Vision (Exp) | `deepseek-v4-flash-vision-exp` | `https://api.deepseek.com` |
| GLM 5.3 | `glm-5.3` | `https://open.bigmodel.cn/api/coding/paas/v4` |
| Qwen3.8 Max | `qwen3.8-max` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` |
| Kimi K3 | `kimi-k3` | `https://api.kimi.com/coding/v1` |

The DeepSeek presets share the official OpenAI-compatible API with a 1M
context window and a 384K output limit, and set
`reasoning_effort: high` (thinking mode ignores `temperature`, so
presets leave it unset). **Flash Vision (Exp)** is the experimental
multimodal entry — the first preset that reads image input, which pairs
with CLAT's image attachments (images are billed as tokens by their
dimensions). The GLM 5.3 preset targets the dedicated Coding Plan
endpoint with a 128K output limit, preserved thinking
(`clear_thinking: false`), and `reasoning_effort: high` as well.
GLM 5.3 cannot disable thinking (the API rejects `disabled`), while
DeepSeek's non-thinking mode stays available through the raw extra
body. Picking a preset fills Model, Endpoint, and the request
parameters; the API Key is never touched.

Editing any preset-controlled field by hand — Model, Endpoint,
Protocol, Request Path, Extra Body, Max Output Tokens, Temperature, or
Parallel Tool Calls — marks the configuration as Custom, so the preset
label never lies about what is active (and preset defaults never
overwrite your saved values on the next run). Editing the Extra Body
also clears the `Shift+Tab`-saved thinking level: the raw body becomes
the source of truth.

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
- Context Window — the auto-compact budget in tokens; once set, history
  beyond it is compacted automatically at the start of the next run
- Temperature
- Parallel Tool Calls

`OpenAI Compatible` is the default for a fresh installation so CLAT is
not tied to the official OpenAI endpoint.
