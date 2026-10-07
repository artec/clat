# CLAT plugins and package model

CLAT has one plugin product surface with multiple delivery runtimes. The goal
is a future CLAT marketplace that can index both DSH-compatible packages and
CLAT-native packages without pretending that JavaScript and Rust share an ABI.

## Runtime choices

| Authoring/runtime | Boundary | Best for |
|---|---|---|
| Rust built-in `Plugin` | statically linked catalog | CLAT core and audited first-party services |
| Rust → WASM component | `clat:plugin@0.1.0` WIT | portable CLAT-native third-party plugins |
| DSH/Cordis adapter | MCP stdio + CLAT host extension | reusing existing DSH TypeScript plugins |
| General MCP | stdio or Streamable HTTP | services in any language or hosted capabilities |

The [official DSH web recipe](web.md) composes pinned, unchanged npm packages
and builds a standalone MCP executable plus manifest for runtime-free installs.

Rust developers can therefore implement the same language-neutral behavior as
a DSH plugin: tools, prompts, sampling, elicitation, host context, filesystem,
shell, and the documented read-only session/agent mirrors. They do not
implement the TypeScript `apply(ctx)` source ABI. A portable Rust marketplace
package should compile to WASM/WIT; CLAT does not expose a dynamic-library ABI,
because Rust compiler ABIs are not stable enough for a durable plugin market.

### Built-in agent capability plugins

First-party agent capabilities use the same static `Plugin`/service/lease
lifecycle as other built-ins. They are independently removable catalog entries,
not UI special cases. The current workflow/intelligence set is
`builtin.plan_mode`, `builtin.skills`, `builtin.language_intelligence`,
`builtin.context_inspector`, `builtin.memory`, `builtin.goal`, and
`builtin.subagent`. Omitting one removes its commands/tools/service; closing the
Trusted Project scope revokes its leases and joins owned workers.

This internal packaging does not make a capability a downloadable marketplace
package. Memory, goal, and subagent currently depend on CLAT-owned Rust runtime
interfaces and durable-event gates; they may become portable only after those
contracts have an interoperable external ABI.

## One semantic host

MCP server requests and WIT imports terminate in `PluginHostBridge`. Transport
code only translates JSON-RPC or component values. The bridge owns:

- active-run context lifetime and bounded session metadata;
- sampling permission, budget, cancellation, and usage accounting;
- elicitation through the frontend-neutral question port;
- the native host-tool allowlist and its permission/path/pipeline execution.

This keeps DSH compatibility useful for the future native ecosystem: a new
host capability is designed once and then projected into MCP and WIT.

## Package manifest v1

`clat-plugin.json` is language-neutral distribution metadata. Its schema is
`schemas/clat-plugin-manifest.schema.json`. It binds:

- stable package id, display name, and version;
- runtime kind, package-relative entry, arguments, and SHA-256;
- declared tools, prompts, sampling, elicitation, context, and host tools;
- static system-prompt contributions;
- configuration schema;
- optional compatibility provenance such as a pinned DSH revision.

Runtime kinds are `wasm-component` and `mcp-stdio`. Both are installed and
activated by the same local package store. A WASM entry enters Wasmtime; an MCP
entry is a package-relative executable launched out of process. CLAT never
runs package install hooks and never requires an end-user language runtime.
DSH authors can use `clat-dsh package` to compile their adapter and dependencies
into one Bun executable.

Package ingestion rejects unknown fields, identity or digest mismatch,
directory and symlink escape, special files, oversized trees, undeclared
prompts/tools, invalid required configuration, malformed registry state, and
tampered installed content before guest execution.

A malformed/unknown-version registry is a global activation-control failure and
fails closed. A bad artifact behind one otherwise valid record is isolated:
that package is reported in `/mcp`, while other verified packages of the same
runtime continue to mount. `clat plugin list` reports its health error;
`disable` and pointer-first `uninstall` remain available as recovery actions,
while `enable`, update and rollback always verify the bytes they would expose.

## Local package lifecycle

Inspecting is read-only:

```bash
clat plugin inspect ./my-plugin
```

Installation and the first capability-bearing activation require explicit
review:

```bash
clat plugin install ./my-plugin --accept-capabilities
clat plugin install ./my-plugin --config-file ./plugin-config.json \
  --accept-capabilities
```

`--config-json` is also available for non-secret automation. Prefer the bounded,
non-symlink `--config-file` path for credentials so values do not enter shell
history or process arguments. The resulting JSON is stored only in the 0600
registry and capped at 64 KiB.

