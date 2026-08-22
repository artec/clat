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

## B8 golden fixtures (2026-08-22, pinned 0.1.1-rc.2 = `b150a551b8`)

`interrupted-session.jsonl.zstd` and `team-events-session.jsonl.zstd` are
committed golden logs produced by the **real DSH write path** (Cordis
`Context` + `SessionStore` + `JsonlSessionPersistence`, compression zstd).
They are consumed by the always-on main test suite
(`src/session/dsh_golden.rs` — no Node dependency):

- interrupted: a mid-stream-cancelled turn whose `assistant/message`
  carries `interrupted: true` (mirrors DSH `agent.ts:352-368`);
- team: the four `team/*` known types riding in required envelopes.

Regenerate (dev side; also runs the DSH read leg over a CLAT-produced
interrupted log when `CLAT_CLAT_LOG` is set):

```sh
cd ../deepseek-harness && \
  ./node_modules/.bin/tsx ../clat/tests/fixtures/dsh-session/gen-dsh-fixtures.mts

# reverse leg: first write the CLAT artifact, then
CLAT_CLAT_LOG=/tmp/clat-interop/clat-interrupted.jsonl.zstd \
  ./node_modules/.bin/tsx ../clat/tests/fixtures/dsh-session/gen-dsh-fixtures.mts
```

Verification ledger: `docs/research/dsh-session-compatibility.md` §14.1.
