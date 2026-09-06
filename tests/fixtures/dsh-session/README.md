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

`interrupted-session.jsonl.zstd`, `team-events-session.jsonl.zstd`, and
`plan-mode-approved-session.jsonl.zstd` are committed golden logs produced by
the **real DSH write path** (Cordis `Context` + `SessionStore` +
`JsonlSessionPersistence`, compression zstd). They are consumed by the
always-on main test suite (`src/session/dsh_golden.rs` — no Node dependency):

- interrupted: a mid-stream-cancelled turn whose `assistant/message`
  carries `interrupted: true` (mirrors DSH `agent.ts:352-368`);
- team: the four `team/*` known types riding in required envelopes;
- plan-mode-approved: `plan/mode` active birth followed by CLAT's bounded
  `approved {text,digest}` extension. The generator reads this session back
  through pinned DSH `JsonlSessionPersistence.load` before copying the golden;
  CLAT then consumes the committed bytes in `src/session/dsh_golden.rs`.

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

## DV-5 golden (2026-09-06, provenance shift)

`model-selection-session.jsonl.zstd` carries the three DSH 0.1.2-alpha.4+
v0-required additions (`model/selection`,
`session-log-deepseek/delivery-accepted`,
`subagent/model-selection-policy`, intro commit `822d735356`). The 0.1.3
pinned checkout (`d347e70390`) can no longer produce v0 bytes — its writer
is hard-wired to `SESSION_FORMAT_VERSION = 2` with no version knob — so this
golden is minted by `gen-dv5-fixture.mjs` with the regen.mjs primitives
(frame layout + envelope field order identical to the B8 goldens), with
payload shapes taken from the three DSH 0.1.3 write points. Consumed by the
always-on suite in `src/session/dsh_golden.rs`
(`dsh_012_model_selection_events_admit_and_replay_skips`).