Lifecycle commands are:

```text
clat plugin list
clat plugin update <package-dir> [--accept-capabilities]
clat plugin disable <id>
clat plugin enable <id>
clat plugin rollback <id>
clat plugin uninstall <id>
```

An update with the same or a narrower capability set needs no redundant
acceptance. Any newly enabled capability or host-tool name stops before
activation and lists the expansion. Rollback swaps the active and previous
already-reviewed activations, including their private configuration.

The immutable artifact tree is copied under `~/.clat/plugin-store/artifacts`
and addressed by a deterministic complete-tree SHA-256. `registry.json` is the
only active/enabled pointer. Artifact copy, digest verification and fsync
finish before one atomic registry replacement commits activation. A failed or
interrupted update therefore leaves the old version active; stale staging is
inert and removed under the storage-root lease. The registry retains the
active and one rollback activation; older unreferenced trees are reclaimed on
the next package-store mutation.

Uninstall commits pointer removal before deleting artifact bytes. A cleanup
failure can leave an unreachable directory but cannot leave half-active code.

## Trust and signatures

Unsigned directories are displayed as `local/unverified`. A signed directory
contains both:

- `clat-plugin.publisher.json` — schema version 1, publisher id and Minisign
  public key;
- `clat-plugin.minisig` — signature over the canonical package identity and
  complete content tree (excluding the signature file itself).

A valid self-contained signature is displayed as `publisher/verified` and is
reverified from immutable installed bytes at activation. This proves that the
same publisher key signed the package; by itself it does **not** mean the CLAT
market reviewed that publisher. Remote installation additionally requires the
signed market index to name the exact publisher and key as trusted and valid at
the package publication time.

An in-place update must retain the exact publisher id and key (including the
unsigned `local/unverified` identity). A publisher/key change or signed-to-
unsigned downgrade requires explicit uninstall followed by a fresh install;
ordinary update can never switch identity silently.

`clat-dsh package` can produce this format with `--publisher`,
`--publisher-key`, and `--minisign-key`.

## Legacy override files

`~/.clat/plugins.json` and `~/.clat/mcp.json` remain supported escape hatches.
When a user-managed entry has the same id as an installed package, the user
entry wins and installed manifest prompts/config do not leak into the override.
The exclusion happens before artifact activation verification, so an explicit
override is also a recovery path for a damaged installed artifact; all other
enabled installed packages remain verified normally.
Installed MCP processes receive package identity/trust plus private config in
`CLAT_PLUGIN_*` environment fields and run with their immutable artifact root
as cwd. User-configured MCP entries may set an explicit `cwd`; a relative value
is resolved under `~/.clat`, never under the untrusted project.

## Signed remote market

