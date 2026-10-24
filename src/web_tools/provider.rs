use super::{config::Snapshot, *};
use std::time::Instant;
use url::Url;
pub(super) fn validate_url(text: &str) -> Result<Url, WebError> {
    if text.len() > 2048
        || text.chars().any(|c| c.is_control() || c.is_whitespace())
        || text.contains(['\\', '@', '#'])
    {
        return Err(WebError::new("invalid_url"));
    }
    let url = Url::parse(text).map_err(|_| WebError::new("invalid_url"))?;
    clat_wasm_net::dns_authority::Origin::parse(&url.origin().ascii_serialization())
        .map_err(|_| WebError::new("invalid_url"))?;
    if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost"))
    {
        return Err(WebError::new("blocked"));
    }
    Ok(url)
}
impl WebService {
    pub(super) fn search(
        &self,
        args: &Value,
        snapshot: &Snapshot,
        cancel: &CancelToken,
        quota: Option<&PaidCallBudget>,
    ) -> Result<Value, WebError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty() && s.chars().count() <= 128)
            .ok_or_else(|| WebError::new("invalid_query"))?;
        let count = match args.get("max_results") {
            None => 10,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=50).contains(n))
                .ok_or_else(|| WebError::new("invalid_query"))?,
        };
        let response = self.api(
            "search",
            json!({"query":query,"count":count,"need_content":false}),
            snapshot,
            cancel,
            quota,
        )?;
        let results = response
            .get("results")
            .and_then(Value::as_array)
            .filter(|v| v.len() <= 50)
            .ok_or_else(|| WebError::new("invalid_response"))?;
        let mut items = Vec::new();
        for item in results.iter().take(count as usize) {
            let url = item
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| WebError::new("invalid_response"))?;
            validate_url(url)?;
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .ok_or_else(|| WebError::new("invalid_response"))?;
            let mut reference = source(url, &title.chars().take(256).collect::<String>());
            reference.published_at = item.get("date").and_then(Value::as_str).map(str::to_owned);
            let snippet = item
                .get("summary")
                .and_then(Value::as_str)
                .or_else(|| item.get("snippet").and_then(Value::as_str))
                .unwrap_or("");
            let (body, truncated) = bounded(snippet);
            items.push(json!({"source":reference,"text":body,"truncated":truncated}));
        }
        Ok(json!({"provider":"deepseek","sources":items,"external_untrusted":true}))
    }
    pub(super) fn fetch(
        &self,
        args: &Value,
        snapshot: &Snapshot,
        cancel: &CancelToken,
        quota: Option<&PaidCallBudget>,
    ) -> Result<Value, WebError> {
        let text = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| WebError::new("invalid_url"))?;
        let url = validate_url(text)?;
        if snapshot.provider == FetchProvider::Deepseek {
            let response = self.api(
                "fetch",
                json!({"url":url.as_str()}),
                snapshot,
                cancel,
                quota,
            )?;
            let body = response
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| WebError::new("invalid_response"))?;
            let title = response
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(256)
                .collect::<String>();
            let (body, truncated) = bounded(body);
            return Ok(
                json!({"provider":"deepseek","source":source(url.as_str(),&title),"body":body,
                "status":null,"final_url":null,"api_status":200,"format":"markdown",
                "external_untrusted":true,"truncated":truncated}),
            );
        }
        self.fetch_http(url, cancel)
    }
    fn api(
        &self,
        operation: &str,
        body: Value,
        snapshot: &Snapshot,
        cancel: &CancelToken,
        quota: Option<&PaidCallBudget>,
    ) -> Result<Value, WebError> {
        check(cancel)?;
        let key = snapshot
            .key
            .as_ref()
            .ok_or_else(|| WebError::new("credential_missing"))?;
        if let Some(quota) = quota {
            quota.charge()?;
        }
        let response = self.transport().execute(
            &HttpRequest {
                url: format!("{API_BASE}/{operation}"),
                method: "POST",
                body: serde_json::to_vec(&body).unwrap(),
                timeout: TIMEOUT,
                headers: vec![
                    ("authorization".into(), format!("Bearer {key}")),
                    ("content-type".into(), "application/json".into()),
                ],
            },
            cancel,
        )?;
        check(cancel)?;
        if response.body.len() > MAX_BYTES {
            return Err(WebError::new("too_large"));
        }
        if response.status != 200 {
            return Err(WebError::new(match response.status {
                401 => "credential_invalid",
                402 => "balance",
                429 => "rate_limit",
                _ => "provider_failed",
            })
            .detail(format!("HTTP {}", response.status)));
        }
        serde_json::from_slice(&response.body).map_err(|_| WebError::new("invalid_response"))
    }
    fn fetch_http(&self, mut url: Url, cancel: &CancelToken) -> Result<Value, WebError> {
        let deadline = Instant::now() + TIMEOUT;
        for hop in 0..=5 {
            check(cancel)?;
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| WebError::new("timeout"))?;
            let r = self.transport().execute(
                &HttpRequest {
                    url: url.to_string(),
                    method: "GET",
                    body: Vec::new(),
                    timeout,
                    headers: vec![
                        ("user-agent".into(), "CLAT web_fetch".into()),
                        (
                            "accept".into(),
                            "text/html, text/plain, text/markdown".into(),
                        ),
                    ],
                },
                cancel,
            )?;
            check(cancel)?;
            if matches!(r.status, 301 | 302 | 303 | 307 | 308) {
                let location = r
                    .location
                    .as_ref()
                    .ok_or_else(|| WebError::new("invalid_response"))?;
                let target = url
                    .join(location)
                    .map_err(|_| WebError::new("invalid_url"))?;
                validate_url(target.as_str())?;
                if target.origin() != url.origin() {
                    return Err(WebError::new("redirect_blocked").detail(target.as_str()));
                }
                if hop == 5 {
                    return Err(WebError::new("redirect_limit"));
                }
                url = target;
                continue;
            }
            return render_http(&url, r, cancel);
        }
        Err(WebError::new("redirect_limit"))
    }
}
fn render_http(url: &Url, r: HttpResponse, cancel: &CancelToken) -> Result<Value, WebError> {
    if r.body.len() > MAX_BYTES {
        return Err(WebError::new("too_large"));
    }
    let content_type = r
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        content_type.as_str(),
        "text/html" | "text/plain" | "text/markdown" | "application/xhtml+xml"
    ) {
        return Err(WebError::new("unsupported_content"));
    }
    let raw = String::from_utf8(r.body).map_err(|_| WebError::new("unsupported_content"))?;
    let body = if matches!(content_type.as_str(), "text/html" | "application/xhtml+xml") {
        html2text::from_read(raw.as_bytes(), 120).map_err(|_| WebError::new("invalid_response"))?
    } else {
        raw
    };
    check(cancel)?;
    let (body, truncated) = bounded(&body);
    Ok(
        json!({"provider":"http","source":source(url.as_str(),""),"body":body,
        "status":r.status,"final_url":url.as_str(),"format":"text","external_untrusted":true,"truncated":truncated}),
    )
}
