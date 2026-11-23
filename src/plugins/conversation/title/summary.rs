use super::{TITLE_RAW_MAX_BYTES, sanitize_title_outcome, scrub_title_text};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default)]
struct ModelOutput {
    title: serde_json::Value,
    summary: serde_json::Value,
}

/// 负责人裁定（2026-11-23）：80/40 只是提示词约束——模型自己决定怎么理解；
/// CLAT 不验证、不截断。产出只做控制字符清洗后原样入库，清洗后为空不算
/// 摘要。
pub(super) fn read_model_output(
    raw: &str,
    conversation: &str,
    include_title: bool,
) -> (String, Option<String>) {
    if raw.len() > TITLE_RAW_MAX_BYTES {
        return (String::new(), None);
    }
    let raw = raw.trim();
    let json = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```"))
        .and_then(|inner| inner.trim().strip_suffix("```"))
        .unwrap_or(raw);
    let (title, summary) = match serde_json::from_str::<ModelOutput>(json) {
        Ok(output) => (
            output.title.as_str().unwrap_or_default().to_owned(),
            output.summary.as_str().map(str::to_owned),
        ),
        Err(_) if !raw.starts_with(['{', '[', '`']) => (raw.to_owned(), None),
        Err(_) => (String::new(), None),
    };
    let title = if include_title {
        sanitize_title_outcome(&title, conversation).unwrap_or_default()
    } else {
        String::new()
    };
    let summary = summary
        .map(|text| scrub_title_text(&text))
        .filter(|text| !text.trim().is_empty());
    (title, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries_pass_through_whatever_the_model_returned() {
        // 中英混排超限文本原样通过：没有 80/40 执法，没有省略号截断。
        let mixed = format!("{} with renderMarkdown API checks", "已完成测试".repeat(30));
        let output = json!({"title":"Sidebar progress", "summary": mixed}).to_string();
        let (title, summary) = read_model_output(&output, "", true);
        assert_eq!(title, "Sidebar progress");
        assert_eq!(summary.as_deref(), Some(mixed.as_str()));

        // 清洗后为空不算摘要；title 与 summary 相互独立。
        let (title, summary) = read_model_output(r#"{"summary": "  "}"#, "", true);
        assert!(title.is_empty());
        assert_eq!(summary, None);
        let (title, summary) =
            read_model_output(r#"{"title":"Valid title","summary":null}"#, "", true);
        assert_eq!(title, "Valid title");
        assert!(summary.is_none());
    }
}
