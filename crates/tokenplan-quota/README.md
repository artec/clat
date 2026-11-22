# tokenplan-quota

Standalone quota client for the two Qwen Token Plan console entries —
**Qianwen AI Platform** (`qianwenai.com`) and **Aliyun Bailian** (`aliyun.com`).
No host-runtime dependencies; liftable verbatim into any Rust client.

- Device-flow login (`client_id` = per-machine UUID, PKCE mandatory) for the
  Qianwen entry; console access token (BYO, e.g. from a local-callback login)
  for the Bailian entry.
- Derived console credential (`GenerateCLIAccessToken`) for the Qianwen
  gateway.
- Two quota surfaces: per-window usage percentages
  (`…/tokenplan/personal/api/v2/usage`) and equity grants
  (`queryTokenPlanEquityInfo`).
- Tencent module (2026-11-22): TC3-HMAC-SHA256 signing plus the
  `tokenhub.tencentcloudapi.com` (2026-03-22) client for
  `DescribeTokenPlanPersonalPackage` — cloud-account SecretId/SecretKey
  credentials, never the model key.
- Typed errors per protocol layer; stateless — storage belongs to the caller.

## Why the model API key cannot do this

The model gateways (`token-plan.maas.qianwenaiapi.com`,
`token-plan.ap-southeast-1.maas.aliyuncs.com`) expose no usage route (404 /
catch-all) and no rate-limit headers; the console gateway answers
`ConsoleNeedLogin` to a model key. Quota lives behind a **console
credential** obtained through the platforms' own first-party login flows —
this crate implements those flows directly and never touches browser cookies.

## Usage

See the crate root docs (`src/lib.rs`) for a complete runnable example:
start device login → poll → derive → query. Minimal sketch:

```rust
let client = tokenplan_quota::QuotaClient::new(
    &transport,
    tokenplan_quota::Platform::Qianwen { gateway_json: String::new() },
    cli_access_token,
);
for window in client.query_usage()? {
    // window.key ("per5Hour"), window.remaining_percent, window.reset_at_ms
}
```

## Wire protocol (reference sources)

Verified 2026-11-22 against qianwen-cli 1.12 (`src/api/auth-client.ts`,
`src/api/adapters/gateway-adapter.ts`, `src/api/request-adapter.ts`) and
bailian-cli 2.10.4 (`packages/core/src/console/gateway.ts`,
`packages/commands/src/commands/usage/token-plan.ts`), plus live probes:

- `POST https://t.qianwenai.com/cli/device/code` — arbitrary UUID
  `client_id`, PKCE S256 required, returns `Token`/`VerificationUrl`/
  `ExpiresIn`/`Interval`.
- `POST https://cli.qianwenai.com/data/v2/api.json` — JSON envelope
  (`params.Data` stringified), error shape `{"code":"ConsoleNeedLogin",…}`.
- `POST https://bailian-cs.console.aliyun.com/cli/api.json` — form envelope,
  region `cn-beijing`, `consoleSite: "BAILIAN_ALIYUN"`.

## Storage discipline

Access/refresh tokens and the derived `cliAccessToken` are **account-scoped
console credentials**: store encrypted, never log, expect periodic refresh
(`ConsoleNeedLogin` → re-derive, or full re-login). This crate holds no
state.

## License

Same license as the host repository.
