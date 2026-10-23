# Interface languages

CLAT includes English (`en`), Simplified Chinese (`zh-Hans`) and Traditional
Chinese (`zh-Hant`). With no saved choice it follows the system or browser;
unsupported languages fall back to English. An interface language changes CLAT
labels and explanations, not model answers, prompts, tool definitions, external
content, protocol identifiers, or diagnostic details supplied by other systems.

## Choose a language

Put the global option before the subcommand:

```sh
clat --lang zh-Hans --help
clat --lang zh-Hant exec --help
clat language
clat language zh-Hans
clat language system
```

Terminal precedence is `--lang`, then `CLAT_LANG`, then the saved terminal
preference, then system languages, then English. `--lang system` explicitly
follows the system, bypassing the environment and saved choice. The option is
not parsed inside prompts, tool arguments, subcommands, or after `--`.
`clat language` reads or saves presentation preferences without opening a Host
or requiring project trust. Invalid explicit choices are usage errors; invalid
environment values or damaged preferences produce a warning and fall back
without replacing the damaged file.

In the TUI use `/language`, `/language en`, `/language zh-Hans`,
`/language zh-Hant`, or `/language system`. `Ctrl+Shift+L` opens the chooser
while another dialog or a run is active. These are local UI commands, including
in attached and DSH terminals; they never reach a model or external Host.
Already running terminals keep their own choice until explicitly changed.

The workbench has a language chooser in settings and on the connection page.
A browser stores its own preference. Inside CLAT's desktop app the desktop
preference takes priority over browser storage; the welcome page, workbench,
native menus, notifications and updater use that same context. Changing it
preserves the update stage and does not initiate another update operation.

Switching languages updates existing UI text without a page reload, Host
restart or task interruption. Draft text, attachments, current interactions
and queued work remain in place. Permission or plugin-consent content that
requires review must be reviewed again after its layout changes.

Terminal preferences live at `~/.clat/ui/terminal.json` with a separate lock.
Desktop UI preferences merge language and automatic-update preferences in its
existing app configuration file. Browser preferences extend
`clat.presentation.v1` in local storage. None of these settings is a shared
Host business setting. JSON/NDJSON, script-facing fields, exit codes and journal
facts retain their existing contracts.

## Maintain translations

`locales/manifest.json` owns supported identifiers, native names and aliases.
English regional variants map to English. `zh`/CN/SG map to Simplified Chinese;
TW/HK/MO map to Traditional Chinese; an explicit Hans/Hant script wins over a
region. Detection uses `sys-locale` for terminals and native desktop, and
`navigator.languages` in browsers.

The three JSON catalogs in `locales/` are the only translation authority.
Use stable keys, complete sentences and named parameters. A quantity entry has
exactly `one` and `other` forms with matching parameters. English uses `one`
for a count of one; Chinese uses `other`. Keep command names, flags, permission
values and confirmation tokens unchanged. Parameters are opaque text, never
HTML or another template. Catalogs cannot contain HTML or executable code.

Rust embeds the resources and renders with an explicit `LocaleContext`.
Keep `UiMessage` descriptors or raw view data until rendering; do not save a
translated sentence in journal facts. A `PresentedText` retains the original
English text and optional presentation metadata. Described parameters use the explicit
`arg_presentations` map; ordinary JSON parameters are never interpreted as messages.
Nested presentation is bounded to eight levels, then uses the English argument.
RPC errors and command results
may carry an optional `presentation` alongside their existing facts and
admission receipt. Background notices, workflow/process retention explanations, file
previews and workspace/captured-operation notes can carry the same descriptor
alongside the existing English fields. Unknown keys use the supplied English fallback; unknown
external text remains original. Do not recognize translations by comparing
English error strings.

Web and desktop use the same small `web/i18n.js` runtime. Bind text and
attributes on existing nodes; do not rebuild a form when the language changes.
Use `CLATI18n.msg` for retained messages and `CLATI18n.bind` for values computed
at render time. Keep operation identifiers separate from translated labels.
The runtime contains offline English so a failed resource fetch cannot prevent
connection or sending. It never sends language changes to business RPCs.

After editing installer messages, regenerate the embedded subsets used by the
standalone shell and PowerShell installers:

```sh
bun scripts/i18n-installers.ts
bun scripts/i18n-check.ts
bun scripts/i18n-installers.ts --check
bun test scripts/i18n-check.test.ts scripts/i18n-installers.test.ts
```

The check rejects duplicate or missing keys, empty entries, invalid templates,
parameter or quantity-form drift, unsafe manifest aliases and unknown literal
translation references. It does not prove that every product text has been
extracted or that a translation is idiomatic; review the actual user flow and
check Traditional Chinese terminology separately. Build, signing and publishing
scripts intentionally keep their existing development output.

## Extraction boundaries

Product labels, client status and progress, built-in command usage, permission
explanations, model-editor validation, recovery explanations and installer
messages belong in the catalogs. Keep their descriptors while a view is alive.
Do not translate model or tool descriptions sent to a provider, assistant/user
text, plugin-supplied schema titles, upstream Host messages, process output, file
contents, patches, evidence references or durable reasons. These are original
content or technical diagnostics. Protocol/model names (including OpenAI
Compatible and OpenAI Responses), command/flag names, permission values, state
identifiers, hashes, versions, paths, confirmation tokens and script-facing table
headers remain stable. UI labels around such values can still be localized.

Raw OS, transport, parser, storage, security-validation and provider diagnostics
remain available unchanged beside localized client explanations. Unknown optional
metadata falls back to that original diagnostic; it must not affect execution,
retry, cancellation, receipt or permission classification. Do not add a
translation by matching an English diagnostic. Development, build, signing and
publishing script output is excluded from this language feature.

Attached terminal information dialogs retain the original Host value and use
`HostClient::info_dialog_text_localized` only when drawing. Language changes do
not refresh `/context`, MCP or skills from the Host. Empty or malformed optional
presentation metadata cannot hide the original diagnostic. Static HTML defaults
initialize a binding once; a later status/attribute binding owns that node, so a
language refresh cannot turn an online connection into a connecting label or
replace the current permission mode with a placeholder. CSS classes, action IDs
and other selectors never come from translated labels.
