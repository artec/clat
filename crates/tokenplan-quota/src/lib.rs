//! # tokenplan-quota
//!
//! Standalone quota client for the two Qwen Token Plan console entries —
//! **Qianwen AI Platform** (`qianwenai.com`) and **Aliyun Bailian**
//! (`aliyun.com`) — with zero dependencies on any host runtime. Extractable
//! as-is for other open-source clients.
//!
//! ## Why a console credential (and never the model API key)
//!
//! Neither entry exposes a quota API to the `sk-sp-`-style model key: the
//! model gateways 404 every usage route and carry no rate-limit headers, and
//! the console gateway answers `ConsoleNeedLogin`. The quota surface is the
//! **console gateway**, authorized by a **console credential** obtained
//! through the platform's own first-party login flows — never by harvesting
//! browser cookies.
//!
//! ## Credential chains (both verified against the vendor CLIs + live probes)
//!
//! **Qianwen entry** (qianwen-cli 1.12 `src/api/auth-client.ts`):
//!   1. `POST {auth_base}/cli/device/code?client_id=<uuid>&code_challenge=<s256>&code_challenge_method=S256`
//!      — `client_id` is an unregistered per-machine UUID; PKCE is mandatory
//!      (live probe: 400 without the challenge).
//!   2. User opens `verification_url` in a browser and approves.
//!   3. `POST {auth_base}/cli/device/token?client_id=…&token=…[&code_verifier=…]`
//!      until `Status:"complete"` → session access token.
//!   4. `GenerateCLIAccessToken` (envelope A) → derived `cliAccessToken`.
//!   5. Gateway reads with `Bearer <cliAccessToken>` on
//!      `https://cli.qianwenai.com/data/v2/api.json`.
//!
//! **Bailian entry** (bailian-cli 2.10.4 `packages/core/src/console/gateway.ts`):
//! console login hands a local callback an access token (integrator-owned
//! step); gateway reads use `Bearer <access token>` on
//! `https://bailian-cs.console.aliyun.com/cli/api.json`.
//!
//! ## Quota surfaces
//!
//! - Usage windows: `…/tokenplan/personal/api/v2/usage` →
//!   `per{N}{Hour|Day|Week|Month}{Percentage|ResetTime}` fields (used %; the
//!   smallest window matches the console's "5-hour quota" view).
//! - Equity grants: `queryTokenPlanEquityInfo` → absolute totals/remaining
//!   with instance windows.
//!
//! ## Example (Qianwen entry)
//!
//! ```no_run
//! use tokenplan_quota::{Platform, QuotaClient, UreqTransport, device};
//!
//! # fn main() -> Result<(), tokenplan_quota::QuotaError> {
//! let transport = UreqTransport::default();
//! let client_id = "b07b4b76-8a2e-4f31-9f1c-2a3d4e5f6a7b"; // persist per machine
//! let login = device::start_device_login(&transport, client_id, device::DEFAULT_AUTH_BASE)?;
//! println!("Open {} and approve within {}s", login.verification_url, login.expires_in_secs);
//! let session = loop {
//!     std::thread::sleep(std::time::Duration::from_secs(login.interval_secs.max(1)));
//!     match device::poll_device_login(&transport, &login, device::DEFAULT_AUTH_BASE)? {
//!         device::PollStatus::Complete(session) => break session,
//!         device::PollStatus::ExpiredToken | device::PollStatus::AccessDenied => {
//!             return Err(tokenplan_quota::QuotaError::Shape("login ended".into()));
//!         }
//!         _ => continue,
//!     }
//! };
//! let cli_token = device::derive_cli_access_token(&transport, &session, device::DEFAULT_GATEWAY_JSON)?;
//! let client = QuotaClient::new(&transport, Platform::Qianwen { gateway_json: String::new() }, cli_token);
//! for window in client.query_usage()? {
//!     println!("{}: {:.1}% left ({}h window)", window.key, window.remaining_percent, window.window_hours);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! **Storage discipline:** access/refresh tokens and the derived
//! `cliAccessToken` are *account-scoped console credentials* — store them
//! encrypted, never log them, and treat refresh (`ConsoleNeedLogin` →
//! re-derive, or full re-login) as routine. This crate holds no state;
//! persistence belongs to the integrator.

