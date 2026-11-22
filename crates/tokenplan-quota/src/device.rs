//! Device-flow login for the Qianwen AI Platform entry plus the derived
//! console credential. Wire shapes verified against qianwen-cli 1.12
//! (`src/api/auth-client.ts`) and live-probed on 2026-11-22:
//!
//! - `POST {auth_base}/cli/device/code?client_id=<uuid>&code_challenge=<s256>&code_challenge_method=S256`
//!   → `{"Success":true,"Data":{"Token","ExpiresIn","VerificationUrl","Interval"}}`
//!   (PKCE is **mandatory** — the endpoint returns 400 without the challenge;
//!   `client_id` is an unregistered per-machine UUID, no vendor secret).
//! - `POST {auth_base}/cli/device/token?client_id=<uuid>&token=<Token>[&code_verifier=…]`
//!   → `{"Data":{"Status","Credentials":{…}}}` (PascalCase with a snake_case
//!   fallback), terminal failures surfaced as HTTP errors whose body names
//!   `expired` / `access_denied` / `slow_down`.
//! - `POST {gateway}/data/v2/api.json` `GenerateCLIAccessToken` with the
//!   access token → `{"code":"200","data":{"cliAccessToken":…}}`.

use serde_json::Value;

use crate::envelope::{self, qianwen_flat_body};
use crate::error::QuotaError;
use crate::pkce::generate_pkce_pair;
use crate::transport::{HttpRequest, HttpTransport, Method};

pub const DEFAULT_AUTH_BASE: &str = "https://t.qianwenai.com";
pub const DEFAULT_GATEWAY_JSON: &str = "https://cli.qianwenai.com/data/v2/api.json";
const PRODUCT_MODEL_STUDIO_REGIONAL: &str = "ModelStudioRegional";
const ACTION_GENERATE_CLI_ACCESS_TOKEN: &str = "GenerateCLIAccessToken";
const VERSION_GENERATE_CLI_ACCESS_TOKEN: &str = "2026-02-10";
const USER_AGENT: &str = concat!("tokenplan-quota/", env!("CARGO_PKG_VERSION"));

/// One pending device login. Persist nothing but `code_verifier` + `token`
/// between polls (the integrator owns storage; this crate stays stateless).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLogin {
    pub token: String,
    pub verification_url: String,
    pub expires_in_secs: u64,
    pub interval_secs: u64,
    pub code_verifier: String,
    pub client_id: String,
}

/// Poll outcome. `Complete` carries the console session credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollStatus {
    Complete(Session),
    AuthorizationPending,
    SlowDown,
    ExpiredToken,
    AccessDenied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub user: Option<UserIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    pub email: String,
    pub aliyun_id: String,
}

/// Start a device login: generates the PKCE pair internally.
pub fn start_device_login(
    transport: &dyn HttpTransport,
    client_id: &str,
    auth_base: &str,
) -> Result<DeviceLogin, QuotaError> {
    let (code_verifier, code_challenge) = generate_pkce_pair();
    let url = format!(
        "{auth_base}/cli/device/code?client_id={client_id}&code_challenge={code_challenge}&code_challenge_method=S256"
    );
    let response = transport.send(HttpRequest {
        method: Method::Post,
        url,
        headers: vec![("User-Agent".into(), USER_AGENT.into())],
        body: None,
    })?;
    let raw: Value = serde_json::from_str(&response.body)
        .map_err(|error| QuotaError::shape(format!("device code response: {error}")))?;
    if raw.get("Success").and_then(Value::as_bool) != Some(true) {
        return Err(QuotaError::shape(
            "device code init did not confirm success",
        ));
    }
    let data = raw
        .get("Data")
        .ok_or_else(|| QuotaError::shape("device code init has no Data"))?;
    let token = text(data, "Token").ok_or_else(|| QuotaError::shape("device code has no Token"))?;
    Ok(DeviceLogin {
        token,
        verification_url: text(data, "VerificationUrl")
            .ok_or_else(|| QuotaError::shape("device code has no VerificationUrl"))?,
        expires_in_secs: data.get("ExpiresIn").and_then(Value::as_u64).unwrap_or(300),
        interval_secs: data.get("Interval").and_then(Value::as_u64).unwrap_or(2),
        code_verifier,
        client_id: client_id.to_string(),
    })
}

