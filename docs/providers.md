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
| DeepSeek V4.1 Flash | `deepseek-flash` | 1M | 384K | `high` |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` | 1M | 384K | `high` |
| GLM 5.4 Coding Plan | `glm-5.4` | 1M | 128K | `high` |
| GLM 5.3 Coding Plan | `glm-5.3` | 1M | 128K | `high` |
| GLM 5.3 Flash | `glm-5.3-flash` | 1M | 128K | `high` |
| Qwen4 Max Token Plan | `qwen4-max` | 1M | 128K | `medium` |
| Qwen3.8 Flash Token Plan | `qwen3.8-flash` | 1M | 128K | `medium` |
| Kimi K3.1 Pro Coding Plan | `k3.1-pro` | 1M | 128K | `high` |
| Kimi for Coding · Kimi Coding Plan | `kimi-for-coding` | 1M | 128K | `high` |
| Hy 4 · Hy Token Plan | `hy4` | 1M | 64K | — (always on) |
| Hy 4 Flash · Hy Token Plan | `hy4-flash` | 1M | 64K | — (always on) |
| DeepSeek V4 Pro · OpenCode Go | `opencode-deepseek-v4-pro` | 1M | 384K | `high` |
| DeepSeek V4.1 Flash · OpenCode Go | `opencode-deepseek-v4.1-flash` | 1M | 384K | `high` |
| Kimi K3.1 Pro · OpenCode Go | `opencode-kimi-k3.1-pro` | 1M | 256K | `high` |
| Kimi K3 · OpenCode Go | `opencode-kimi-k3` | 1M | 256K | `max` |
| GLM 5.3 · OpenCode Go | `opencode-glm-5.3` | 1M | 128K | `high` |
| GLM 5.3 Flash · OpenCode Go | `opencode-glm-5.3-flash` | 1M | 128K | `high` |
| Qwen4 Max · OpenCode Go | `opencode-qwen4-max` | 1M | 262K | `high` |
| MiniMax M3 · OpenCode Go | `opencode-minimax-m3` | 1M | 128K | — (toggle only) |
| Claude Haiku 5.5 · OpenCode Go | `opencode-claude-haiku-5-5` | 1M | 128K | `medium` |
| GPT-6 Luna · OpenCode Go | `opencode-gpt-6-luna` | 1.05M | 128K | `medium` |
| Gemini 4 Argon · OpenCode Go | `opencode-gemini-4-argon` | 1M | 64K | `medium` |
| Grok 4.7 · OpenCode Go | `opencode-grok-4.7` | 500K | 256K | `medium` |

Vision capability is hardcoded per preset, never probed at runtime: the
image-input routes are `deepseek-flash`, `qwen4-max`,
`qwen3.8-flash`, `k3.1-pro`, `kimi-for-coding`, and `hy4`, plus
`glm-5.3-flash` and `glm-5.4` (both verified for image tool results; 5.4
reuses the Flash vision matrix) — the vendor-declared
routes follow official vendor documentation (Hy 4 per the 2026-11-07 owner
ruling), the GLM Flash route is proven by CLAT's own live probe, and the ten
OpenCode Go vision routes (`opencode-deepseek-v4.1-flash`,
`opencode-kimi-k3.1-pro`, `opencode-kimi-k3`, `opencode-glm-5.3-flash`,
`opencode-qwen4-max`, `opencode-minimax-m3`, `opencode-claude-haiku-5-5`,
`opencode-gpt-6-luna`, `opencode-gemini-4-argon`, `opencode-grok-4.7`)
follow the gateway's own `/v1/models` metadata snapshot. The vendor-declared
visual presets
use a conservative per-request policy of eight images, at most 4 MiB per image;
custom profiles default to the same limits once image input has been enabled
by a successful `/vision-probe`. All other presets are text-only.

Kimi note (2026-11-03): the Coding-plan flagship was upgraded in place from
`kimi-k3` to `k3.1-pro` (K3.1 Pro). The wire id is `k3.1-pro` — the
open-platform spelling `kimi-k3.1-pro` does not apply to the coding
endpoint, and the endpoint silently falls back to a non-thinking K2.8
Preview on unknown ids, so no compatibility alias is kept. Saved
configurations referencing `kimi-k3` simply stop resolving to a preset.
The highspeed variant and the overseas `api.kimi.ai/coding/v1` endpoint are
deliberately not presetted.

DeepSeek note (2026-09-10): the official V4.1 Flash release replaced the
retired `deepseek-v4-flash` and experimental `deepseek-v4-flash-vision-exp`
models; the preset was upgraded in place to `deepseek-flash` with the V4.0
Flash parameters carried over and the vision slot inherited. Saved
configurations still referencing the old ids are not migrated — they simply
stop resolving to a preset.

The context value also seeds automatic compaction. The output value bounds
request configuration and the aggregated response budget. User edits convert a
preset to Custom so the label cannot outlive its owned parameters.

### DeepSeek

Both presets use `https://api.deepseek.com` and explicitly send:

