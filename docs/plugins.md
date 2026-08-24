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

Runtime kinds are `wasm-component` and `mcp-stdio`. The current CLAT loader
installs manifest-backed WASM packages through `~/.clat/plugins.json`; MCP/DSH
packages still launch through `~/.clat/mcp.json`. Keeping both kinds in the
same schema establishes the index/signing identity needed by a marketplace
without prematurely adding an installer or executing arbitrary package hooks.

Manifest-backed WASM loading rejects unknown fields, id/key mismatch,
directory escape, malformed digest, digest mismatch, undeclared prompts/tools,
and invalid required configuration before guest execution. One bad package is
reported in `/mcp` and does not prevent other packages from loading.

## Marketplace readiness and remaining work

The repository now has the runtime contract, package identity, digest binding,
capability declarations, static prompts, config validation, failure isolation,
and a DSH compatibility scanner. It does **not** yet have a remote catalog,
download/install transaction, publisher signatures, review policy, update
solver, revocation feed, or marketplace UI.

A safe marketplace should add those layers in this order:

1. signed immutable package/index records and publisher identity;
2. transactional install/update/rollback with digest verification;
3. capability and config review before activation;
4. separate trust labels for sandboxed WASM and out-of-process DSH/MCP code;
5. compatibility evidence tied to exact CLAT/WIT/DSH revisions;
6. revocation, vulnerability, and deterministic rollback paths.

See [WASM authoring](wasm.md), [DSH porting](dsh-plugins.md),
[MCP integration](mcp.md), and [architecture](architecture.md).
