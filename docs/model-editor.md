# Model editor (`/model`)

Use `/model` to select a built-in provider preset or manage named custom model
profiles. This document covers the editor workflow and saved fields. Wire-level
adapter behavior lives in [Providers](providers.md).

## Fast path: built-in preset

1. Run `/model`.
2. Choose a vendor, then a preset.
3. Open the preset editor, paste the API key, and save with `Ctrl+S` or the
   `[ Save ]` row.

The short preset form contains only Preset, Model, Endpoint, and API Key.
Changing Model or Endpoint intentionally converts the configuration to Custom;
the preset label never remains attached to modified preset-owned fields.

API keys are remembered per vendor. Switching among presets of the same vendor
does not erase the key, and choosing a preset never copies credentials into a
session journal.

## Web/Desktop quick switcher

The model button beside the composer opens a compact picker. The top row shows
the **provider · model name** (for example, `GLM · GLM 5.3 Flash`, or
`OpenCode · GPT-6 Luna` for an OpenCode Go route) with a
vision icon only when the active route accepts images. Its settings icon opens
**Settings → Models**.

Choose **Built-in presets** or **Custom models** first. Built-in presets are
grouped by **connection channel** (DeepSeek, GLM Coding Plan, Qwen Token Plan,
Kimi Coding Plan, Hy Token Plan and OpenCode Go); choosing a channel reveals
only its models. The channel is not the maker: OpenCode Go can serve models from
several makers. Saved custom profiles are displayed directly as a separate list.
The picker does not ask for an API key; configure or change credentials under
**Settings → Models**. Existing saved keys and all preset routing semantics
are unchanged.

The thinking-intensity control stays beneath the selection list and always
uses the **available levels returned by the Host for the active model**. Its
animated progress, endpoint labels and current-level feedback are presentation
only; changing a level keeps the existing override/save behavior. Motion
respects the platform's reduced-motion preference. The popup is anchored to the
composer's right edge and never extends past it.

## Built-in presets

The shipped catalog is the source used by both the picker and runtime:

| Preset | Model id | Endpoint | Context | Max output |
|---|---|---|---:|---:|
| DeepSeek V4.1 Flash | `deepseek-flash` | `https://api.deepseek.com` | 1M | 384K |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` | `https://api.deepseek.com` | 1M | 384K |
| GLM 5.4 | `glm-5.4` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| GLM 5.3 | `glm-5.3` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| GLM 5.3 Flash | `glm-5.3-flash` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| Kimi K3.1 Pro | `k3.1-pro` | `https://api.kimi.com/coding/v1` | 256K (Plus) / 1M (Pro+) | 128K |
| Kimi K3.1 Pro Highspeed | `k3.1-pro-highspeed` | `https://api.kimi.com/coding/v1` | 1M (Pro+) | 128K |
| Kimi K3 | `k3` | `https://api.kimi.com/coding/v1` | 256K (Plus) / 1M (Pro+) | 128K |
| Kimi for Coding | `kimi-for-coding` | `https://api.kimi.com/coding/v1` | 1M (any tier) | 128K |
| Qwen4 Max | `qwen4-max` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Qwen3.8 Max | `qwen3.8-max` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Qwen3.8 Flash | `qwen3.8-flash` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| DeepSeek V4.1 Flash | `deepseek-v4.1-flash-1017` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 384K |
| DeepSeek V4.0 Pro | `deepseek-v4-pro-0813` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 384K |
| GLM 5.3 | `qwen-glm-5.3` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Hy 4 | `hy4` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 64K |
| Hy 4 Flash | `hy4-flash` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 64K |
| DeepSeek V4.1 Flash · Hy Common | `hy-deepseek-v4.1-flash-1017` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 384K |
| GLM 5.4 · Hy Common | `hy-glm-5.4` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| GLM 5.3 · Hy Common | `hy-glm-5.3` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| GLM 5.3 Flash · Hy Common | `hy-glm-5.3-flash` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| Kimi K3.1 Pro · Hy Common | `hy-k3.1-pro` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| Kimi K3 · Hy Common | `hy-k3` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| MiniMax M3 · Hy Common | `hy-minimax-m3` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| MiniMax M3.1 Flash · Hy Common | `hy-minimax-m3.1-flash` | `https://tokenhub.tencentmaas.com/plan/v3` | 1M | 128K |
| DeepSeek V4 Pro · OpenCode Go | `opencode-deepseek-v4-pro` | `https://opencode.ai/inference/go/v1` | 1M | 384K |
| DeepSeek V4.1 Flash · OpenCode Go | `opencode-deepseek-v4.1-flash` | `https://opencode.ai/inference/go/v1` | 1M | 384K |
| Kimi K3.1 Pro · OpenCode Go | `opencode-kimi-k3.1-pro` | `https://opencode.ai/inference/go/v1` | 1M | 256K |
| Kimi K3 · OpenCode Go | `opencode-kimi-k3` | `https://opencode.ai/inference/go/v1` | 1M | 256K |
| GLM 5.3 · OpenCode Go | `opencode-glm-5.3` | `https://opencode.ai/inference/go/v1` | 1M | 128K |
| GLM 5.3 Flash · OpenCode Go | `opencode-glm-5.3-flash` | `https://opencode.ai/inference/go/v1` | 1M | 128K |
| Qwen4 Max · OpenCode Go | `opencode-qwen4-max` | `https://opencode.ai/inference/go/v1` | 1M | 262K |
| MiniMax M3 · OpenCode Go | `opencode-minimax-m3` | `https://opencode.ai/inference/go/v1` | 1M | 128K |
| Claude Haiku 5.5 · OpenCode Go | `opencode-claude-haiku-5-5` | `https://opencode.ai/inference/go/v1` | 1M | 128K |
| GPT-6 Luna · OpenCode Go | `opencode-gpt-6-luna` | `https://opencode.ai/inference/go/v1` | 1.05M | 128K |
| Gemini 4 Argon · OpenCode Go | `opencode-gemini-4-argon` | `https://opencode.ai/inference/go/v1` | 1M | 64K |
| Grok 4.7 · OpenCode Go | `opencode-grok-4.7` | `https://opencode.ai/inference/go/v1` | 500K | 256K |

