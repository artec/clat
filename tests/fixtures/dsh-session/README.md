# DSH session-format interop fixtures

`regen.mjs` regenerates golden bytes using Node's own `node:zlib` zstd and
`JSON.stringify` — the exact primitives deepseek-harness uses at the pinned
revision (`docs/research/dsh-session-compatibility.md`).

```sh
node tests/fixtures/dsh-session/regen.mjs          # → /tmp/clat-interop/
cargo test --lib session::interop                  # consumes them, self-skips otherwise
```

The full DSH-checkout-based fixture set (pinned `99f6f02`, §2.3 of the plan)
supersedes these primitives-only bytes when the Node+DSH toolchain lands.