```json
{
  "thinking": { "type": "enabled" },
  "reasoning_effort": "high",
  "stream_options": { "include_usage": true }
}
```

Thinking mode ignores sampling fields such as temperature, so the presets leave
them unset. Usage parsing recognizes DeepSeek's cache-hit token field. Native
image input is officially declared on `deepseek-flash` itself, so attach
admission is open on that route.

DeepSeek streams chain-of-thought as `delta.reasoning_content`. Core retains
that original content on every assistant item, including plain answers.
Following the [official thinking-mode guide](https://api-docs.deepseek.com/guides/thinking_mode/),
requests to the official endpoint carrying tools replay reasoning from every
loaded assistant turn, even when that turn did not call a tool. Tool calls
remain grouped with their assistant message and followed by the matching
results. Without tools, plain-answer reasoning is omitted. Missing historical
reasoning is never synthesized.

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

Following the [official preserved-thinking guide](https://docs.z.ai/guides/capabilities/thinking-mode),
CLAT returns complete, unmodified reasoning from all loaded assistant turns
when preserved thinking is active, including answers without tool calls.
The official Coding endpoint enables this by default; an effective
`thinking.clear_thinking: true` disables plain-answer replay. Request overrides
are evaluated after merging configuration, so the replay policy follows the
parameters actually sent.

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
text-only presets do not.

When this preset and its API key are active at project mount, the MCP adapter
also prepares the GLM Coding Plan server pack. See
[MCP integration](mcp.md#glm-coding-plan-pack).

### Qwen Token Plan

The presets use the Token Plan endpoint and their subscription key. Since
2026-11-20 the settings page offers a **Token Plan entry** radio on the Qwen
card: **Aliyun Bailian** (the Singapore MaaS endpoint, the default) or
**Qianwen AI Platform** (`token-plan.maas.qianwenaiapi.com`, the
`qianwenai.com` subscription entry; live-verified to serve both presets with
the same parameter surface). One preset pair, one key slot — the radio only
switches the endpoint. Switching while a Qwen Token Plan model is active
takes effect immediately and keeps your tuning overrides; on an inactive card
the choice rides the next **Apply**. Applying the preset without an explicit
entry keeps the currently selected entry (the radio is the single source of
truth; only an explicit entry overrides it), and switching entries accepts a
newly typed key in the same step — the key field value is submitted with the
radio, since one platform's key is rejected by the other (identical 401
bodies, so an entry/key mismatch is easy to misread). Re-selecting the preset
later — even after switching to another vendor or restarting — restores the
entry from the remembered key slot: the `sk-sp-` prefix identifies the
Qianwen AI Platform key and re-pairs the Qianwen endpoint; any other key
shape falls back to the Aliyun default. The settings radio mirrors this
restored entry (`model.settings.get` reports it as `qwen_entry`), so the
selected option matches the endpoint actually in effect. Console-credential quota (2026-11-22): on the Qianwen entry the
quota icon in the left rail can sign in through the platform's device flow —
click the ring with the key glyph, approve in the browser, and the per-window
usage percentages appear (credentials are stored locally, encrypted at rest
with an embedded key; the model API key is never involved). The Aliyun
entry works the same way through its own console sign-in (2026-11-22): the
browser calls back to a local listener on this machine and hands CLAT the
console access token; quota then reads from the Bailian console gateway. Both
entries report the same `per*` window fields as **0..1 ratios** and CLAT
scales them ×100 (2026-11-23 live capture: the Bailian gateway returned
`per1MonthPercentage: 0.1799` for the account the Qianwen entry read as
17.99% used). One platform caveat: the two console sites share one login
session (ONE_CONSOLE), and a successful Qianwen quota read pins that session
to the Qianwen site — the Bailian gateway then answers
`BailianGateway.Login.NotLogined` for some minutes until the pin decays, so
the Bailian card may read unavailable right after a Qianwen refresh and
recover on its own; a fresh Bailian browser login re-pins the session the
other way. The callback page itself is confirmed by a reply that repeats
`Access-Control-Allow-Origin: *` (2026-11-23: without it the page never saw
the reply and the browser tab hung open after authorizing, even though the
token had already reached this machine).
In Web and Desktop, open **Settings → Authorizations** to manage the Qianwen
and Tencent quota credentials from any model or project. Start or repeat the
Qianwen browser authorization, save or replace the Tencent Cloud API key, and
choose **Refresh all** to refresh the plan quotas. Removing an authorization
deletes the Host's saved quota credential. Model API keys are managed in
**Models**. Authorizations are shared across projects on the same Host.

The Qianwen AI Platform
applies content inspection to the request text (`data_inspection_failed`);
conversation content such as fetched web pages can trigger it, in which case
a fresh session or the Aliyun entry resolves it. They send top-level
`reasoning_effort`
without a `thinking` object. CLAT maps
its Low / High / Max UI to Qwen's `low` / `medium` / `xhigh` ladder. Both
`qwen4-max` and the incremental `qwen3.8-flash` preset carry image input per
the official model pages (Qwen4 Max is the flagship multimodal follow-up; the
vision understanding is officially confirmed; flash documents Image+Text
input, video stays outside CLAT's scope). Their context/output budgets match
the documented 1,000,000 / 131,072 values.

Qwen note (2026-11-18): the flagship was upgraded in place from `qwen3.8-max`
to `qwen4-max` (Qwen4 Max). The preview-window id `qwen4-max-preview` and the
nonexistent `qwen4-flash` are deliberately not presetted; `qwen3.8-flash`
remains the utility companion. Saved configurations referencing
`qwen3.8-max` simply stop resolving to a preset.

Context caching is implicit. Usage parsing accepts both
`prompt_tokens_details.cached_tokens` and the transitional top-level cached
token field. CLAT does not send explicit `cache_control`, whose availability
and miss pricing differ from this endpoint's documented implicit cache.

There is no public balance endpoint for the plan, so the status surface shows
cache and context but does not invent a Token segment.

### Kimi Coding Plan

Settings → Models displays a **Plus (256K) / Pro or higher (1M)** radio
selection beside the Kimi Coding Plan heading, matching the Qwen Token Plan
heading controls. The user-selected subscription tier is saved as a vendor-wide
preference in `settings.json` (`kimiTier`), independently of the active model
or profile; missing historical settings default to Plus. The tier gates the
flagship `k3.1-pro` only: 262,144 tokens on Plus, 1,048,576 on Pro and
higher. `kimi-for-coding` (K2.8 Preview) is exempt from the tier gate — the
official Kimi Code model table lists its 1,048,576-token window with no
subscription-tier note (available from Plus), so it always uses 1,048,576
regardless of the selection (live-verified on a Plus account, 2026-11-21).
Changing the selection immediately updates an active Kimi preset, while other
vendors retain their own context windows. Selecting Kimi later, including after
a Host restart, restores the user's tier. An old automatic capacity cache does
not override this explicit choice; CLAT does not verify subscription entitlement.
The output-token preset and the endpoint/API key remain unchanged.

The preset uses `https://api.kimi.com/coding/v1`, top-level
`reasoning_effort`, and automatic context caching. Low / High / Max map directly
to the vendor ladder. `k3.1-pro` (K3.1 Pro) image input is declared by the
official API documentation, so attach admission is open on that route.

The endpoint also strictly validates tool JSON schemas: every
`tools.function.parameters` property must declare an explicit `type` — an
enum-only property is rejected with 400 "type is not defined" (2026-11-21,
live-reproduced; other vendors accept it, so the gap only surfaces here). The
same-day fix covered `ui_wait`'s `condition`, `ui_act`'s `action`, and the
schedule tools' `time` (an `anyOf` without a top-level `type`); an invariant
test now walks every registered tool schema so new tools cannot regress this.

Messages are validated just as strictly: an assistant message with no
payload — the empty-string serialization of a thinking-only turn — is
rejected with 400 "must not be empty" (same day, live-reproduced; an empty
`content` alongside `tool_calls` is accepted, so tool turns keep their
historical shape). CLAT now skips payloadless assistant turns when
serializing history; reasoning-replay endpoints (DeepSeek, GLM) still emit
`reasoning_content` as the turn's payload and keep the turn. Tool-call
pairing is enforced too: every `tool_call_id` must be followed by a
responding tool message, so a cancelled or interrupted call whose result
was never journaled is rejected with 400 "did not have response messages"
(same day, live-reproduced). CLAT now synthesizes an honest
`[no recorded result]` placeholder response for such calls when serializing
history.

The selected Plus / Pro-or-higher tier sets the actual Kimi Coding Plan
context window. CLAT no longer guesses this entitlement from provider 403
wording, probes for a smaller window, or stores inferred limits in
`~/.clat/tier_limits.json`. A mismatch between the user's selected tier and
the server's actual entitlement is reported as a provider error; no
automatic capacity-based compaction/retry occurs in response to that error.
Switching to or from Kimi uses the same target-window compaction policy as
all other models (including Grok's configured 500K window).

The Coding membership endpoint historically admitted subscription traffic only
for allowlisted coding-agent User-Agents (community matrix, 2026-08), and the
preset injected the compatible `claude-cli/2.1.161` disguise. A 2026-11-21 live
probe with a valid Coding-plan key returned 200 for CLAT's own `clat/<version>`
User-Agent — and even a generic client UA — on both inference and `/usages`, so
the gate is withdrawn and the disguise retired: the presets now send CLAT's own
identity User-Agent. Saved configurations seeded by the older preset are
migrated on the next load; a custom User-Agent in Extra Headers still wins.

The status monitor reads `/usages` and displays one entry per distinct window
the vendor reports — as many as exist, never padded. Today that is the
five-hour window plus a single monthly entry read from `limit_month_total`;
the vendor's `limit_month_code` meter is deliberately ignored (owner ruling,
2026-11-21). No weekly window is reported today; one appears automatically
if the vendor starts returning it.

### Tencent Hy Token Plan

The preset uses `https://tokenhub.tencentmaas.com/plan/v3` with `hy4`
(GA) — the subscription endpoint for Tencent's own Hunyuan models (the
separate "general" Token Plan aggregating third-party open models is
deliberately not covered; those vendors already have direct presets).
Generate the key in the TokenHub console; the same key and URL serve both
plans and deduct by model id.

Console-credential quota (2026-11-22): the model key cannot read quota.
TokenHub exposes it through the Tencent Cloud OpenAPI
(`tokenhub.tencentcloudapi.com`, TC3-signed). The rail quota icon offers a
key-glyph ring on this route: click it and paste a cloud **SecretId /
SecretKey** pair (create one in the Tencent Cloud console, ideally on a
read-only sub-account). The remaining-credits percentage is computed from
the provider-reported total/used; the hover popover and the authorization
card also show the cycle refresh time from the package's `ExpireTime`
(2026-11-23: a Beijing wall-clock string without a zone — parsed as fixed
UTC+8, never as UTC, since the live package opened 13:12 local and a UTC
reading would have been in the future). The pair is stored locally,
encrypted at rest with an embedded key.

Hy note (2026-11-07): the GA release upgraded the preset in place from
`hy4-preview` to `hy4`; the endpoint spelling follows the official plan
docs (`tokenhub.tencentmaas.com`, with the legacy `lkeap.cloud.tencent.com`
host kept for vendor detection of saved custom configurations). The GA
calling guide documents a `reasoning_effort` parameter (default `high`) on
the TokenHub platform API, but the plan subscription endpoint has not been
re-probed, so the preset keeps sending no thinking parameters. Saved
configurations referencing `hy4-preview` simply stop resolving to a
preset. The GA vision capability is not enabled: the plan's official model
list still notes that multimodal input is not supported on this endpoint.

All other parameters are pinned by the TC-0 live probe
(`docs/research/tc0-probe/`, 2026-09-02) and confirmed unchanged by the GA
spec table (1024k total window, 960k max input, 64k max output):

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

Use **Settings → Authorizations** to manage the Tencent Cloud key and refresh
the personal plan quota. You can also check it in the
[TokenHub console](https://console.cloud.tencent.com/tokenhub/tokenplan).
Concurrency limits scale with the plan tier and may tighten at peak hours.

### OpenCode Go

Twelve presets ride the compliant OpenCode gateway
`https://opencode.ai/inference/go/v1` — the owner-mandated address
(2026-11-18): eight open-source models (DeepSeek V4 line, Kimi K3 line,
GLM-5.3 line, Qwen4, MiniMax M3) plus four overseas closed models added by
the owner on 2026-11-19 (Claude Haiku 5.5, GPT-6 Luna, Gemini 4 Argon,
Grok 4.7). The gateway is OpenAI-compatible; preset ids carry an
`opencode-` prefix because four gateway model ids collide with the direct
Connection-plan presets (`glm-5.4`, `glm-5.3`, `glm-5.3-flash`, `qwen4-max`,
`deepseek-v4-pro`) — the `model` field still uses the raw gateway id.
Context and output values are pinned from the gateway's public `/v1/models`
snapshot (including non-power-of-two originals — GPT-6 Luna's
1,050,000/128,000, the gateway DeepSeek lines' decimal 384,000 output, the
gateway Kimi lines' 1,048,576 context — all corrected by the 2026-11-19
full audit) and deliberately not
cross-referenced with each vendor's own
endpoint documentation (Kimi K3/K3.1 Pro serve 1M output here versus 128K
on the Kimi Coding endpoint; MiniMax M3 above 512K context is priced ×2;
the gateway also lists `gpt-5.6-luna`, which stays unpresetted because it
was not named). For the three models whose advertised maximum output equals
the whole context window (Kimi K3, Kimi K3.1 Pro at 1M/1M, Grok 4.7 at
500K/500K), the raw maximum is not a pinnable value — CLAT's capacity
budget requires positive input room after the output reserve — so those
presets pin a pragmatic 262,144-token output cap instead (the same style as
the Hy presets, which pin below the endpoint's accepted maximum); a
sentinel test rejects any future preset whose window/output pair fails the
budget.

The gateway's third-party policy explicitly allows coding agents and lists
CLAT-class clients among verified clients, with two wire obligations the
presets satisfy:

- **Own User-Agent** — every OpenCode preset injects
  `User-Agent: clat/<version>` (overridable in Extra Headers, cleaned again
  when switching presets if the value is still a preset-seeded `clat/*`).
- **Stable `x-opencode-session` routing header on every request** — missing
  the header is rejected with 400. The run carries its session id into every
  model request; read-only subagent children carry the parent session id;
  sessionless one-shot requests (titles, suggestions, Git commit messages,
  sampling) fall back to the stable `clat-auxiliary` routing key. The
  adapter injects the header only when the endpoint domain is
  `opencode.ai`, so ordinary endpoints never observe it.

Reasoning effort follows the gateway's **per-model** effort arrays (the
original "unified ladder" research reading was overturned by the 2026-11-19
`/v1/models` audit — the first batch's `medium` pin was invalid on six of
the eight open models). Each preset pins a value from its own model's
array — `high` for the DeepSeek/Kimi K3.1/GLM/Qwen4 Max routes, `max` for
Kimi K3 (the only value its array offers), nothing for MiniMax M3 (toggle
only, no effort levels — the same send-no-ineffective-parameter precedent
as the Hy presets), and `medium` for the four closed models whose arrays
include it and whose pins the owner verified live. The Shift+Tab ladder is
also per model: a level is offered exactly when its wire value is in the
model's array (Kimi K3 offers only Max, DeepSeek V4 Pro starts at High,
Grok 4.7 / Gemini 4 Argon / Qwen4 Max stop at High, MiniMax M3 has none),
and a saved level the model does not accept — for example a Max carried
over from another model — is neither displayed nor sent; the preset's
pinned effort goes out instead. The
gateway has no DeepSeek/GLM-style `thinking` object, and
`stream_options.include_usage` behavior is unverified there, so the preset
extra body carries `reasoning_effort` only (nothing at all for MiniMax).

The gateway's chat-completions facade also translates tool schemas per
upstream family and rejects some standard JSON-Schema constructs that
direct endpoints accept (owner live test, 2026-11-19): Google models
reject `strict: true` outright; OpenAI strict mode requires `required` to
list every property, which CLAT's optional-parameter tools cannot satisfy;
Anthropic's schema whitelist rejects array-bound keywords such as
`maxItems`. On `opencode.ai` endpoints the adapter therefore omits
`strict` for `gemini-*`, `gpt-*`, and `claude-*` models and recursively
strips `maxItems`/`minItems` from tool schemas for `claude-*`; the
gateway-native open-model families and every other endpoint keep the wire
shape unchanged.

**Workspace regions**: each OpenCode workspace carries a privacy
data-residency setting, and models hosted outside the selected regions are
refused with an actionable 400 naming the console URL (owner live test,
2026-11-19: both gateway DeepSeek lines are hosted outside Europe/US and
require the workspace region setting `Global`; the other ten models route
under the EU/US default). This is a console-side decision, not a CLAT
configuration — the error text propagates as-is.

**Quota windows**: `GET /v1/usage` (Bearer) reports three windows — rolling
(5 hours, capped at 20% of the monthly allowance), weekly (50%), and
monthly. The status monitor maps each window to a remaining-percentage
quota item with its reset time when the gateway reports one
(`resetsAt` as ISO text or epoch milliseconds); missing reset times stay
absent rather than invented.

## Reasoning control

`ThinkingLevel` is provider-aware:

| CLAT level | DeepSeek / GLM / Kimi | Qwen | Tencent Hy | OpenCode Go |
|---|---|---|---|---|
| Low | `low` | `low` | — | `low` |
| High | `high` | `medium` | — | `high` |
| Max | `max` | `xhigh` | — | `max` |

Tencent Hy exposes no effective reasoning control (see above); the ladder is
empty, `Shift+Tab` has no effect on that vendor, and the title bar shows
`Thinking · Server` so the always-on state stays visible.

On OpenCode Go the mapping above is the vendor-wide default, and the actual
ladder is narrowed per model from the gateway's effort arrays (see the
OpenCode Go section): models without `max` never offer Max, Kimi K3 offers
only Max, and a saved out-of-ladder level is not sent — the preset pin goes
out instead.

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
| serialized request body | 32 MiB |
| baseline connection setup timeout | 30 s |
| baseline response-header timeout | 60 s |
| stream idle timeout | 300 s |
| one SSE line | 1 MiB |
| provider error body | 64 KiB |
| aggregated response | output tokens × 64 bytes, clamped to 1–64 MiB |

The response ceiling scales with the configured output limit so legitimate
large outputs are not subject to an unrelated flat cap. Crossing any limit
returns a typed provider error. The connection/header values are transport
baselines; consumer deadlines can narrow them. The idle timeout bounds silent
stream waits, not the total duration of an interactive turn.

Journal zstd frames have their own 64 MiB decoded budget; see
[Persistent state](storage.md#session-journals).

## Retry policy

Every model call is built through a `ModelFactory` and wrapped by `RetryModel`.
Each attempt creates a fresh adapter instance, so connection state is not reused.
The default policy allows four total attempts (the initial call and up to three
retries), with 1 s, 2 s and 4 s backoff delays.

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

For any non-success HTTP response carrying `Retry-After`, the advertised delay
is capped at 30 seconds. Retry eligibility still follows the typed error
classification above. Once any stream event has
been emitted to the caller, the attempt is no longer retryable; resending would
duplicate output or tool calls the user already observed.

Factory failures follow the same typed classification.

## Deadlines and attempt caps

Normal interactive agent turns have no total deadline; the user cancellation
token is their outer lifetime. Internal model consumers use explicit budgets:

| Consumer | Total deadline | Underlying attempt cap |
|---|---:|---:|
| automatic session title | 15 s | 1 |
| manual prompt suggestion (utility) | 15 s | 1 |
| compaction map/reduce | 60 s | 8 |

A deadline is attached to a child `CancelToken`, not only to retry sleep. It
therefore bounds connection setup, response headers, a silent SSE body, and
backoff together. Provider connect/header/global timeouts are clamped to the
remaining time.

Map/reduce consumers share one retry wrapper, so the attempt cap covers the
whole operation instead of resetting for each subrequest.

## Interactive tool arguments

Both HTTP adapters require one complete JSON object for `ask_user`.
Malformed input becomes a failed tool result with instructions to retry.
The native tool validates field types, option labels and size limits before
showing a question. DeepSeek thinking mode ignores temperature, so this
recovery does not depend on sampling settings.

## Cancellation

Providers check the shared token between SSE reads and before decoding a
completed tool call. Cancelling a compatible stream discards incomplete tool
arguments instead of reporting misleading malformed JSON.

Parent cancellation always wins over an internal deadline. Run cancellation
also propagates to native tools, MCP requests, WASM execution, and plugin-host
sampling so the provider layer is not an isolated cancellation island.

## Prompt cache policy

DeepSeek, Qwen and Kimi presets use their endpoint's automatic caching rather
than injecting one vendor's explicit cache controls into another vendor's API.
GLM additionally uses preserved thinking as described above. Unknown compatible
endpoints retain the conservative tool-turn-only reasoning replay policy.
The Responses adapter preserves native opaque reasoning state; it does not
substitute compatible-API `reasoning_content` for that state.

Stable instructions and all policy-allowed tool schemas precede growing
conversation history. The core orders common tools in a fixed table, then
other allowed tools by name; RESIDENT controls order, not publication.
A `describe_capability` request must never be required to call a tool
already admitted by the frozen policy. Keeping definitions visible and
ordered preserves functional tool access and maximizes reusable prefix
opportunities. Both HTTP adapters append transient host context, such as
the active scratch directory, after projecting history. That context is counted
against the input budget and regenerated for the next run. Scratch guidance
asks the model to apply it silently and keep routine setup out of replies and
progress updates.
Instructions, model/permission/skill changes and context compaction can change
the reusable prefix. A new conversation or an expired server cache can have a
low hit rate; enabling caching does not promise a particular hit percentage.
See the [DeepSeek cache guide](https://api-docs.deepseek.com/guides/kv_cache/),
[Qwen cache guide](https://www.alibabacloud.com/help/en/model-studio/context-cache)
and [OpenAI cache guide](https://developers.openai.com/api/docs/guides/prompt-caching).
Optional cache lifetimes and paid explicit writes are not enabled by default;
endpoint-specific options remain available through model configuration.

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

## Parameter sources and cross-checks

Every preset's pinned values trace to a dated primary source; diverging
secondary sources are recorded rather than silently followed.

| Preset family | Primary source (dated) |
|---|---|
| DeepSeek (`deepseek-flash`, `deepseek-v4-pro`) | [Models & Pricing](https://api-docs.deepseek.com/quick_start/pricing), api-docs.deepseek.com, re-checked 2026-11-03 |
| GLM Coding Plan (`glm-5.4`, `glm-5.3`, `glm-5.3-flash`) | [GLM 5.3 guide](https://docs.z.ai/guides/llm/glm-5.3) + the [Coding endpoint](https://open.bigmodel.cn/api/coding/paas/v4) docs; owner first-party confirmation 2026-08; `glm-5.4` reuses the Flash vision matrix |
| Qwen Token Plan (`qwen4-max`, `qwen3.8-flash`) | [Qwen4 Max](https://help.aliyun.com/zh/model-studio/qwen4-max), [Qwen3.8 Flash](https://help.aliyun.com/zh/model-studio/qwen3-8-flash), [Token Plan overview](https://www.alibabacloud.com/help/zh/model-studio/token-plan-personal-overview), checked 2026-11-18 |
| Kimi Coding Plan (`k3.1-pro`, `kimi-for-coding`) | Kimi Code docs [model configuration](https://www.kimi.com/code/docs/kimi-code/models.html), re-checked 2026-11-21 (the Coding model table now lives here; previously platform.kimi.com docs, first fetched 2026-11-03) |
| Hy Token Plan (`hy4`, `hy4-flash`) | TokenHub plan/calling guides (2026-11-04/11-07 updates) behind the [TokenHub console](https://console.cloud.tencent.com/tokenhub/tokenplan), plus the TC-0 live probe (`docs/research/tc0-probe/`) |
| OpenCode Go (12 presets) | The gateway's own public `GET https://opencode.ai/inference/go/v1/models` — snapshots 2026-11-18 and 2026-11-19 (zero drift between them); [console docs](https://opencode.ai/docs/console/go) for policy |

Two cross-check databases confirm or challenge the pins and are consulted
when a value changes:

- **models.dev** ([`api.json`](https://models.dev/api.json)) — the public
  model database. Its independent `opencode-go` subscription entry matches
  CLAT's post-audit gateway pins exactly (including the decimal 384,000
  DeepSeek outputs and the 1,048,576 Kimi contexts), and its first-party
  entries agree with the direct GLM/Qwen/Grok/GPT/Claude pins. Known
  benign divergences (first-party capability ceilings versus
  official-document figures CLAT follows): DeepSeek context 1,048,576
  versus the documented 1M, and Kimi coding-plan outputs — recorded in the
  local research reference, not acted on.
- **cc-switch** ([github.com/farion1231/cc-switch](https://github.com/farion1231/cc-switch))
  — the provider-switcher maintainers hand-verify per-address model
  metadata. Two observations recorded from it: GPT-shaped models sourced
  from chat subscriptions are effectively window-capped at 272,000 tokens
  on many relays (the gateway's own GPT-6 Luna pricing has a 272,000
  context breakpoint), and its official Go preset uses the
  `opencode.ai/zen/go/v1` alias of the same gateway. Its per-address
  caps (for example GLM at 272K on unrelated relays) describe those
  relays, not the OpenCode gateway.

The full per-preset comparison table against both databases lives in the
local-only research note `docs/research/oc-model-parameter-reference.md`
(plain-text pointer; local research notes are not linked from public docs).

## Adding a provider

A new provider should normally implement a `ModelFactory` and `Model` adapter,
then register it through the Provider registry. Keep vendor request/response
types inside the adapter. Do not add vendor matches to `Run`, frontends, or
session projections.

Before adding a preset, verify model id, endpoint, request path, context window,
output limit, reasoning parameters, usage shape, and authentication semantics.
The preset application path and editor must continue to share one catalog
entry so displayed and executed values cannot drift.
