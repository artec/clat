//! WEB-D1（2026-11-20 负责人裁定）：DeepSeek Messages 原生搜索执行器。
//!
//! 未配置自有 web_search key 且当前模型为 DeepSeek 厂商时，`web_search`
//! 默认改走 DeepSeek 内部免费搜索（对齐 DSH rc.5 `f47a0a2b52` 的默认
//! 化）：Anthropic 兼容端点 `https://api.deepseek.com/anthropic/v1/messages`
//! 加服务端工具 `web_search_20250305`——搜索在模型服务端跑，无按次
//! 收费，成本 = 一次辅助模型 turn 的时延与 token。端点与对话端点相互
//! 独立（DSH `$DEEPSEEK_SEARCH_BASE_URL` 同义）；这里钉官方常量，不
//! 新增协议枚举——这只是 web_tools 内部的一次 HTTP 调用。
//!
//! 纪律（任务书 docs/todo/webd1-deepseek-native-search-2026-11-20.md）：
//! - 凭据：普通 DeepSeek API key（`x-api-key` + `Authorization: Bearer`
//!   双头，官方与 Anthropic 兼容代理任一解析）；无账号体系。
//! - INV-WD3：结果只取结构化 `web_search_tool_result` 块的
//!   `web_search_result` 项（url/title/snippet/page_age）；回复正文
//!   永不当作搜索结果来源。`cited_text`（text 块按 URL 的引用摘录）
//!   只用于给缺 snippet 的项补摘录。
//! - INV-WD5：401/无结果块/网络失败 → 结构化错误透传（含"搜索端点与
//!   对话端点独立配置"的指引口径），绝不静默回落 Ceramic。
//! - 不吃 PaidCallBudget：预算闸只属于按次计费的 Search API 路径。

use super::config::Snapshot;
use super::provider::validate_url;
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const MESSAGES_URL: &str = "https://api.deepseek.com/anthropic/v1/messages";
const SEARCH_MODEL: &str = "deepseek-v4-flash";
const MAX_TOKENS: u32 = 4096;
/// 服务端工具单请求最大搜索次数（DSH 默认同款）。
const MAX_USES: u32 = 5;
const ANTHROPIC_VERSION: &str = "2023-06-01";

impl WebService {
    pub(super) fn search_messages(
        &self,
        query: &str,
        count: u64,
        snapshot: &Snapshot,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<Value, WebError> {
        check(req_cancel)?;
        let key = snapshot
            .messages_key
            .as_deref()
            .ok_or_else(|| WebError::new("credential_missing"))?;
        let body = json!({
            "model": SEARCH_MODEL,
            "max_tokens": MAX_TOKENS,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": format!("Perform a web search for the query: {query}"),
                }],
            }],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": MAX_USES,
            }],
            "thinking": {"type": "disabled"},
        });
        let response = self.consent_execute(
            HttpRequest {
                url: MESSAGES_URL.into(),
                method: "POST",
                body: serde_json::to_vec(&body).unwrap(),
                timeout: TIMEOUT,
                consented: Vec::new(),
                headers: vec![
                    ("x-api-key".into(), key.into()),
                    ("authorization".into(), format!("Bearer {key}")),
                    ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
                    ("content-type".into(), "application/json".into()),
                    ("accept".into(), "application/json".into()),
                ],
            },
            req_cancel,
            run_cancel,
        )?;
        check(req_cancel)?;
        if response.body.len() > MAX_BYTES {
            return Err(WebError::new("too_large"));
        }
        if response.status != 200 {
            // INV-WD5：失败诚实透传——401 是凭据问题，其余归 provider；
            // 详情带端点与"搜索端点独立于对话端点"的指引口径。
            return Err(WebError::new(match response.status {
                401 => "credential_invalid",
                429 => "rate_limit",
                _ => "provider_failed",
            })
            .detail(format!(
                "HTTP {} at {MESSAGES_URL}; the search endpoint is configured separately from the chat endpoint",
                response.status
            )));
        }
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|_| WebError::new("invalid_response"))?;
        parse_messages_search(&payload, count)
    }
}

/// INV-WD3 结构化只读解析：`web_search_tool_result` 块为唯一结果来源，
/// 无该块的响应按 provider_failed 严格失败（不从回复文本里刮 URL）；
/// URL 去重保首现（`max_uses > 1` 可能跨次搜索重复）；DeepSeek 无结果
/// 数旋钮，超出 `count` 截断并在顶层置 `truncated`。
pub(super) fn parse_messages_search(payload: &Value, count: u64) -> Result<Value, WebError> {
    let blocks = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| WebError::new("invalid_response"))?;
    let result_blocks: Vec<&Value> = blocks
        .iter()
        .filter(|block| block["type"] == "web_search_tool_result")
        .collect();
    if result_blocks.is_empty() {
        return Err(WebError::new("provider_failed").detail(
            "no web_search_tool_result block in the Messages response; \
             the request may not have triggered native web search",
        ));
    }
    // url → cited_text（text 块引用摘录，首现胜出）：缺 snippet 项的
    // 补录来源，永不反向作为结果来源。
    let mut cited: BTreeMap<&str, &str> = BTreeMap::new();
    for block in blocks.iter().filter(|block| block["type"] == "text") {
        for cite in block["citations"].as_array().into_iter().flatten() {
            let (Some(url), Some(text)) = (
                cite["url"].as_str().filter(|u| !u.is_empty()),
                cite["cited_text"].as_str().filter(|t| !t.is_empty()),
            ) else {
                continue;
            };
            cited.entry(url).or_insert(text);
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut items = Vec::new();
    let mut dropped = false;
    for block in result_blocks {
        for item in block["content"].as_array().into_iter().flatten() {
            let url = item["url"]
                .as_str()
                .ok_or_else(|| WebError::new("invalid_response"))?;
            validate_url(url)?;
            if !seen.insert(url) {
                continue;
            }
            if items.len() >= count as usize {
                dropped = true;
                continue;
            }
            let title = item["title"].as_str().unwrap_or("");
            let mut reference = source(url, &title.chars().take(256).collect::<String>());
            reference.published_at = item["page_age"].as_str().map(str::to_owned);
            let snippet = item["snippet"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| cited.get(url).copied())
                .unwrap_or("");
            let (text, truncated) = bounded(snippet);
            items.push(json!({"source":reference,"text":text,"truncated":truncated}));
        }
    }
    let mut result =
        json!({"provider":"deepseek-messages","sources":items,"external_untrusted":true});
    if dropped {
        result["truncated"] = json!(true);
    }
    Ok(result)
}
