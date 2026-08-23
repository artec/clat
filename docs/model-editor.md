# Model editor (`/model`)

`/model` opens a picker: the four built-in provider vendors plus
**Custom**. Custom models are **named profiles** — each profile persists
its own configuration *and* its own API key, so switching between
profiles (or between a profile and a preset) never loses anything.

- **No profiles yet**: selecting Custom goes straight to the new-profile
  page (a blank template with safe defaults).
- **One or more profiles**: Custom shows the profile list (name plus an
  `endpoint · model` summary; the active profile is marked `●`). `Enter`
  switches and closes, `e` edits, `d` deletes (press `d` twice to
  confirm), and the `New…` row at the bottom starts a blank template.

Going back a level (`Esc`) always restores the cursor to the row you
entered from, and cancelling an editor opened from the picker returns
to the picker in place — one `Esc` never collapses the whole selection
chain.

The profile editor never shows the Preset row (profiles are by
definition custom), and every numeric parameter is a choice, not a blank
box, listed small to large: **Context Window** (128K default / 256K /
1M / Custom…), **Max Output** (8K / 32K default / 128K / Custom…), and
**Spend Budget** (1M / default 10M / 50M / off / Custom…). `Enter` or
`←`/`→` cycles; the `Custom…` position opens a numeric input. **Thinking** is a choice
too (low / **high (default)** / max / off) and is stored with the
profile, matching the built-in presets which all ship
`reasoning_effort: high`; `off` sends nothing and follows the vendor
default. The level is injected automatically when the endpoint is one
of the four known vendor domains; on unknown endpoints it is stored but
never injected (strict gateways reject unknown parameters) — write the
vendor-native parameter into Extra Body there instead. `Shift+Tab`
stays a live adjustment of the running configuration: while a profile
is active it changes the current model's level but not the stored row —
the `●` drops until you switch away and back (the row's saved level
returns with it); to keep a level, set it in the profile editor. Fill
in **Name**, **Model**, **Endpoint** (an API key is optional — local
gateways don't need one), save, and the profile becomes active
immediately. Deleting the active profile falls back to the first
remaining profile, or to factory defaults when none are left. Upgrades
migrate an existing single-slot custom configuration into the first
profile automatically.

For preset configurations, the editor opens on a short form: **Preset**,
**Model**, **Endpoint**, and **API Key** — the four things a normal user
needs.

Press `Enter` (or click, or just start typing) on any field to open a
small input box — `Enter` confirms, `Esc` cancels. `Ctrl+S` or the
`[ Save ]` row saves; `Esc` leaves the editor and returns to the picker
position you came from (at the top level it closes the dialog). Every
model dialog pins its key-hint bar to the bottom edge in the same dim
gray, separated from the content by one blank line.

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
- Spend Budget — the per-run token guardrail (`input+output`; cache hits
  count inside input, never twice). Empty = the 10M default; `0` disables
  it (not recommended). When a run crosses 50% and 90% of the budget a
  durable warning event is journaled (`clat/budget`); crossing the cap
  stops the run with a three-part error (used / cap / raise via /model).
  The budget is per model configuration, so an expensive model can carry
  a tighter cap than a cheap one
- Temperature
- Parallel Tool Calls

`OpenAI Compatible` is the default for a fresh installation so CLAT is
not tied to the official OpenAI endpoint.
