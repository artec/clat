# DSH-in-WASM experiment (PLG-3)

This private author-side experiment tests the **unchanged** official DSH web
quartet and the existing adapter against `clat:plugin@0.1.0`. It does not publish
a package, add a host runtime, or alter the Bun/MCP fallback.

The strict attempt currently fails. DeepSeek search uses global `fetch`; HTTP
fetch uses Node DNS and an Undici address-pinned dispatcher. The adapter also
uses `AsyncLocalStorage`. The existing WIT host contract provides config,
sampling, elicitation and allowlisted native tools, but no HTTP/DNS transport.
Granting general WASI networking or substituting empty Node APIs does not meet
the experiment's criteria. The raw ComponentizeJS input also rejects the
`XID_Start` regular expression in the transitive DSH tools package during
preinitialization. Author-side lowering now removes that syntax obstacle;
the lowered original bundle then fails loading `node:async_hooks`. Both attempts
are retained independently from the missing transport.

The compiler uses Acorn tokens and regexpu to rewrite property **literals** in
generated output, preserving `u` and the original input files. Its Unicode 17
tables are pinned. Native Bun and lowered Node patterns agree for both XID
classes over all 1,114,112 code points, including lone surrogates. Runtime
metadata alone is insufficient: this Bun reports Unicode 15.1 while its regex
membership agrees with the pinned 17 tables. A 15.1 table trial disagreed at
8,949 start and 9,132 continuation code points and was rejected. The gates use
the actual Bun membership digest to detect future drift. This is proof for
the three pinned XID literals, not arbitrary engine-wide Unicode equivalence.

The **engine probe** is a separate artifact. It tests Promise resolution,
async generators, LIFO cleanup primitives, the original adapter EventBus's
waterfall, AbortController, private configuration and CLAT's fuel/epoch limits.
It also runs the three lowered original regex patterns in the actual guest on
empty, underscore, Chinese, astral, combining, surrogate and invalid inputs.
The full-code-point comparison is author-side; the guest checks representative
compositions. Removing lowering fails at guest preinitialization in a separate
test, before any Node imports obscure the discriminator.
It is not the full adapter's lifecycle oracle, the web quartet, or a marketable
plugin. Its emitted component world must contain no WASI, Node or Undici imports.

## Reproduce

Use Node 22 on the author machine; the native comparison also uses Bun from
the existing recipe setup. End users still need only CLAT for WASM components.

```sh
(cd sdk/dsh-adapter && npm ci && npm run build)
(cd sdk/dsh-adapter/examples/official-web && npm ci --ignore-scripts)
(cd sdk/dsh-wasm-spike && npm ci --ignore-scripts && npm test)

# Preserve build errors, emitted world, dependency graph and source hashes.
node sdk/dsh-wasm-spike/run.mjs --out /absolute/new-directory

# Optional three-sample measurements, with no concurrent builds or tests.
CLAT_PLG3_COMPONENT=/absolute/new-directory/engine-probe.wasm \
CLAT_PLG3_MEASUREMENTS=/absolute/measurements.json \
  cargo test -p clat-core --features test-support \
  plg3_component_engine_probe -- --ignored --nocapture
```

`--out` refuses an existing directory. Credentials are not accepted by the
build CLI. Wizer gets an explicitly empty environment, and the guest reads only
its private config at invocation time. Temporary test output is removed in
`finally`; originals are hashed before and after the strict attempt. Toolchain
versions and integrity hashes are locked. The componentizer override keeps jco
on the same current componentizer and avoids its older vulnerable weval chain;
AOT is disabled. `npm audit` is clean for this lock at experiment delivery.

The regular tests arm the Rust engine test and verify that exactly one test
ran. Running the ignored Rust test without `CLAT_PLG3_COMPONENT` does no work;
that result is not engine acceptance. Full gates run this author suite explicitly.

## Reopen conditions

A useful second flavor needs an independently specified, reviewed host egress
contract: public-address classification and DNS pinning, redirects, TLS,
bounded headers/bodies/deadlines, cancellation during DNS/body reads, and
permission/manifest ceilings. It also needs equivalent async cleanup ownership,
portable dependency bundling. The XID syntax lowering is available as an author
compiler pass; other runtime semantics still need validation. No sockets or ambient environment may leak
into the guest. The existing WIT version remains intact until an additive,
versioned contract is deliberately adopted.

After those conditions hold, use a separate id such as
`io.artec.dsh-official-web-wasm`, so both variants can be installed, updated and
removed independently. Their own tool prefixes keep contributions separate.
Then run the complete PLG-2 signed staging/PWA verdict and PLG-1 provider cases,
including loopback rejection and cancellation. Only that full result qualifies
the WASM quartet for the market's sandbox review tier.

Toolchain reference: [ComponentizeJS](https://github.com/bytecodealliance/ComponentizeJS)
documents async export resolution and feature disabling. The experiment tests
these claims in the actual emitted component and CLAT host rather than treating
the engine documentation as acceptance.
Regex lowering uses [regexpu-core](https://github.com/mathiasbynens/regexpu-core)
and pinned [Unicode property data](https://github.com/mathiasbynens/regenerate-unicode-properties).
