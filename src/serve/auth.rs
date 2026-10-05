//! Local protocol trust and private browser sessions. Never put the long-lived
//! credential in a URL, cookie, log, or browser storage. Cross-site protection
//! applies to *all* credentials; a remote authority requires explicit access.
use super::{http, state::ServeShared, web_assets};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

const LIFETIME: u64 = 30 * 24 * 60 * 60;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cookie_name(port: u16) -> String {
    format!("clat_session_{port}")
}

fn mac(secret: &str, authority: &str, payload: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(b"clat-browser-session-v1\0");
    mac.update(authority.as_bytes());
    mac.update(b"\0");
    mac.update(payload.as_bytes());
    mac
}

fn session(secret: &str, authority: &str, time: u64) -> String {
    let payload = format!("{}.{}", time + LIFETIME, uuid::Uuid::new_v4().simple());
    let signature = mac(secret, authority, &payload).finalize().into_bytes();
    let hex: String = signature.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{payload}.{hex}")
}

fn valid_session(secret: &str, authority: &str, value: &str, time: u64) -> bool {
    let Some((payload, signature)) = value.rsplit_once('.') else {
        return false;
    };
    let Some((expires, nonce)) = payload.split_once('.') else {
        return false;
    };
    let Ok(expires) = expires.parse::<u64>() else {
        return false;
    };
    if expires <= time
        || expires > time.saturating_add(LIFETIME)
        || nonce.len() != 32
        || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || signature.len() != 64
    {
        return false;
    }
    let bytes: Option<Vec<u8>> = (0..64)
        .step_by(2)
        .map(|offset| {
            signature
                .get(offset..offset + 2)
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        })
        .collect();
    bytes.is_some_and(|bytes| mac(secret, authority, payload).verify_slice(&bytes).is_ok())
}

fn browser_cookie(request: &http::HttpRequestHead, port: u16) -> Result<Option<&str>, ()> {
    let name = cookie_name(port);
    let mut found = None;
    for pair in request.cookie.as_deref().unwrap_or("").split(';') {
        let Some((key, value)) = pair.trim().split_once('=') else {
            continue;
        };
        if key == name {
            if found.is_some() {
                return Err(());
            }
            found = Some(value);
        }
    }
    Ok(found)
}

/// Origin equality and Fetch Metadata are CSRF fences, never credential
/// alternatives. Missing metadata is accepted only for origin-less local tools.
fn same_origin(request: &http::HttpRequestHead) -> bool {
    let origin_matches = request
        .origin
        .as_ref()
        .is_none_or(|origin| origin == &format!("http://{}", request.host));
    let fetch_matches = match request.fetch_site.as_deref() {
        Some("same-origin" | "same-site" | "none") => true,
        None => request.origin.is_none(),
        _ => false,
    };
    origin_matches && fetch_matches
}

fn reject(stream: &mut TcpStream, status: u16, disposition: &str) {
    let code = match status {
        400 => "bad-request",
        403 => "forbidden",
        _ => "unauthorized",
    };
    let body = serde_json::json!({"ok":false,"error":{
        "code":code,"message":"request authentication rejected","authentication":disposition
    }});
    let _ = http::write_response_with_headers(
        stream,
        status,
        "application/json",
        body.to_string().as_bytes(),
        &[("Cache-Control", "no-store")],
    );
}

fn authorized(
    stream: &mut TcpStream,
    request: &http::HttpRequestHead,
    shared: &ServeShared,
) -> bool {
    if !same_origin(request) {
        reject(stream, 403, "untrusted_request");
        return false;
    }
    let explicit = request.authorization.as_deref();
    if explicit.is_some() {
        if http::bearer_token(explicit).as_deref() == Some(shared.token.as_str()) {
            return true;
        }
        reject(stream, 401, "invalid_credentials");
        return false;
    }
    if request.host == format!("127.0.0.1:{}", shared.port)
        || request.host == format!("localhost:{}", shared.port)
    {
        return true;
    }
    match browser_cookie(request, shared.port) {
        Ok(Some(value)) if valid_session(&shared.token, &request.host, value, now()) => true,
        Ok(None) => {
            reject(stream, 403, "authentication_required");
            false
        }
        _ => {
            reject(stream, 401, "invalid_credentials");
            false
        }
    }
}

/// Apply before any request body, workspace routing, or protected response.
/// Returns true only when the API dispatcher should continue.
pub(super) fn admit(
    stream: &mut TcpStream,
    request: &http::HttpRequestHead,
    shared: &ServeShared,
) -> bool {
    if !authorized(stream, request, shared) {
        return false;
    }
    if request.method == "GET"
        && let Some((bytes, content_type)) = web_assets::asset(&request.path)
    {
        if request.host == format!("localhost:{}", shared.port) {
            let query = request
                .workspace_query
                .as_ref()
                .map(|id| format!("?workspace={id}"))
                .unwrap_or_default();
            let location = format!("http://127.0.0.1:{}{}{query}", shared.port, request.path);
            let _ = http::write_response_with_headers(
                stream,
                307,
                "text/plain",
                b"",
                &[
                    ("Location", &location),
                    ("Cache-Control", "no-store"),
                    ("Referrer-Policy", "no-referrer"),
                ],
            );
        } else {
            let _ = http::write_response_with_headers(
                stream,
                200,
                content_type,
                &bytes,
                &[
                    (
                        "Content-Security-Policy",
                        "default-src 'self'; img-src 'self' blob:; connect-src 'self' https://pi.at.cn; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'self'",
                    ),
                    ("Cache-Control", "no-store"),
                    ("Referrer-Policy", "no-referrer"),
                    ("X-Content-Type-Options", "nosniff"),
                ],
            );
        }
        return false;
    }
    if request.method == "POST" && request.path == "/auth" {
        if request.content_length != 0 {
            reject(stream, 400, "invalid_request");
        } else {
            let cookie = format!(
                "{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={LIFETIME}",
                cookie_name(shared.port),
                session(&shared.token, &request.host, now())
            );
            let _ = http::write_response_with_headers(
                stream,
                200,
                "application/json",
                br#"{"ok":true}"#,
                &[("Set-Cookie", &cookie), ("Cache-Control", "no-store")],
            );
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_sessions_bind_authority_expiry_and_key() {
        let value = session("secret", "127.0.0.1:2691", 100);
        assert!(valid_session("secret", "127.0.0.1:2691", &value, 101));
        assert!(!valid_session("secret", "127.0.0.1:2692", &value, 101));
        assert!(!valid_session("secret", "evil.example", &value, 101));
        assert!(!valid_session("rotated", "127.0.0.1:2691", &value, 101));
        assert!(!valid_session(
            "secret",
            "127.0.0.1:2691",
            &value,
            100 + LIFETIME
        ));
        assert!(!valid_session(
            "secret",
            "127.0.0.1:2691",
            &(value + "0"),
            101
        ));
    }
}