/// Poll once. The caller owns the interval loop (never poll faster than
/// [`DeviceLogin::interval_secs`]).
pub fn poll_device_login(
    transport: &dyn HttpTransport,
    login: &DeviceLogin,
    auth_base: &str,
) -> Result<PollStatus, QuotaError> {
    let mut url = format!(
        "{auth_base}/cli/device/token?client_id={}&token={}",
        login.client_id,
        urlencode(&login.token)
    );
    if !login.code_verifier.is_empty() {
        url.push_str("&code_verifier=");
        url.push_str(&urlencode(&login.code_verifier));
    }
    let response = transport.send(HttpRequest {
        method: Method::Post,
        url,
        headers: vec![("User-Agent".into(), USER_AGENT.into())],
        body: None,
    })?;
    if response.status >= 400 {
        let lowered = response.body.to_lowercase();
        return Ok(if lowered.contains("expired") {
            PollStatus::ExpiredToken
        } else if lowered.contains("access_denied") {
            PollStatus::AccessDenied
        } else if lowered.contains("slow_down") {
            PollStatus::SlowDown
        } else {
            return Err(QuotaError::Http(response.status, response.body));
        });
    }
    let raw: Value = serde_json::from_str(&response.body)
        .map_err(|error| QuotaError::shape(format!("device token response: {error}")))?;
    let status = raw
        .get("Data")
        .and_then(|data| data.get("Status"))
        .and_then(Value::as_str)
        .or_else(|| raw.get("status").and_then(Value::as_str))
        .unwrap_or("authorization_pending")
        .to_lowercase();
    match status.as_str() {
        "complete" => {
            let credentials = raw
                .get("Data")
                .and_then(|data| data.get("Credentials"))
                .or_else(|| raw.get("credentials"));
            let access_token = credentials
                .and_then(|credentials| {
                    credentials
                        .get("AccessToken")
                        .or_else(|| credentials.get("access_token"))
                })
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if access_token.is_empty() {
                return Err(QuotaError::shape("complete poll carried no access token"));
            }
            let refresh_token = credentials
                .and_then(|credentials| {
                    credentials
                        .get("RefreshToken")
                        .or_else(|| credentials.get("refresh_token"))
                })
                .and_then(Value::as_str)
                .map(str::to_owned);
            let expires_at = credentials
                .and_then(|credentials| {
                    credentials
                        .get("ExpireTime")
                        .or_else(|| credentials.get("expire_time"))
                })
                .and_then(Value::as_str)
                .map(str::to_owned);
            let user = credentials
                .and_then(|credentials| credentials.get("User"))
                .map(|user| UserIdentity {
                    email: user
                        .get("Email")
                        .or_else(|| user.get("email"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    aliyun_id: user
                        .get("AliyunId")
                        .or_else(|| user.get("aliyunId"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            Ok(PollStatus::Complete(Session {
                access_token,
                refresh_token,
                expires_at,
                user,
            }))
        }
        "slow_down" => Ok(PollStatus::SlowDown),
        "expired_token" => Ok(PollStatus::ExpiredToken),
        "access_denied" => Ok(PollStatus::AccessDenied),
        _ => Ok(PollStatus::AuthorizationPending),
    }
}

/// Exchange the session access token for the derived console credential
/// (`cliAccessToken`) that authorizes cs-data gateway reads on the Qianwen
/// entry. Long-lived processes should re-derive when the gateway answers
/// `ConsoleNeedLogin`.
pub fn derive_cli_access_token(
    transport: &dyn HttpTransport,
    session: &Session,
    gateway_json: &str,
) -> Result<String, QuotaError> {
    let body = qianwen_flat_body(
        PRODUCT_MODEL_STUDIO_REGIONAL,
        ACTION_GENERATE_CLI_ACCESS_TOKEN,
        VERSION_GENERATE_CLI_ACCESS_TOKEN,
    );
    let response = transport.send(HttpRequest {
        method: Method::Post,
        url: gateway_json.to_string(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            (
                "Authorization".into(),
                format!("Bearer {}", session.access_token),
            ),
        ],
        body: Some(body),
    })?;
    let raw: Value = serde_json::from_str(&response.body)
        .map_err(|error| QuotaError::shape(format!("derive response: {error}")))?;
    let data = envelope::unwrap_outer(&raw)?;
    let token = data
        .get("cliAccessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty() && !token.contains(char::is_whitespace))
        .ok_or_else(|| QuotaError::shape("token issuance returned no usable cliAccessToken"))?;
    Ok(token.to_string())
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;
    use std::sync::Mutex;

    struct Scripted(Vec<HttpResponse>);

    impl HttpTransport for Scripted {
        fn send(&self, _request: HttpRequest) -> Result<HttpResponse, QuotaError> {
            self.0
                .first()
                .cloned()
                .ok_or_else(|| QuotaError::Transport("script empty".into()))
        }
    }

    fn scripted(responses: Vec<HttpResponse>) -> Mutex<Scripted> {
        Mutex::new(Scripted(responses))
    }

    const INIT_BODY: &str = r#"{"Success":true,"Data":{"Token":"dev-token","ExpiresIn":300,"VerificationUrl":"https://account.qianwenai.com/sso","Interval":2}}"#;

    #[test]
    fn start_parses_live_shape_and_carries_pkce() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: INIT_BODY.into(),
        }]);
        let login =
            start_device_login(&*transport.lock().unwrap(), "uuid-1", DEFAULT_AUTH_BASE).unwrap();
        assert_eq!(login.token, "dev-token");
        assert_eq!(login.interval_secs, 2);
        assert_eq!(login.code_verifier.len(), 43);
        assert_eq!(login.client_id, "uuid-1");
    }

    #[test]
    fn start_rejects_unsuccessful_init() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: r#"{"Success":false}"#.into(),
        }]);
        let error = start_device_login(&*transport.lock().unwrap(), "uuid-1", DEFAULT_AUTH_BASE)
            .unwrap_err();
        assert!(matches!(error, QuotaError::Shape(_)));
    }

    #[test]
    fn poll_maps_pascal_complete_with_user() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: r#"{"Success":true,"Data":{"Status":"complete","Credentials":{"AccessToken":"at-1","RefreshToken":"rt-1","ExpireTime":"2026-12-01T00:00:00Z","User":{"Email":"a@b.c","AliyunId":"42"}}}}"#.into(),
        }]);
        let login = DeviceLogin {
            token: "dev-token".into(),
            verification_url: String::new(),
            expires_in_secs: 300,
            interval_secs: 2,
            code_verifier: "v".into(),
            client_id: "uuid-1".into(),
        };
        match poll_device_login(&*transport.lock().unwrap(), &login, DEFAULT_AUTH_BASE).unwrap() {
            PollStatus::Complete(session) => {
                assert_eq!(session.access_token, "at-1");
                assert_eq!(session.refresh_token.as_deref(), Some("rt-1"));
                assert_eq!(session.user.as_ref().unwrap().aliyun_id, "42");
            }
            other => panic!("expected complete, got {other:?}"),
        }
    }

    #[test]
    fn poll_snake_fallback_and_pending() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: r#"{"status":"authorization_pending"}"#.into(),
        }]);
        let login = DeviceLogin {
            token: "dev-token".into(),
            verification_url: String::new(),
            expires_in_secs: 300,
            interval_secs: 2,
            code_verifier: "v".into(),
            client_id: "uuid-1".into(),
        };
        let status =
            poll_device_login(&*transport.lock().unwrap(), &login, DEFAULT_AUTH_BASE).unwrap();
        assert_eq!(status, PollStatus::AuthorizationPending);
    }

    #[test]
    fn poll_http_error_bodies_map_to_terminal_states() {
        for (body, expected) in [
            (r#"{"error":"expired"}"#, PollStatus::ExpiredToken),
            (r#"{"error":"access_denied"}"#, PollStatus::AccessDenied),
            (r#"{"error":"slow_down"}"#, PollStatus::SlowDown),
        ] {
            let transport = scripted(vec![HttpResponse {
                status: 400,
                body: body.into(),
            }]);
            let login = DeviceLogin {
                token: "dev-token".into(),
                verification_url: String::new(),
                expires_in_secs: 300,
                interval_secs: 2,
                code_verifier: "v".into(),
                client_id: "uuid-1".into(),
            };
            assert_eq!(
                poll_device_login(&*transport.lock().unwrap(), &login, DEFAULT_AUTH_BASE).unwrap(),
                expected,
                "{body}"
            );
        }
    }

    #[test]
    fn derive_reads_cli_access_token() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: r#"{"code":"200","successResponse":true,"data":{"cliAccessToken":"cli-1"}}"#
                .into(),
        }]);
        let session = Session {
            access_token: "at-1".into(),
            refresh_token: None,
            expires_at: None,
            user: None,
        };
        let token =
            derive_cli_access_token(&*transport.lock().unwrap(), &session, DEFAULT_GATEWAY_JSON)
                .unwrap();
        assert_eq!(token, "cli-1");
    }

    #[test]
    fn derive_surfaces_need_login_envelope() {
        let transport = scripted(vec![HttpResponse {
            status: 200,
            body: r#"{"code":"ConsoleNeedLogin","message":"You need to log in.","successResponse":false}"#.into(),
        }]);
        let session = Session {
            access_token: "stale".into(),
            refresh_token: None,
            expires_at: None,
            user: None,
        };
        let error =
            derive_cli_access_token(&*transport.lock().unwrap(), &session, DEFAULT_GATEWAY_JSON)
                .unwrap_err();
        assert_eq!(
            error,
            QuotaError::envelope("ConsoleNeedLogin", "You need to log in.")
        );
    }
}
