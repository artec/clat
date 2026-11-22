use super::{TITLE_RAW_MAX_BYTES, sanitize_title_outcome, scrub_title_text};
use crate::i18n::Locale;
use crate::session::catalog::conversation_summary::fit_text;
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default)]
struct ModelOutput {
    title: serde_json::Value,
    summary: serde_json::Value,
}

pub(super) fn read_model_output(
    raw: &str,
    conversation: &str,
    locale: Locale,
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
    (
        title,
        summary.and_then(|text| fit_text(&scrub_title_text(&text), locale)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::catalog::conversation_summary::within_limit;
    use serde_json::json;

    #[test]
    fn conversation_summary_bounds_model_output_and_keeps_valid_titles_independent() {
        let output = json!({"title":"Sidebar progress", "summary":"测试".repeat(60)}).to_string();
        for locale in [Locale::Simplified, Locale::Traditional] {
            let (title, zh) = read_model_output(&output, "", locale, true);
            assert_eq!(title, "Sidebar progress");
            let zh = zh.unwrap();
            assert_eq!(zh.chars().count(), 80);
            assert!(zh.ends_with('…'));
        }

        let output = json!({"title":"Sidebar progress", "summary":vec!["progress"; 45].join(" ")})
            .to_string();
        let (title, en) = read_model_output(&output, "", Locale::English, false);
        assert!(title.is_empty());
        let en = en.unwrap();
        assert!(within_limit(&en, Locale::English));
        assert!(en.ends_with("progress…"));
        let (title, summary) = read_model_output(
            r#"{"title":"Valid title","summary":null}"#,
            "",
            Locale::Traditional,
            true,
        );
        assert_eq!(title, "Valid title");
        assert!(summary.is_none());
    }
}
