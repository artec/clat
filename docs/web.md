# Web search and fetch

## Native Web tools

CLAT includes `web_search` and `web_fetch` in the Rust binary. They work with
any configured conversation model; no MCP process or user-side runtime is
required. Existing Web plugins remain independently configurable.

### Configure

Open **Settings → Web** in the workbench. Save a dedicated DeepSeek API key,
or explicitly choose to use the saved DeepSeek model key for the official
Web API. Reads return configuration status, never credential bytes. Search
and DeepSeek Fetch may incur charges from the provider.

Terminal clients share the same settings:

```sh
# Requires an already-running local Host.
clat web status
clat web key --stdin
clat web fetch http
clat web fetch deepseek
clat web credential model
clat web credential dedicated
clat web key --clear
```

`key --stdin` reads the dedicated key from standard input, with a 4 KiB bound;
the key is not a command-line argument. Inside the TUI, `/web` reports status,
`/web fetch http|deepseek` selects retrieval, and
`/web credential model|dedicated` selects the credential source. Standalone
TUI settings use its existing core owner. Configuration takes effect on the
next call; a call already prepared for permission review retains its snapshot.

### Search and retrieve

`web_search` sends the explicit query to the DeepSeek Search API, with ten
results by default. It does not send the original user prompt or invoke an
auxiliary model. Missing credentials return configuration guidance without a
network request. There is no automatic retry or paid-provider fallback.

`web_fetch` defaults to anonymous HTTP(S). Only public domain-name addresses
are supported: private/special addresses, IP literals, credentials in URLs and
DNS rebinding are rejected. Up to five same-origin redirects are followed;
a cross-origin destination requires another explicit tool call. Requests have
a 30-second deadline and a 5 MiB decoded-body limit. UTF-8 HTML, plain text and
Markdown are supported; scripts, login, cookies and PDF/OCR are not.

Both tools use the ordinary Network permission policy. Results identify
external untrusted material and preserve source URLs, retrieval times and
truncation markers. Direct HTTP reports the observed response status, including
non-success page responses. DeepSeek Fetch does not report the target's status
or final redirected URL: those fields remain `null`, separately from the API
response status. A source reference is evidence of retrieval, not independent
verification of the page's claims.

Long optional content is bounded before the generic tool-result pipeline so
source metadata remains intact. Durable arguments and output use the existing
secret redactor; raw provider error bodies are not exposed.

## Official DSH web search and fetch

CLAT can use the unchanged official DSH web quartet through the MCP adapter:
`@deepseek-ai/dsh-web`, `dsh-web-search-deepseek`, `dsh-web-fetch-http`, and
`dsh-tool-web`, all pinned to **0.2.0-rc.2**. The recipe is under
`sdk/dsh-adapter/examples/official-web`; it requires the static web-service and prompt-query support. The
adapter `0.1.0-rc.3`, published on 2026-10-03, includes these additions. Use
that version or later when distributing the recipe.

The two tools appear as `mcp_<server>_web_search` and
`mcp_<server>_web_fetch`. Both have **Network** effect and use ordinary CLAT
approval rules. The official tool guidance is automatically imported through
the marked DSH system prompt. Startup, discovery and prompt assembly make no
network requests; search and fetch access the network only when invoked.

### Install from the local PWA

1. Open **Plugin Index** in CLAT’s local PWA and select the available **DSH official web search and fetch** package.
2. Review Tools, System instructions and native executable access; enter your DeepSeek API key and confirm **Install**.
3. Ask CLAT to search or fetch a web page. Use the installed card to configure, disable, update or uninstall.

The four upstream packages arrive as one reviewed capability bundle. The user
needs no Node, Bun or build commands. The market release must include a signed
index and an artifact for the current platform; preview entries are not
installable. Keys stay in the local private registry. If using a host-side
`DEEPSEEK_API_KEY` instead, leave the key field blank. Configuration changes
and installation refresh the external runtime after active work finishes.

#### WASM search-only edition

A search-only WASM edition is also available as
`io.artec.dsh-official-web-wasm` (catalog verified 2026-10-06). Its manifest v2
uses the separate signed index at `https://pi.at.cn/v2/`:

```sh
clat plugin market install io.artec.dsh-official-web-wasm --market https://pi.at.cn/v2/ --accept-capabilities
```

