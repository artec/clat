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

pub mod device;
pub mod envelope;
pub mod error;
pub mod pkce;
pub mod quota;
pub mod tencent;
pub mod transport;

pub use device::{
    DEFAULT_AUTH_BASE, DEFAULT_GATEWAY_JSON, DeviceLogin, PollStatus, Session, UserIdentity,
    derive_cli_access_token, poll_device_login, start_device_login,
};
pub use error::QuotaError;
pub use quota::{EquityItem, Platform, QuotaClient, USAGE_API, UsageWindow};
pub use tencent::{
    DEFAULT_REGION, PersonalPackage, TOKENHUB_HOST, TOKENHUB_VERSION, Tc3Credential, TokenHubClient,
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
            // usage response: outer → gateway DataV2 → harness → per fields
            r#"{"code":"200","successResponse":true,"data":{"DataV2":{"ret":["SUCCESS::"],"data":{"success":true,"data":{"per5HourPercentage":10,"per5HourResetTime":1795300000000,"per1WeekPercentage":42.5,"per1WeekResetTime":1795900000000}}}}}"#,
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
        assert_eq!(windows[0].remaining_percent, 90.0);
        assert_eq!(windows[1].key, "per1Week");

        // Wire assertion: JSON body, stringified Data with cornerstoneParam.
        let sent = Scripted::last_request(&transport);
        assert_eq!(sent.url, DEFAULT_GATEWAY_JSON);
        let body: Value = serde_json::from_str(&sent.body.expect("usage posts a body")).unwrap();
        assert_eq!(body["product"], envelope::GATEWAY_PRODUCT);
        let data: Value = serde_json::from_str(body["params"]["Data"].as_str().unwrap()).unwrap();
        assert_eq!(data["cornerstoneParam"]["consoleSite"], "QIANWENAI");
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
    fn usage_errors_surface_as_business_errors() {
        let transport = Scripted::new(vec![
            r#"{"code":"200","successResponse":true,"data":{"DataV2":{"ret":["Err::no quota"],"data":{}}}}"#,
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