The official market origin is [pi.at.cn](https://pi.at.cn). Its human catalog
and website are static; `index.json` plus `index.json.minisig` are the
machine-readable source. The production trust anchor is the same embedded
Minisign public key used for CLAT releases. The index signature's trusted
comment binds the market id and generation timestamp, and each index expires
within fourteen days. CLAT accepts HTTPS only, except loopback HTTP for local
testing.

The schema is `schemas/clat-plugin-market-index.schema.json`. It carries:

- reviewed publishers, active/retired/revoked keys and validity windows;
- immutable package versions, capabilities, compatibility and dependencies;
- target-specific or `any` content-addressed `.clatpkg` artifacts;
- yanks, timed package/version/artifact revocations and vulnerability records.

Publisher onboarding requires a stable publisher id, a Minisign public key, a
review record, and separate review of runtime risk. A capability-bounded WASM
component and an unrestricted `mcp-stdio` executable can share the market but
must not receive the same review conclusion merely because both signatures are
valid. Rotation adds the new key before it is used and retires the old key;
compromise marks the key revoked and adds affected artifact revocations.

Browse without executing package code:

```text
clat plugin market list
clat plugin market search <query>
clat plugin market info <id>
clat plugin market audit
```

`audit` compares installed publisher keys and versions with the current signed
publisher, revocation and vulnerability records. It reports; it never silently
disables a working local plugin merely because the network or market is
unavailable. Operators decide whether to disable, roll back or uninstall.

Install or update from a mirror/staging index by adding `--market <HTTPS-URL>`:

```bash
clat plugin market install <id> --version '^1.2.0' --accept-capabilities
clat plugin market update <id> --accept-capabilities
```

Both commands accept `--config-json <json>` or `--config-file <path>` for
package-private configuration. Prefer a private file for credentials so values
do not enter shell history. See the configuration rules above.

`market info` displays the market summary but omits v2 network/clock
capabilities; `clat plugin inspect <package-dir>` displays them from the manifest.

Supported dependency constraints are `*`, an exact three-component SemVer,
`^`, `~`, and space/comma-separated `>`, `>=`, `<`, `<=` comparisons. The
solver deterministically chooses the highest compatible non-yanked version for
the current CLAT and target. Missing versions, conflicts, dependency cycles,
revocations and unknown range syntax fail closed. A known vulnerability also
blocks installation; `--accept-vulnerabilities` is an explicit emergency
override, not a global preference.

The index and signature cache is usable only after signature and expiry checks.
Artifacts are streamed with advertised length and outer SHA-256 checks. CLAT
then validates the `.clatpkg` file table, extracts with non-overwriting paths,
checks each file digest, and reuses the manifest, complete-tree and publisher
signature verification from local install. All transitive dependencies finish
those checks before one registry replacement activates the complete graph.

## `.clatpkg` bundles and publishing

Authors or market operators can build a deterministic container without a
language runtime or install hook:

```bash
clat plugin inspect ./my-plugin
clat plugin pack ./my-plugin --output my-plugin-1.0.0-any.clatpkg
```

The format begins with `CLATPKG1`, a bounded JSON file table, then sorted raw
file bodies. Paths are UTF-8 relative paths; absolute/parent paths, duplicate
entries, symlinks, special files, truncation, trailing data, oversized files
and digest mismatch are rejected. The market index binds the entire container
length and SHA-256; the package signature binds its identity and inner content.

The independently deployable site and signed-index release tooling live in
`market/`; its deployment runbook is `market/README.md`. As verified on
2026-10-06, `io.artec.dsh-official-web` and `io.artec.dsh-official-web-wasm`
are `available` on pi.at.cn; the other catalog entries remain `preview`.
A preview does not become remotely installable until a reviewed publisher,
signed index record and immutable artifact are deployed together.

## Install and manage from the PWA or TUI

Run `/plugin` in the host-attached TUI, or open **Plugin Index** in the local PWA, choose an available package, review its
permissions and native-process risk, enter any configuration, and select
**Install**. The same panel lists installed packages with Configure, Disable,
Enable, Update, Roll back and Uninstall controls. Changes become available in
all mounted projects without restarting CLAT; finish active runs or compaction
before preparing or committing changes, and wait for plugin startup to settle.
The dialog scrolls when its installed packages, review form or public catalog
exceed the available screen height, including on mobile screens.

The public catalog is a display surface. Its cross-origin request uses no
credentials or local Bearer token. Download, signature verification, publisher
trust checks, capability consent and installation belong to the authenticated
local host. The public website cannot install or remove anything. Unsupported
platforms, revoked publishers, expired indexes and known vulnerabilities fail
closed; vulnerability overrides remain an explicit CLI operation.

Preparing an install downloads and verifies the complete dependency solution
without running it. The host allows at most eight pending `plugin.prepare`
reviews. At that limit, cancel an unused review before preparing another. A single-use,
15-minute review ticket binds the package
trees and installed-registry revision. Commit rechecks the index lifetime and
publisher identities; changed state requires a new review. Cancelled or expired
reviews never activate a package. Enable, rollback and configuration changes
also require consent; disable and uninstall revoke contributions before
registry publication and artifact cleanup.

Configuration values are write-only in the UI and travel only in a local POST
body into the owner-only package registry. The form is cleared on submission or
close; values never appear in management replies, the catalog, browser storage
or installation events. Authors should annotate secret schema properties with
`writeOnly: true` (or `format: "password"`) and provide a clear title and
description. Flat object properties have a guided form; other shapes accept
JSON. On update, leave configuration blank to retain existing values; entered
configuration replaces it. Configure replaces the complete object.

Installed packages continue to work with the market offline. Configure, Enable,
Disable, rollback and Uninstall use local state only. Runtime startup failures
are reported separately from a committed installation; inspect MCP status and
configuration if a package starts unsuccessfully.

The additive local RPC amendment adds `plugin.list`, `plugin.market`, `plugin.prepare`,
`plugin.commit`, `plugin.cancel` and `plugin.remove`. These are host-scoped
methods, independent of session selection, journal events and `RunEvent`.
`plugin.prepare` takes an id and action (`install`, `update`, `enable`,
`rollback`, `configure`); `plugin.commit` takes the returned ticket,
`accept_capabilities: true` and optional `configs` keyed by package id.
`plugin.remove` takes an id and `disable` or `uninstall`. A lost response is
reconciled with `plugin.list`, never an optimistic browser installation cache.

See [WASM authoring](wasm.md), [DSH porting](dsh-plugins.md),
[MCP integration](mcp.md), and [architecture](architecture.md).

`plugin.market` returns a read-only verified package projection for the requested
`market_generation` (1 or 2). `command.run` returns `kind: "plugin_manager"` for
`/plugin`; each frontend opens its own management view. A caller may optionally
provide a UUID `ticket` to `plugin.prepare`, allowing cancellation even if its
prepare acknowledgement is lost. Omission preserves the previous behavior.
TUI review ownership is cancelled on close, expiry, stale responses and graceful
exit; abrupt process termination still relies on the host's review expiry.


## Network WASM components (manifest v2)

Machine-readable schemas for manifest v2 and the v2 market index are not yet
available. The two files under `schemas/` describe v1 only and reject v2; do
not use them to validate v2 packages or indexes.

Manifest v2 selects a separate semantic HTTP/DNS runtime. Each invocation gets
fresh guest state, no filesystem preopens, environment, sockets or host tools.
The signed `capabilities.network` declares exact scheme/hostname/port and HTTP
methods using `clat:net-task@0.1.0`; `capabilities.clock` can explicitly declare
`wasi:clocks@0.2.10`. Every outbound operation also uses the current run's Network
permission policy. Cancellation, policy changes and unloading revoke authority.
This first runtime exports tools/config only; sampling, elicitation, prompts and
host context are rejected at installation rather than silently granted.

Optional configuration `networkPolicy` narrows these declarations; it cannot
expand them. New origins or methods in an update require capability consent.
Reopening, configuring and rolling back retain the signed capability ceiling.
V1 components continue to use the original runtime and linker. During v2
mounting, tool discovery runs in a separate, network-free Discovery scope with
a 30-second total budget; discovery does not grant invocation network authority.

The first official DSH WASM release is a search-only variant with independent id
`io.artec.dsh-official-web-wasm`. Its network ceiling is DeepSeek's declared API
origin. Arbitrary URL fetching remains available through
`io.artec.dsh-official-web`, the existing Bun/MCP package. Original upstream
packages are consumed unchanged by both flavors. The WASM release passed
independent final review on 2026-10-05 and was published on pi.at.cn; its catalog
status was verified as `available` on 2026-10-06.

Network packages use a separate signed v2 index at `https://pi.at.cn/v2/`:

```sh
clat plugin market install io.artec.dsh-official-web-wasm --market https://pi.at.cn/v2/ --accept-capabilities
```

The PWA consent card displays the verified network origins and methods. Its
catalog's `manifestVersion: 2` selects that fixed index; catalog data cannot
supply arbitrary download or trust URLs. The ordinary v1 index remains separate.


### Startup verification and compiled-code cache

Every installed package is fully verified against its manifest, directory tree,
registry and publisher signature before activation. The tree scan also validates
the executable digest; MCP mounting consumes that activation-local evidence and
rechecks file identity and change timestamps. Windows additionally retains a final full digest check because its timestamps
can be explicitly rewritten. Verification is never postponed
until after executable code has started. ARM hosts detect SHA-256 instructions at
runtime and retain a software fallback; x86 keeps its existing SHA/AVX2 path.

Both WASM interfaces reuse compiled code in a private
`.clat-wasm-cache-<storage-id>` directory beside the storage root. Entries bind
the complete component bytes and Wasmtime engine compatibility. A private local
host key authenticates the complete compiled artifact before deserialization;
modified, truncated, foreign or incompatible entries are cache misses and are
recompiled. Publication is atomic. Each authenticated compiled-cache entry is
capped at 256 MiB. Cache failures, including a read-only storage
root, fall back to compilation. Network invocations get fresh instances; legacy
plugins retain their existing permission-bound instance lifecycle. Configuration,
resource limits and permission checks remain per runtime use; the cache stores
no credentials, grants or guest state. The cache assumes the same protected local
user storage boundary as CLAT's credentials; code running with the host user's
full filesystem privileges is outside that boundary. WASM filesystem preopens
may not expose this directory or an ancestor; such grants fail before execution
or write-grant persistence. Ordinary project and storage-root grants remain
available. The cache can be deleted
while CLAT is stopped; the next activation rebuilds it. First compilation remains
slower than subsequent activations.
