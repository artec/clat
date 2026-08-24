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

Rust developers can therefore implement the same language-neutral behavior as
a DSH plugin: tools, prompts, sampling, elicitation, host context, filesystem,
shell, and the documented read-only session/agent mirrors. They do not
implement the TypeScript `apply(ctx)` source ABI. A portable Rust marketplace
package should compile to WASM/WIT; CLAT does not expose a dynamic-library ABI,
because Rust compiler ABIs are not stable enough for a durable plugin market.

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
same publisher key signed the package; it does **not** mean CLAT or a future
market has reviewed or trusted that publisher. Publisher onboarding, key
rotation and revocation remain market policy.

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

## Marketplace readiness and remaining work

The repository now has the runtime contract, package identity, bounded and
transactional local install/update/rollback/uninstall, complete-tree digest
binding, capability review, optional publisher signatures, trust labels,
config validation, failure isolation, DSH port/package tooling and a semantic
compatibility lab. It does **not** yet have a remote catalog, publisher
onboarding/review policy, dependency/update solver, revocation feed,
vulnerability service, or marketplace UI.

A safe marketplace should add those layers in this order:

1. signed immutable remote index records and publisher onboarding;
2. review policy and separate trust labels for sandboxed WASM versus MCP code;
3. dependency/update solving over immutable versions;
4. compatibility evidence tied to exact CLAT/WIT/DSH revisions;
5. revocation and vulnerability feeds;
6. remote download transactions and marketplace UI over the existing local
   activation/rollback core.

See [WASM authoring](wasm.md), [DSH porting](dsh-plugins.md),
[MCP integration](mcp.md), and [architecture](architecture.md).
