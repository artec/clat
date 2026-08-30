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

## Built-in presets

The shipped catalog is the source used by both the picker and runtime:

| Preset | Model id | Endpoint | Context | Max output |
|---|---|---|---:|---:|
| DeepSeek V4.0 Flash | `deepseek-v4-flash` | `https://api.deepseek.com` | 1M | 384K |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` | `https://api.deepseek.com` | 1M | 384K |
| DeepSeek V4.0 Flash Vision (Exp) | `deepseek-v4-flash-vision-exp` | `https://api.deepseek.com` | 1M | 384K |
| GLM 5.3 | `glm-5.3` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| GLM 5.3 Flash | `glm-5.3-flash` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| Qwen3.8 Max | `qwen3.8-max` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Kimi K3 | `kimi-k3` | `https://api.kimi.com/coding/v1` | 1M | 128K |

All use the OpenAI-compatible protocol. The catalog also owns request paths,
reasoning parameters, usage streaming, context-window seeds, and vendor-specific
headers. For example, GLM preserves thinking, Qwen uses its
`low`/`medium`/`xhigh` ladder, and the Kimi Coding endpoint requires a
whitelisted coding-agent User-Agent. See [Providers](providers.md#built-in-presets)
before overriding those fields.

GLM 5.3 Flash is currently the only built-in preset with probe-verified native
image input, and therefore the only picker route that enables the image
attachments described in [Using CLAT](usage.md#image-attachments). The
DeepSeek Vision experimental preset and all other presets remain text-only in
CLAT until their exact route has equivalent evidence. This does not restrict
images handled wholly inside configured MCP tools.

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
| Spend Budget | 1M, 10M (default), 50M, off, Custom… |

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

**Spend Budget** limits total input+output tokens for one run. The default is
10M and `0` disables it. Despite the label, its unit is tokens, not money.
Every model or extension-sampling request reserves a
conservative amount before it starts; provider usage reconciles the reservation
when available. Malformed or absurd usage cannot wrap or silently reset the
ledger. Crossing the cap stops the run with used/cap values and a pointer back
to `/model`.

The spend value belongs to the model configuration, so different profiles can
carry different risk/cost limits.

## Provider replay data

Some providers require hidden state on later turns. The editor does not expose
or persist that state as configuration:

- OpenAI Responses reasoning items travel as opaque provider replay state;
- DeepSeek `reasoning_content` is replayed only on assistant turns that made
  tool calls, which is the wire shape required for multi-turn tool use.

This data belongs to the session journal and provider adapter. Switching a
profile changes future requests but does not rewrite historical events.