The PWA selects that v2 index from the WASM catalog card. It does not provide
arbitrary URL fetch; retain the MCP edition for search and fetch. See
[network WASM packages](plugins.md#network-wasm-components-manifest-v2) for
permissions, configuration and installation details. WASM receives package-private
configuration from the host; its guest has no ambient environment variables.

### Build once, install without a language runtime

Plugin authors or operators build the package on each destination platform.
Node 22.19+ and Bun are build tools; recipients need neither Node nor Bun.
From the CLAT checkout:

```bash
cd sdk/dsh-adapter
npm ci --ignore-scripts
npm run build
cd examples/official-web
npm ci --ignore-scripts
npm test
npm run package -- --out /absolute/new/official-web-package
```

The lockfile pins the npm artifacts and their integrity. Do not substitute
`latest` or a floating `next`: the official packages' `next` was 0.2.0-rc.2
when this recipe was validated, while `latest` was older. Compilation reuses
the adapter's static package-metadata bundler so upstream `createRequire()`
version lookups work without sidecar dependencies. It also disables executable
autoloading of `.env` and `bunfig.toml`, and verifies initialize/tools-list
before publishing the output directory.

The output is a platform-specific executable plus `clat-plugin.json`, with
identity `io.artec.dsh-official-web`. It is a **local/unverified** package;
this recipe does not publish or sign it, or add an installable market entry.
For sharing a release, retain upstream license notices, review the executable,
and use the publisher and signed-market process in [Plugins](plugins.md).

CLAT users inspect the delivered package and install it:

```bash
clat plugin inspect /absolute/path/official-web-package
clat plugin install /absolute/path/official-web-package --accept-capabilities
```

For a private API key, create a private local JSON file containing
`{"apiKey":"YOUR_KEY"}` and install with
`--config-file /absolute/path/private-web-config.json`. This avoids placing the
key in command arguments or shell history. CLAT stores that config in its 0600
package registry; remove the temporary file when no longer needed. A package
without config can instead use an exported `DEEPSEEK_API_KEY`. Restart CLAT
and inspect `/mcp` after installation. `update`, `disable`, `rollback` and
`uninstall` use the existing package lifecycle.

### Executable configuration alternative

An operator can also distribute only the executable. Edit `~/.clat/mcp.json`
with a private editor (protect the file with owner-only access):

```json
{
  "official_web": {
    "command": "/absolute/path/clat-dsh-official-web",
    "env": { "DEEPSEEK_API_KEY": "YOUR_KEY" }
  }
}
```

Use `clat-dsh-official-web.exe` on Windows. No wrapper scripting is required.
For development only, `command: "node"` and `args: ["/absolute/path/bin.mjs"]`
run the same recipe before compiling it.

DeepSeek search uses its own Anthropic Messages endpoint,
`https://api.deepseek.com/anthropic/v1/messages`; it does not use the active
CLAT conversation provider or `DEEPSEEK_BASE_URL`. Advanced operators may set
`DEEPSEEK_SEARCH_BASE_URL` to a trusted compatible base; the API key is sent to
that endpoint. Search is a separate billable provider request, whose cost is
not included in CLAT's conversation-model usage ledger.

### Results and boundaries

- Missing keys return `WEB_PROVIDER_CREDENTIAL_MISSING` with the key reference;
  HTTP authentication failures preserve the status and provider message.
  Missing native search result blocks are errors, never successful empty data.
- Fetch uses the original HTTP provider's public-address checks, pinned DNS,
  same-origin redirect policy and bounds. The original tool converts HTML to
  Markdown; this recipe caps complete rendered output at 100,000 characters.
- The official `WebRuntime` owns provider selection. One usable provider is
  selected automatically; multiple usable providers require an explicit
  `DSH_WEB_SEARCH_PROVIDER` or `DSH_WEB_FETCH_PROVIDER`, set before startup.
  DeepSeek's resolver-backed provider can remain locally “available” even
  without a key, so adding Exa does not establish automatic credential fallback.
- Optional Exa composition is tested with the original Exa package, but is not
  included in the shipped executable. `deepseek-official` and `exa` are its
  provider ids. Unknown or unavailable selections fail explicitly.
- MCP cancellation reaches the provider signal; EOF/project close revokes
  tools/prompts and shuts down the adapter. The MCP process has account-level
  authority; Network annotations do not sandbox it.
- `ctx.agents.currentInitiator()` returns `undefined`: this adapter executes no
  DSH Agent. Read-only CLAT mirrors remain available through get/list/roots.
  The upstream optional DSH request-journal hook is consequently inactive;
  this feature adds no DSH durable-event producer to CLAT.
- DSH tool presentation cards, agent-scoped tool visibility and the DSH
  timeout-policy plugin are not implemented by this recipe. MCP uses its own
  tool-call deadline; the upstream tools' `timeoutMs` metadata does not install
  the DSH timeout policy.
- Office-to-PDF is not bundled: its service needs authorized workspace-file
  access, Typert calls and LibreOffice conversion resources, beyond this web
  recipe.

### Verification

`npm test` in the recipe covers real npm tool/schema/prompt contributions,
key success/error/absence, provider selection, HTML rendering, output bounds,
loopback rejection, cancellation and disposal. HTTP calls in those tests are
mocked; no paid API or public network is used. Complete gates install the
locked recipe and run the CLAT mcp.json/Application tests against a loopback
DeepSeek-shaped fixture.

To verify a compiled artifact through the same CLAT path:

```bash
CLAT_OFFICIAL_WEB_BIN=/absolute/path/clat-dsh-official-web \
  cargo test -p clat-core official_web -- --ignored
# From sdk/dsh-adapter/examples/official-web:
CLAT_OFFICIAL_WEB_BIN=/absolute/path/clat-dsh-official-web npm run test:compiled
```

The local compiled acceptance establishes the current platform only. Real
provider/account access, other-platform binaries and signed-market release
remain separate acceptance steps. See [DSH compatibility](dsh-plugins.md),
[MCP security and lifecycle](mcp.md), and [Plugins](plugins.md).
