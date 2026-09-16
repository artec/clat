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
| DeepSeek V4.1 Flash | `deepseek-flash` | `https://api.deepseek.com` | 1M | 384K |
| DeepSeek V4.0 Pro | `deepseek-v4-pro` | `https://api.deepseek.com` | 1M | 384K |
| GLM 5.3 | `glm-5.3` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| GLM 5.3 Flash | `glm-5.3-flash` | `https://open.bigmodel.cn/api/coding/paas/v4` | 1M | 128K |
| Qwen3.8 Max | `qwen3.8-max` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Qwen3.8 Flash | `qwen3.8-flash` | `https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1` | 1M | 128K |
| Kimi K3 | `kimi-k3` | `https://api.kimi.com/coding/v1` | 1M | 128K |
| Kimi for Coding | `kimi-for-coding` | `https://api.kimi.com/coding/v1` | 1M | 128K |
| Hy 4 Preview | `hy4-preview` | `https://api.lkeap.cloud.tencent.com/plan/v3` | 1M | 64K |
| Hy 3 | `hy3` | `https://api.lkeap.cloud.tencent.com/plan/v3` | 256K | 128K |

All use the OpenAI-compatible protocol. The catalog also owns request paths,
reasoning parameters, usage streaming, context-window seeds, and vendor-specific
headers. For example, GLM preserves thinking, Qwen uses its
`low`/`medium`/`xhigh` ladder, the Kimi Coding endpoints require a
whitelisted coding-agent User-Agent, and the Hy presets always think
server-side (no ladder). See [Providers](providers.md#built-in-presets)
before overriding those fields.

GLM 5.3 Flash is currently the only built-in preset with probe-verified native
image input; the officially-declared vision presets — `deepseek-flash`,
`qwen3.8-max`, `qwen3.8-flash`, `kimi-k3`, and `kimi-for-coding` — and
GLM 5.3 Flash all enable
the image attachments described in
[Using CLAT](usage.md#image-attachments). All other presets remain text-only
in CLAT. This does not restrict images handled wholly inside configured MCP
tools. DeepSeek note (2026-09-10): the official V4.1 Flash release retired
`deepseek-v4-flash` and `deepseek-v4-flash-vision-exp`; the preset was
upgraded in place to `deepseek-flash` (vision slot inherited), and saved
configurations referencing the old ids simply stop resolving to a preset.

Most presets also declare a companion utility model used only for automatic
session naming and manual prompt suggestions: `deepseek-v4-pro` uses
`deepseek-flash`, `glm-5.3` uses `glm-5.3-flash`, `qwen3.8-max` uses
`qwen3.8-flash`, `kimi-k3` uses `kimi-for-coding`, and `hy4-preview` uses
`hy3`; every other preset uses the primary model itself. See
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

## Web workbench settings

Open **Workbench settings → Models** to choose a vendor-grouped preset or
create, edit, activate, and delete a custom profile without closing the host.
Model settings are shared by every mounted project. A change is broadcast to
online clients; already-running requests retain their frozen model configuration.

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
`model.preset.select`, plus `model.thinking.cycle`; the secret-free notification is `models`.
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