In **Settings → Models**, the GLM Coding Plan heading lets you choose
**bigmodel.cn** (China, the default) or **z.ai** (international). With a GLM
Coding Plan preset active, the choice takes effect immediately. Otherwise,
select the entry and click **Apply** on that card. CLAT remembers the entry
across model changes and restarts and saves an API key for each entry. Use the
key from the platform you choose. The international entry uses
`https://api.z.ai/api/coding/paas/v4` for the GLM Coding Plan presets above.

The same page has three more vendor-level controls. The Qwen Token Plan heading
offers **Aliyun Bailian** (the default) or **Qianwen AI Platform**; the choice
switches only the endpoint, and each platform needs its own key. The Kimi Coding
Plan heading offers a **Plus** or **Pro or higher** subscription tier, which
sets the 256K or 1M window of `k3.1-pro` and `k3`; `kimi-for-coding` always
uses 1M. The tier also gates visibility: `k3.1-pro-highspeed` is "Pro and
above" in the official model table, so it is hidden from every selection
surface on Plus (and direct selection is refused; a downgrade with it active
is refused until another model is selected), while `k3` and the other Kimi
presets stay available on both tiers. The Hy Token Plan heading offers a
**Hy** or **Common** plan radio: both plans share
one key and endpoint (the official "independent subscriptions"), so the
radio switches only the selection surface — the Hy plan shows the Hunyuan
pair, the Common plan additionally shows the eight third-party presets
(`hy-`-prefixed ids; specs pinned per each model's own documentation).
Quota credentials for Qianwen, Aliyun Bailian and Tencent are managed under
**Settings → Authorizations**, not in the model editor.

All use the OpenAI-compatible protocol. The catalog also owns request paths,
reasoning parameters, usage streaming, context-window seeds, and vendor-specific
headers. For example, GLM preserves thinking, Qwen uses its
`low`/`medium`/`xhigh` ladder, the Kimi Coding and OpenCode Go presets send
CLAT's own `clat/<version>` User-Agent (Kimi's coding-agent allowlist gate was
verified withdrawn on 2026-11-21), the OpenCode Go presets additionally carry a
stable `x-opencode-session` routing header, and the Hy presets always think
server-side (no ladder). See [Providers](providers.md#built-in-presets)
before overriding those fields.

GLM 5.3 Flash carries probe-verified native image input (including image tool
results); GLM 5.4 reuses that same vision matrix. The officially-declared vision
presets — `deepseek-flash`, `qwen4-max`, `qwen3.8-max`, `qwen3.8-flash`,
`deepseek-v4.1-flash-1017`, `k3.1-pro`,
`k3.1-pro-highspeed`, `k3`,
`kimi-for-coding`, `hy4`, `hy4-flash`,
`hy-deepseek-v4.1-flash-1017`, `hy-k3.1-pro`, `hy-k3`,
`hy-minimax-m3`, and `hy-minimax-m3.1-flash` — plus GLM 5.3 Flash and GLM 5.4 (and their Hy Common twins `hy-glm-5.3-flash` / `hy-glm-5.4`) all enable
the image attachments described in
[Using CLAT](usage.md#image-attachments), as do the ten OpenCode Go vision
presets (`opencode-deepseek-v4.1-flash`, `opencode-kimi-k3.1-pro`,
`opencode-kimi-k3`, `opencode-glm-5.3-flash`, `opencode-qwen4-max`,
`opencode-minimax-m3`, `opencode-claude-haiku-5-5`, `opencode-gpt-6-luna`,
`opencode-gemini-4-argon`, `opencode-grok-4.7`; gateway `/v1/models`
metadata). All other presets
remain text-only
in CLAT. This does not restrict images handled wholly inside configured MCP
tools. DeepSeek note (2026-09-10): the official V4.1 Flash release retired
`deepseek-v4-flash` and `deepseek-v4-flash-vision-exp`; the preset was
upgraded in place to `deepseek-flash` (vision slot inherited), and saved
configurations referencing the old ids simply stop resolving to a preset.

Most presets also declare a companion utility model used only for automatic
session naming, manual prompt suggestions and automatic Web/Desktop next steps: `deepseek-v4-pro` uses
`deepseek-flash`, `glm-5.3` and `glm-5.4` use `glm-5.3-flash`,
`qwen4-max`, `qwen3.8-max`, `qwen3.8-flash`, `deepseek-v4.1-flash-1017`,
`deepseek-v4-pro-0813` and `qwen-glm-5.3` use `qwen3.8-flash`,
`k3.1-pro`, `k3.1-pro-highspeed` and `k3` use `kimi-for-coding`, and
`hy4` and the eight Hy Common presets use `hy4-flash`. The OpenCode Go
flagships use the gateway's flash tiers —
`opencode-glm-5.3` uses `opencode-glm-5.3-flash`, and presets without an
in-family light tier (`opencode-deepseek-v4-pro`, `opencode-kimi-k3.1-pro`,
`opencode-kimi-k3`, `opencode-qwen4-max`, `opencode-minimax-m3`,
`opencode-gemini-4-argon`, `opencode-grok-4.7`) use
`opencode-deepseek-v4.1-flash` on the same endpoint and key slot, while
`opencode-claude-haiku-5-5` and `opencode-gpt-6-luna` are themselves light
tiers and use the primary model; every
other preset uses the primary model itself. See
[Using CLAT](usage.md#companion-utility-and-manual-suggestions) for the
budget and preview semantics.

## Custom profiles

Custom models are named profiles. Each row keeps a complete model
configuration and its own optional API key, so switching profiles does not
destroy the inactive configuration.

- With no profiles, selecting Custom opens a blank profile template.
- With profiles, Custom opens a list showing `name`, `endpoint · model`, and a
  `●` beside the active saved row.
- `Enter` activates a row, `e` edits, and `d` twice confirms deletion.
- `New…` opens the blank template.

Deleting the active profile activates the first remaining profile. If none
remain, CLAT returns to factory custom defaults. Existing installations with a
legacy single custom slot are adopted into the first named profile.

The required profile fields are:

- **Name** — unique display identity;
- **Model** — provider model id;
- **Endpoint** — base URL;
- **API Key** — optional, so local gateways can omit it.

Save activates the profile immediately. `Esc` cancels the current editor and
returns to the exact picker row it came from; it does not collapse the entire
dialog chain.

## Editor controls

Press `Enter`, click, or type on a text field to open its input. `Enter`
confirms the field, `Esc` cancels it. `Ctrl+S` saves the whole editor.
On Max Output, Context Window, Temperature, Parallel Tool Calls, and the
profile Thinking row, `Ctrl+D` toggles an explicit `Clear` tombstone. A
cleared row says `cleared (field omitted)`; this differs from an empty value,
which means `Inherit`. Editing or cycling the row replaces the tombstone.

Numeric settings use bounded choice rows before a custom input:

| Field | Choices |
|---|---|
| Context Window | 128K (default template), 256K, 1M, Custom… |
| Max Output | 8K, 32K (default template), 128K, Custom… |
| Spend Budget | 1M, no cap (default), 50M, off, Custom… |

`Enter` or `←`/`→` cycles a choice. Selecting Custom opens a numeric input.

## Reasoning level

The profile Thinking row offers Low, High, Max, and Off. Off sends no inferred
reasoning parameter and follows the endpoint's own behavior. Known vendor
domains receive the vendor-native mapping; unknown domains retain the saved
selection for display but do not receive an injected parameter because strict
gateways may reject it. Configure unknown endpoints through Extra Body.

Preset defaults are pinned to a practical middle tier:

- DeepSeek, GLM, and Kimi store `high`;
- Qwen stores `medium`, which is CLAT's High tier;
- all presets keep a context-window value so automatic compaction works on the
  first long conversation.

`Shift+Tab` changes the active runtime configuration for the next run. For a
named profile this deliberately does not rewrite the saved row: the `●` marker
drops until you switch away and reactivate the profile. To persist the change
inside that profile, edit its Thinking row.

The ladder is per model where the endpoint says so: on the OpenCode Go gateway
each model's effort array decides which levels are offered (Kimi K3 exposes
only Max, DeepSeek V4 Pro starts at High, Grok 4.7 / Gemini 4 Argon /
Qwen4 Max stop at High, MiniMax M3 has no levels at all), and a saved level
that a model does not accept is neither displayed nor sent — the preset's
pinned effort goes out instead. Direct vendors keep their vendor-wide
ladders.

DeepSeek also supports a raw non-thinking mode, but the shortcut ladder stays
Low → High → Max. GLM 5.3 rejects disabled thinking. Hand-written Extra Body is
the escape hatch for any provider-specific mode not represented by the editor.

## Advanced fields

`[ Advanced ]` exposes transport and request controls:

- **Protocol** — OpenAI Compatible (default) or OpenAI Responses;
- **Request Path** — `/chat/completions` or `/responses` by default;
- **Auth Header** and **Auth Prefix** — default `Authorization` and `Bearer `;
- **Extra Headers JSON** — for example `{"X-Tenant":"acme"}`;
- **Extra Body JSON** — for example `{"top_p":0.9}`;
- **Max Output Tokens**;
- **Context Window**;
- **Spend Budget**;
- **Temperature**;
- **Parallel Tool Calls**.

The provider adapter protects managed fields such as model, messages, tools,
stream, and instructions. Trying to override them through Extra Body is an
error rather than an ambiguous merge.

Editing any preset-controlled field—Model, Endpoint, Protocol, Request Path,
Extra Body, Max Output, Temperature, or Parallel Tool Calls—clears the preset
identity. Editing Extra Body also clears the shortcut-managed thinking value so
the raw body becomes the single source of truth.

## Context and spend budgets

**Context Window** is the automatic compaction budget. Presets seed it with the
catalog value; a user-entered value wins. CLAT estimates context before the
next run, triggers around 80% of the window, and compacts toward the same ratio
while preserving the original journal.

**Spend Budget** optionally limits total input+output tokens for one run.
The default is **no cap**: CLAT no longer applies a silent local default
(the former 10M fallback was removed on 2026-10-15), so long tasks are not
stopped by a limit you never chose. `0` explicitly disables an existing
guardrail and behaves the same as no cap. Despite the label, its unit is
tokens, not money. Every model or extension-sampling request reserves a
conservative amount before it starts; provider usage reconciles the reservation
when available. Malformed or absurd usage cannot wrap or silently reset the
ledger. Crossing a cap you set stops the run with used/cap values and a
pointer back to `/model`; without a cap, accounting still runs and real
provider quota and context compaction still apply.

The spend value belongs to the model configuration, so different profiles can
carry different risk/cost limits.

## Provider replay data

Some providers require hidden state on later turns. The editor does not expose
or persist that state as configuration:

- OpenAI Responses reasoning items travel as opaque provider replay state;
- DeepSeek `reasoning_content` is replayed from every loaded assistant turn
  when a request to the official endpoint carries tools (even for turns that
  made no tool call) and thinking is not disabled; without tools plain-answer
  reasoning is omitted. GLM does the same while preserved thinking is active.
  See [Providers](providers.md#deepseek).

This data belongs to the session journal and provider adapter. Switching a
profile changes future requests but does not rewrite historical events.

## Web workbench settings

Open **Workbench settings → Models** to choose a vendor-grouped preset or
create, edit, activate, and delete a custom profile without closing the host.
Model settings are shared by every mounted project. A change is broadcast to
online clients; already-running requests retain their frozen model configuration.

The composer model picker popup labels the active route at its top as
vendor · model name, using the same vendor label each round's model-request
trace shows; a route that does not match a built-in preset shows the model
name alone.

API keys are write-only. Read responses expose only whether a key is set, never
its value, extra headers, extra request bodies, or authentication prefixes.
Leaving the preset key blank uses the remembered key for that vendor. In a
custom profile, a blank key preserves the old key only if the full route is
unchanged. Changing protocol, model, endpoint, or request path starts with clean
credentials and advanced settings; use the clear-key checkbox to remove a key
explicitly. Saving a profile does not activate it: choose **Use** afterwards.

Choose **New profile** to clear the editor and start a separate profile without
changing any saved profile. Delayed profile reads cannot replace newer form
input or reopen an editor after settings have been closed.

Deleting the active profile atomically activates the first remaining profile,
or returns to an unconfigured model if none remains. Activating a profile and
updating its active pointer are a single settings-file commit. A vision probe
that finishes after model settings changed cannot replace the newer settings.

The additive host methods are `model.settings.get`, `model.profile.get`,
`model.profile.save`, `model.profile.activate`, `model.profile.delete`, and
`model.preset.select`, plus `model.preset.entry` (GLM/Qwen entry),
`model.preset.tier` (Kimi tier), `model.overrides.set`, `model.thinking.cycle`
and `model.utility.get` /
`model.utility.set`; the secret-free notification is `models`.

## Attached terminal support

`clat attach` supports the existing `/model` picker for preset selection,
profile activation and confirmed deletion through the host. The terminal reads
only redacted model summaries (including the image-input capability), never
saved credentials. Shift+Tab cycles thinking levels on the host using its latest
configuration, and applies to the next run. While a change is pending, repeated
keypresses do not send additional requests. Unsupported models reject the change;
transport failures require checking host state before retrying. Other clients
receive the model-settings notification.

The Custom picker can now create and edit basic host profiles in the traditional
terminal editor: name, model, endpoint, protocol, request path, and API key.
Hosts advertising `limits` also expose context window, maximum output tokens,
and per-run token budget with the existing arrow-key choices and Custom input.
An empty custom output/context value or Ctrl+D removes that limit; a blank
budget uses the default, and budget **off** is explicit zero. Invalid numbers
remain in the editor without sending a save request. Older hosts keep the basic form.
Hosts advertising `tuning` add an **Advanced** section for Temperature,
Parallel Tool Calls, and Thinking. Enter edits temperature or toggles parallel
calls; arrow keys cycle thinking levels. Ctrl+D clears a field. A cleared parallel
setting omits the request parameter, unlike **off**, which sends false. Cleared
thinking removes the explicit effort override; it does not disable the model's
own reasoning. Vendor mapping and unsupported-endpoint behavior stay in core.
Only changed tuning fields are submitted for an unchanged profile route. Saving
under another name or changing the route carries the visible tuning values,
but never copies hidden headers, extra JSON, or credentials.
Confirm a field with Enter before Ctrl+S saves the profile; select **Use** to
activate it afterwards. Changing the name saves a separate profile without
deleting the original or copying its hidden credentials and extra settings.
An untouched key is omitted from the save request. Explicitly confirming an empty
key clears it. Key input is masked and cleared after a save request is dispatched;
on failure, check host state and re-enter the key if needed. Newer editor input
survives a delayed save response. Routes marked `route_redacted` by the host cannot be
edited here.

Hosts advertising `extra_body_edit_supported` expose **Extra Body JSON** under
Advanced, with the same write-only, masked, explicit-object replacement workflow
as headers. Omission/null preserves same-route content; `{}` clears it. Objects
are limited to 64 KiB encoded JSON and depth 32. Replacing the body relinquishes
the typed Thinking override so activation/reload cannot rewrite the raw JSON.
The row says **resets Thinking**, and the Thinking row is hidden while a body
draft is pending. Core rejects a simultaneous explicit Thinking patch; save
those changes separately. Other tuning fields remain independent. Dispatch clears
the secret draft and rebases the tuning form; on failure check host state and
re-enter the intended edits. Existing provider reserved-field and null-tombstone
rules still apply. Saving a profile does not activate it.

Hosts advertising `extra_headers_edit_supported` expose **Extra Headers JSON**
under Advanced. This is a write-only whole-object replacement: existing header
names and values are never fetched; an untouched draft preserves same-route
headers, and explicit `{}` clears them. Blank input is invalid. Values must be
strings with valid HTTP header names/values and no control characters. The host
limits the object to 128 entries, names to 256 bytes, values to 8192 bytes, and
encoded JSON to 64 KiB. The row and input popup hide the draft; dispatch clears
it even if saving fails, so retry requires re-entry. Saving does not activate
the profile; changing the route never inherits hidden headers.
Unchanged routes retain their host-owned hidden settings; changing the route
resets those hidden settings, just as in PWA Models settings.
The standalone editor described above keeps its existing behavior.

Hosts advertising `auth_edit_supported` also expose **Auth Header** and
**Auth Prefix** under Advanced. These are write-only replacements, not a view
of the stored values. Enter opens an empty field; confirming an empty value
explicitly clears that field. Untouched fields stay omitted. Prefix input is
masked and preserves trailing spaces (for example `Bearer `). Confirm the field
before saving; both auth drafts are cleared when the request is dispatched,
including on failure. Re-enter them for an explicit retry. Core rejects invalid
HTTP header names, control characters, and oversized values without echoing
the input. These fields retain their existing provider-specific meaning; they
do not override adapters that own their authentication format.

On an attached preset row, Enter selects immediately using the host's remembered
vendor key; **e** opens a masked, key-only editor for that preset. Confirm the key
with Enter, then Ctrl+S saves **and activates** the preset. The preset ID is fixed;
no endpoint or other preset-owned fields are sent. Leaving the key untouched uses
the remembered vendor key. Confirming an empty key clears the current credentials,
not the remembered vendor cache (a later selection can reuse that cache).
Cancel does not submit; once dispatched, a save is not cancelled by closing the
form. The same single-flight, write-only and delayed-response protections apply.

## Desktop setup and connection checks

Desktop first-use setup and the Models settings connection button share the
core `model.setup.get`, `model.setup.apply`, `model.setup.verify`,
`model.setup.status` and `model.setup.cancel` methods, advertised in
`workbench.info.methods`. Older hosts keep their existing settings entry.
Readiness distinguishes missing routes and required preset credentials from a
configured custom service that intentionally uses no key. A saved configuration
is not presented as a successfully verified connection.

Setup writes carry `expected_revision` and a UUID `operation_id`. The revision
includes the Host lifetime as well as its shared model revision. Applying a
custom profile saves that profile, its active route and active pointer in one
settings-file commit; the older profile-save method still only saves. Root-wide
serialization, existing route checks and secret handling remain in core. On a
concurrent edit, refresh the configuration before applying the retained form.

Checks are direct, single-attempt requests through the selected adapter, with
no retry wrapper, tools, project context, journal entries or capability changes.
They release the application lock before contacting the provider. Cancellation
wins over late success. Status returns bounded classifications rather than raw
provider text that could echo credentials. A result for a changed model is stale.
Repeated identical operation IDs return the existing receipt; changing the
payload under the same ID is rejected. Receipts are private Host memory, capped
at 1,024 operations per Host lifetime without eviction or automatic replay.
