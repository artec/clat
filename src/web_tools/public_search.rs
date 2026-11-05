//! Anonymous DuckDuckGo Lite search through the same public-web transport as fetch.
use super::{provider::validate_url, *};
use html2text::{Element, Handle, config};
use std::collections::HashSet;
use url::Url;

const SEARCH_URL: &str = "https://lite.duckduckgo.com/lite/";

pub(super) fn search(
    transport: &dyn WebTransport,
    query: &str,
    count: usize,
    cancel: &CancelToken,
) -> Result<Value, WebError> {
    let mut url = Url::parse(SEARCH_URL).expect("fixed search URL");
    url.query_pairs_mut().append_pair("q", query);
    let response = transport.execute(
        &HttpRequest {
            url: url.to_string(),
            method: "GET",
            headers: vec![
                (
                    "user-agent".into(),
                    "CLAT Web Search (https://github.com/artec/clat)".into(),
                ),
                ("accept".into(), "text/html".into()),
            ],
            body: Vec::new(),
            timeout: TIMEOUT,
        },
        cancel,
    )?;
    check(cancel)?;
    match response.status {
        202 | 403 | 429 => return Err(WebError::new("public_search_blocked")),
        200 => {}
        status => return Err(WebError::new("provider_failed").detail(format!("HTTP {status}"))),
    }
    if response.body.len() > MAX_BYTES {
        return Err(WebError::new("too_large"));
    }
    if !response
        .content_type
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/html"))
    {
        return Err(WebError::new("invalid_response"));
    }
    let sources = parse(&response.body, count, cancel)?;
    Ok(json!({"provider":"duckduckgo","sources":sources,"external_untrusted":true}))
}

fn parse(body: &[u8], count: usize, cancel: &CancelToken) -> Result<Vec<Value>, WebError> {
    let dom = config::plain()
        .parse_html(body)
        .map_err(|_| WebError::new("invalid_response"))?;
    let mut pending = vec![dom.document.clone()];
    let mut results: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut current = None;
    let mut empty = false;
    while let Some(node) = pending.pop() {
        check(cancel)?;
        if let Element { attrs, .. } = &node.data {
            let attrs = attrs.borrow();
            let attr = |key: &str| {
                attrs
                    .iter()
                    .find(|a| &*a.name.local == key)
                    .map(|a| a.value.as_ref())
            };
            let classes = attr("class").unwrap_or("");
            let has = |class: &str| classes.split_ascii_whitespace().any(|value| value == class);
            if attr("id") == Some("challenge-form") || has("anomaly-modal") {
                return Err(WebError::new("public_search_blocked"));
            }
            empty |= has("no-results") || has("result--no-result");
            if has("result-link") {
                if results.len() >= count {
                    break;
                }
                current = None;
                if let Some(url) = attr("href").and_then(result_url)
                    && seen.insert(url.to_string())
                    && let Some((title, _)) = text(&node, 256)
                {
                    current = Some(results.len());
                    results.push(
                        json!({"source":source(url.as_str(), &title),"text":"","truncated":false}),
                    );
                }
            } else if has("result-snippet")
                && let Some(index) = current.take()
                && let Some((snippet, truncated)) = text(&node, 512)
            {
                results[index]["text"] = json!(snippet);
                results[index]["truncated"] = json!(truncated);
            }
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
    if results.is_empty() && !empty {
        return Err(WebError::new("invalid_response"));
    }
    Ok(results)
}

fn result_url(href: &str) -> Option<Url> {
    let url = Url::parse(SEARCH_URL).ok()?.join(href).ok()?;
    let url = if matches!(
        url.host_str(),
        Some("duckduckgo.com" | "lite.duckduckgo.com")
    ) && url.path() == "/l/"
    {
        let (_, target) = url.query_pairs().find(|(key, _)| key == "uddg")?;
        validate_url(&target).ok()?
    } else {
        validate_url(url.as_str()).ok()?
    };
    // Result URLs are provenance only. Fetch independently revalidates DNS.
    (matches!(url.host(), Some(url::Host::Domain(_)))).then_some(url)
}

fn text(node: &Handle, limit: usize) -> Option<(String, bool)> {
    let mut html = Vec::new();
    node.serialize(&mut html).ok()?;
    let text = html2text::from_read_with_decorator(
        html.as_slice(),
        120,
        html2text::render::TrivialDecorator::new(),
    )
    .ok()?;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let excerpt: String = chars.by_ref().take(limit).collect();
    (!excerpt.is_empty()).then(|| (excerpt, chars.next().is_some()))
}
