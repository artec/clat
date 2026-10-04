# DSH WASM second flavor (D2 author lane)

This directory derives an explicit-scope flavor from the original adapter Shim,
without editing the Bun source or the official package sources. It is an author
lane, not an installed production plugin or a released market package.

The closed adapters provide semantic DNS/HTTP only. `lookup` requires a hostname
that identifies one configured exact origin; ambiguous origins fail. The host
independently enforces its signed capability policy, so guest configuration never
grants access. The original Undici lookup callback must match one private host
resolution. That resolution is consumed once; it is not resolved again. Proxy,
raw pool, process, filesystem and arbitrary module loading are rejected.

Original package metadata is embedded at build time. `import.meta.url` is the
static component identity, not a filesystem location. The guest has an empty
launch environment. API keys must enter as per-invocation configuration, never
as snapshot environment values. No production credentials are used by these checks.

`explicit-scope.mjs` fails on source drift rather than applying an approximate
rewrite. Context closures capture their cleanup scope across awaits. Nested
injections own their tools, events, providers and prompt registrations. Disposal
waits for setup and preserves LIFO and failure isolation. Captured registrations
reject after disposal. Network host services cannot supply process or preopens.

Local checks:

```sh
node --test sdk/dsh-wasm-flavor/network-fetch.test.mjs sdk/dsh-wasm-flavor/pinned-adapter.test.mjs
node sdk/dsh-wasm-flavor/check-explicit-scope.mjs /absolute/new-output-directory
node sdk/dsh-wasm-flavor/build-scope-component.mjs /absolute/pinned-native-engine.wasm /absolute/new-output-directory
PLG4_SCOPE_COMPONENT=/absolute/scope.wasm cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml --locked --offline --lib invocation_explicit_scope_component -- --ignored
node sdk/dsh-wasm-flavor/build-official-component.mjs /absolute/pinned-native-engine.wasm /absolute/new-output-directory
PLG4_OFFICIAL_COMPONENT=/absolute/official.wasm cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml --locked --offline --lib invocation_original_quartet -- --ignored
```

Real DNS checks are omitted by the owner's TUN-environment decision. Scope
component tests are armed explicitly; ordinary Cargo tests leave them ignored.
Componentization or discovery alone is insufficient. D2 consumer acceptance now
also includes real original search/HTTP invocation, physical cancellation, shared
pin vectors and the existing CLAT tools contract. Production installation and
release remain D3 work.

The author `run` wrapper reports caught original-tool failures as a `failure`
packet so runtime diagnostics remain observable with ambient stderr disabled.
A failure packet is a failing acceptance result, not a successful tool result.

The pinned engine also lacks `URL.canParse` and mishandles the iterable argument
of `AbortSignal.any`. The flavor supplies their Web-standard semantics from the
existing URL parser and AbortController; it adds no host capabilities. Native
function-source whitespace is canonicalized so the original tool schema's
intrinsic-constructor witness works on SpiderMonkey. User function bodies are
unchanged. Provider DNS receives the original HTTP deadline signal through the
provider's existing explicit resolver parameter.

The original CLAT tools/config WIT is mirrored byte for byte under `plugin-wit`;
the builder rejects contract drift and an unapproved engine digest. Build the
actual tools export with the fourth argument `tools`:

```sh
node sdk/dsh-wasm-flavor/build-official-component.mjs /absolute/pinned-native-engine.wasm /absolute/new-output-directory tools
PLG4_TOOLS_COMPONENT=/absolute/official.wasm cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml --locked --offline --lib original_quartet_uses_existing_tools_contract -- --ignored
PLG4_OFFICIAL_COMPONENT=/absolute/author-run-official.wasm cargo test --manifest-path sdk/wit-proposals/http-consumer-host/Cargo.toml --locked --offline --lib invocation_original_http -- --ignored
node --test sdk/dsh-wasm-flavor/*.test.mjs
```

The HTTP consumer fixture projects public test addresses and dials fixed numeric
loopback only under `cfg(test)`. It preserves origin/port/authority and checks
actual requests and connection EOF. It is not public DNS or dial acceptance.
The shared `pin-cases.json` is consumed by both the unchanged original provider
and the Rust host; disabling complete-set validation makes its host test fail.
D2 is ready for independent audit. D3 still owns production loading, signed
market packages, staging, encoding compatibility and repeated public-provider
measurements. The existing Bun/MCP fallback remains in use.