pub mod bailian;
pub mod device;
pub mod envelope;
pub mod error;
pub mod pkce;
pub mod quota;
pub mod tencent;
pub mod transport;

pub use bailian::{
    CONSOLE_ORIGIN_DOMESTIC, ConsoleCallback, ConsoleCallbackCancel, ConsoleCallbackWait,
    console_login_url, new_state,
};
pub use device::{
    DEFAULT_AUTH_BASE, DEFAULT_GATEWAY_JSON, DeviceLogin, PollStatus, Session, UserIdentity,
    derive_cli_access_token, poll_device_login, start_device_login,
};
pub use error::QuotaError;
pub use quota::{EquityItem, Platform, QuotaClient, USAGE_API, UsageWindow};
pub use tencent::{
    DEFAULT_REGION, PersonalPackage, TOKENHUB_HOST, TOKENHUB_VERSION, Tc3Credential,
    TokenHubClient, beijing_time_to_epoch_ms,
};
pub use transport::{HttpRequest, HttpResponse, HttpTransport, Method, UreqTransport};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    /// Records requests and replays scripted bodies (popped in order); lets
    /// the flow tests assert the exact wire shapes end to end.
    struct Scripted {
        responses: Mutex<Vec<&'static str>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl Scripted {
        fn new(responses: Vec<&'static str>) -> Mutex<Self> {
            Mutex::new(Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            })
        }
        fn last_request(mutex: &Mutex<Self>) -> HttpRequest {
            mutex
                .lock()
                .unwrap()
                .requests
                .lock()
                .unwrap()
                .last()
                .expect("a request was made")
                .clone()
        }
    }

    impl HttpTransport for Scripted {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, QuotaError> {
            self.requests.lock().unwrap().push(request);
            let body = self.responses.lock().unwrap().remove(0);
            Ok(HttpResponse {
                status: 200,
                body: body.to_string(),
            })
        }
    }

    fn percent_decode(value: &str) -> Option<String> {
        let mut out = Vec::new();
        let bytes = value.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'%' if index + 2 < bytes.len() + 1 && index + 2 < bytes.len() + 1 => {
                    let hex = bytes.get(index + 1..index + 3)?;
                    let byte = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                    out.push(byte);
                    index += 3;
                }
                b'+' => {
                    out.push(b' ');
                    index += 1;
                }
                byte => {
                    out.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8(out).ok()
    }

    fn device_login() -> DeviceLogin {
        DeviceLogin {
            token: "dev-token".into(),
            verification_url: "https://account.qianwenai.com/sso".into(),
            expires_in_secs: 300,
            interval_secs: 2,
            code_verifier: "verifier".into(),
            client_id: "uuid-1".into(),
        }
    }

    #[test]
    fn qianwen_flow_end_to_end_reports_usage_windows() {
        let transport = Scripted::new(vec![
            // derive response
            r#"{"code":"200","successResponse":true,"data":{"cliAccessToken":"cli-1"}}"#,
            // usage response: cs-data form gateway layers with 0..1 ratios
            // (live capture shape, 2026-11-22).
            r#"{"code":"200","data":{"DataV2":{"ret":["SUCCESS::ok"],"data":{"code":"SUCCESS","success":true,"data":{"per5HourPercentage":0.10,"per5HourResetTime":1795300000000,"per1WeekPercentage":0.425,"per1WeekResetTime":1795900000000}}},"success":true,"errorCode":""},"successResponse":true}"#,
        ]);
        let guard = transport.lock().unwrap();
        let locked = &*guard;
        let session = Session {
            access_token: "at-1".into(),
            refresh_token: None,
            expires_at: None,
            user: None,
        };
        let cli_token = derive_cli_access_token(locked, &session, DEFAULT_GATEWAY_JSON).unwrap();
        let client = QuotaClient::new(
            locked,
            Platform::Qianwen {
                gateway_json: String::new(),
            },
            cli_token,
        );
        let windows = client.query_usage().unwrap();
        drop(guard);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].key, "per5Hour");
        assert!((windows[0].remaining_percent - 90.0).abs() < 1e-9);
        assert_eq!(windows[1].key, "per1Week");

        // Wire assertion: cs-data form gateway (host + form + cornerstone).
        let sent = Scripted::last_request(&transport);
        assert!(
            sent.url
                .starts_with("https://cs-data.qianwenai.com/cli/api.json?"),
            "{}",
            sent.url
        );
        let form = sent.body.expect("cs-data usage posts a form");
        let decoded = form
            .strip_prefix("params=")
            .and_then(|encoded| encoded.split("&region=").next())
            .and_then(percent_decode)
            .expect("params is the first field");
        let params: Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(params["Api"], quota::USAGE_API);
        assert_eq!(
            params["Data"]["cornerstoneParam"]["consoleSite"],
            "QIANWENAI"
        );
        assert_eq!(params["Data"]["cornerstoneParam"]["switchUserType"], 3);
        let auth = sent
            .headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert_eq!(auth, "Bearer cli-1");
        let _ = device_login(); // shape guard
    }

    #[test]
    fn qianwen_equity_sends_request_id_and_parses_items() {
        let transport = Scripted::new(vec![
            r#"{"code":"200","successResponse":true,"data":{"DataV2":{"ret":["SUCCESS::"],"data":{"success":true,"code":"SUCCESS","data":{"tokenPlanEquityInfos":[{"equityType":"web_search","totalQuota":1000,"availableQuota":"250.5","instanceStartTime":"1795100000000","instanceEndTime":"1797700000000"}]}}}}}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(
            &*guard,
            Platform::Qianwen {
                gateway_json: String::new(),
            },
            "cli-1".into(),
        );
        let items = client.query_equity().unwrap();
        drop(guard);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].remaining_quota, 250.5);

        let sent = Scripted::last_request(&transport);
        let body: Value = serde_json::from_str(&sent.body.unwrap()).unwrap();
        let data: Value = serde_json::from_str(body["params"]["Data"].as_str().unwrap()).unwrap();
        let request = &data["queryTokenPlanEquityInfoRequest"];
        assert!(
            request["requestId"]
                .as_str()
                .is_some_and(|id| id.len() == 36)
        );
        assert_eq!(body["params"]["Api"], quota::EQUITY_API);
    }

    /// 2026-11-22 live capture（负责人授权凭据实弹）：cs-data form 信封 +
    /// 比率标度（per1MonthPercentage 0..1 → ×100）。端点接错（JSON 网关）
    /// 或忘了 ×100 都会在此红——正是"授权完成却刷不出额度"事故的两个
    /// 真实根因。
    #[test]
    fn qianwen_usage_reads_cs_data_ratios() {
        let transport = Scripted::new(vec![
            r#"{"code":"200","data":{"DataV2":{"ret":["SUCCESS::ok"],"data":{"msg":"Success.","code":"SUCCESS","data":{"per1MonthPercentage":0.14863470413333332,"per1MonthResetTime":1797782400000},"success":true}},"success":true,"errorCode":""},"httpStatusCode":"200","successResponse":true}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(
            &*guard,
            Platform::Qianwen {
                gateway_json: String::new(),
            },
            "derived-token".into(),
        );
        let windows = client.query_usage().unwrap();
        drop(guard);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].key, "per1Month");
        assert_eq!(windows[0].window_hours, 720);
        assert!((windows[0].used_percent - 14.863_470_413).abs() < 1e-6);
        assert!((windows[0].remaining_percent - 85.136_529_586).abs() < 1e-4);
        assert_eq!(windows[0].reset_at_ms, Some(1_797_782_400_000));

        let sent = Scripted::last_request(&transport);
        assert!(
            sent.url
                .starts_with("https://cs-data.qianwenai.com/cli/api.json?"),
            "{}",
            sent.url
        );
        let form = sent.body.expect("cs-data posts a form");
        assert!(form.contains("switchUserType%22%3A3"), "{form}");
        assert!(form.contains("QIANWENAI"));
        let auth = sent
            .headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert_eq!(auth, "Bearer derived-token");
    }

    #[test]
    fn bailian_usage_posts_form_envelope_and_parses_windows() {
        let transport = Scripted::new(vec![
            r#"{"DataV2":{"ret":["SUCCESS::"],"data":{"data":{"per1MonthPercentage":7,"per1MonthResetTime":1798400000000}}}}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(&*guard, Platform::Bailian, "console-access-token".into());
        let windows = client.query_usage().unwrap();
        drop(guard);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].key, "per1Month");
        assert_eq!(windows[0].window_hours, 720);

        let sent = Scripted::last_request(&transport);
        assert!(
            sent.url
                .starts_with("https://bailian-cs.console.aliyun.com/cli/api.json?")
        );
        let form = sent.body.expect("bailian posts a form");
        assert!(form.starts_with("params=%7B%22Api%22"));
        assert!(form.contains("region=cn-beijing"));
        let auth = sent
            .headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert_eq!(auth, "Bearer console-access-token");
    }

    #[test]
    fn bailian_usage_reads_unit_ratios_like_the_qianwen_entry() {
        // Live-captured 2026-11-23: the Bailian gateway reported
        // `per1MonthPercentage: 0.1799` for the same account the Qianwen
        // cs-data entry read as 17.99% used — the ratio scale is 0..1 here
        // too; reading it as a percentage showed a phantom 99.8% remaining.
        let transport = Scripted::new(vec![
            r#"{"code":"200","data":{"DataV2":{"ret":["SUCCESS::ok"],"data":{"success":true,"errorCode":"","data":{"per1MonthPercentage":0.1799,"per1MonthResetTime":1797782400000}}},"success":true},"httpStatusCode":"200","successResponse":true}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(&*guard, Platform::Bailian, "console-access-token".into());
        let windows = client.query_usage().unwrap();
        drop(guard);
        assert_eq!(windows.len(), 1);
        assert!((windows[0].used_percent - 17.99).abs() < 1e-9);
        assert!((windows[0].remaining_percent - 82.01).abs() < 1e-9);
        assert_eq!(windows[0].reset_at_ms, Some(1_797_782_400_000));
    }

    #[test]
    fn bailian_site_pin_error_surfaces_instead_of_empty_success() {
        // Live-captured 2026-11-23, right after a successful Qianwen cs-data
        // query pinned the shared account to the QIANWENAI console site:
        // HTTP 200, `success:false`, `errorCode:BailianGateway.Login.NotLogined`.
        // The unwrap must surface this, not collapse into an empty success.
        let transport = Scripted::new(vec![
            r#"{"code":"200","data":{"success":false,"httpStatus":200,"errorCode":"BailianGateway.Login.NotLogined","api":"zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage","errorMsg":"BailianGateway.Login.NotLogined"},"httpStatusCode":"200","requestId":"90beb3fa-88a3-4a23-9d29-e1b7bfdf6001","successResponse":true}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(&*guard, Platform::Bailian, "console-access-token".into());
        let error = client.query_usage().unwrap_err();
        drop(guard);
        assert_eq!(
            error,
            QuotaError::envelope(
                "BailianGateway.Login.NotLogined",
                "BailianGateway.Login.NotLogined",
            )
        );
    }

    #[test]
    fn usage_errors_surface_as_business_errors() {
        let transport = Scripted::new(vec![
            r#"{"code":"200","data":{"DataV2":{"ret":["Err::no quota"],"data":{}},"success":true,"errorCode":""},"successResponse":true}"#,
        ]);
        let guard = transport.lock().unwrap();
        let client = QuotaClient::new(
            &*guard,
            Platform::Qianwen {
                gateway_json: String::new(),
            },
            "cli-1".into(),
        );
        let error = client.query_usage().unwrap_err();
        assert_eq!(error, QuotaError::business("Err", "no quota"));
    }

    #[test]
    fn usage_http_errors_keep_the_status() {
        struct Failing;
        impl HttpTransport for Failing {
            fn send(&self, _request: HttpRequest) -> Result<HttpResponse, QuotaError> {
                Ok(HttpResponse {
                    status: 502,
                    body: "bad gateway".into(),
                })
            }
        }
        let client = QuotaClient::new(&Failing, Platform::Bailian, "token".into());
        assert_eq!(
            client.query_usage().unwrap_err(),
            QuotaError::Http(502, "bad gateway".into())
        );
    }
}
