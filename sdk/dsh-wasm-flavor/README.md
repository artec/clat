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
D2 has passed independent audit. The D3 production integration and local
acceptance below are complete; independent review and owner publication remain
separate. The existing Bun/MCP fallback remains in use.


## D3 search-only release candidate

The production loader accepts the formal tools/config world and signed manifest
v2 through clat-wasm-net. The first candidate exports only web_search, with POST
restricted to https://api.deepseek.com:443 and a separate clock capability. The
four upstream dependencies remain unchanged. The original MCP edition remains
the full search/fetch option. The shared HTTP host supports identity, gzip
(including multiple members), zlib/raw deflate and strict Brotli. Both encoded
and decoded entities are bounded to 8 MiB, with a 128 MiB shared reservation
budget and cancellation/deadline checks during decoding. Unsupported or stacked
codings and malformed or oversized compressed entities fail closed.

```sh
node sdk/dsh-wasm-flavor/build-official-component.mjs /absolute/pinned-native-engine.wasm /absolute/new-search-build search
node sdk/dsh-wasm-flavor/package-release.mjs /absolute/new-search-build/official.wasm /absolute/new-package
node web/e2e/plugin-network-staging.mjs --package /absolute/new-package --clat /absolute/clat
PLG4_SEARCH_COMPONENT=/absolute/new-search-build/official.wasm PLG4_SEARCH_SAMPLES_OUT=/absolute/new-fixture-report.json cargo test -p clat-wasm-net original_search_only_formal_contract_repeated_samples -- --ignored --nocapture
```

Staging creates and deletes private ephemeral signing keys. Its numeric route
is limited to the test host, preserving actual core DNS/HTTP approvals and
public-IP validation. Ordinary product builds have no environment-driven
fixture route. The generated display proposal selects the signed v2 endpoint;
the signed legacy index is not rewritten.

Live measurements require a private owner-provided JSON config containing
apiKey. Never put key contents in commands or environment variables. External
A and AAAA JSON answer sets replace system DNS only in the explicitly armed
test; full public-IP validation, production numeric connector and TLS remain
active. This is real provider evidence, not system DNS acceptance.

```sh
PLG4_LIVE_CONFIG=/absolute/private-config.json PLG4_LIVE_A=/absolute/public-a.json PLG4_LIVE_AAAA=/absolute/public-aaaa.json PLG4_SEARCH_COMPONENT=/absolute/official.wasm PLG4_SEARCH_SAMPLES_OUT=/absolute/new-live-report.json cargo test -p clat-wasm-net original_search_only_live_provider_repeated_samples -- --ignored --nocapture
bun sdk/dsh-wasm-flavor/measure-bun-search.mjs /absolute/private-config.json /absolute/new-bun-report.json /absolute/original-compiled-executable
```

Both probes run five real queries. Reports contain fuel, time, size and result
byte counts, with no keys or result contents. Cold component compile time is
separate from tool wall time. Service/network variation precludes a performance
guarantee. Formal signing, production upload and independent owner acceptance
remain separate from local staging.

The same six encoding vectors are consumed by the unchanged original HTTP
provider under Node and by the actual formal WASM tools component:

```sh
node --test sdk/dsh-wasm-flavor/encoding.test.mjs
PLG4_TOOLS_COMPONENT=/absolute/full-tools/official.wasm cargo test -p clat-wasm-net original_formal_http_component_decodes_shared_wire_vectors -- --ignored --nocapture
```

Direct `bun test` with Bun 1.3.14 currently fails in the original provider's
undici Agent disposal (`dispatcher.close` is unavailable). This baseline
failure is preserved in the delivery record; the original Bun/MCP flavor has
not been patched, and Node results are not labeled Bun HTTP acceptance.
The staging command above additionally runs the complete signed market matrix:
consent, disabled state, rejected signatures/expiry/digests, configuration,
concurrent writer, capability expansion, update, rollback, reopen and uninstall.
