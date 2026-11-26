//! First-request latency policy for recognizable conversational inputs.
use crate::model::{
    ContentPart, ModelConfig, ModelItem, ModelOptions, ThinkingLevel, effective_thinking_level,
    model_thinking_levels,
};
use serde_json::{Value, json};

pub(in crate::application) fn tune(
    config: &ModelConfig,
    item: &ModelItem,
    options: &mut ModelOptions,
) -> Option<ThinkingLevel> {
    let ModelItem::User { content } = item else {
        return None;
    };
    let [ContentPart::Text(text)] = content.as_slice() else {
        return None;
    };
    let preset = super::super::dto::matching_preset(config)?;
    let vendor = config.vendor();
    if preset.protocol != crate::model::ModelProtocol::OpenAiCompatible
        || !config.overrides.thinking_level.is_inherit()
        || config.thinking_level.is_some()
        || config
            .extra_body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            != preset.reasoning_effort
        || options.provider_options.get("reasoning_effort").is_some()
        || !(options.provider_options.is_null() || options.provider_options.is_object())
        || !model_thinking_levels(vendor, &config.model).contains(&ThinkingLevel::Low)
        || effective_thinking_level(config)? <= ThinkingLevel::Low
        || !is_simple_reply(text)
    {
        return None;
    }

    // The supported routes accept a top-level effort. Keep the thinking
    // object, reasoning replay, message history and tool schemas intact.
    options.provider_options["reasoning_effort"] = json!(ThinkingLevel::Low.wire_effort(vendor));
    Some(ThinkingLevel::Low)
}

fn is_simple_reply(text: &str) -> bool {
    let text = text.trim();
    if text.contains(['\n', '\r']) || text.chars().take(65).count() > 64 {
        return false;
    }
    let normalized = text
        .trim_end_matches(['.', '。', '!', '！', '?', '？', '～', '~'])
        .to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "你好"
            | "您好"
            | "早上好"
            | "晚上好"
            | "谢谢"
            | "謝謝"
            | "多谢"
            | "多謝"
            | "明白了"
            | "知道了"
            | "了解了"
            | "懂了"
            | "hi"
            | "hello"
            | "hey"
            | "good morning"
            | "good evening"
            | "thanks"
            | "thank you"
            | "got it"
            | "understood"
    ) {
        return true;
    }
    is_definition_question(&normalized) || is_quantity_confirmation(&normalized)
}

fn is_definition_question(text: &str) -> bool {
    let subject = ["是什么意思", "是什麼意思", "什么意思", "什麼意思", "是啥"]
        .into_iter()
        .find_map(|suffix| text.strip_suffix(suffix))
        .or_else(|| text.strip_prefix("什么是"))
        .or_else(|| text.strip_prefix("什麼是"))
        .or_else(|| text.strip_prefix("what does ")?.strip_suffix(" mean"))
        .or_else(|| text.strip_prefix("what is a "))
        .or_else(|| text.strip_prefix("what is an "))
        .or_else(|| {
            text.strip_prefix("what is ").filter(|term| {
                term.split_whitespace().count() == 1
                    && !matches!(
                        *term,
                        "this" | "that" | "it" | "wrong" | "happening" | "missing"
                    )
            })
        })
        .or_else(|| text.strip_prefix("define "));
    subject.map(str::trim).is_some_and(|term| {
        !term.is_empty()
            && term.chars().take(29).count() <= 28
            && term.split_whitespace().take(5).count() <= 4
            && !term.contains(['。', '！', '？', '，', '、', '；', '!', '?', ',', ';'])
    })
}

fn is_quantity_confirmation(text: &str) -> bool {
    // A short acknowledgement can include one sentence of context before
    // the actual choice. The choice must still have a bounded quantity.
    let text = text
        .split_once('。')
        .filter(|(context, _)| {
            ["嗯，", "嗯,", "对，", "對，", "是的，"]
                .into_iter()
                .any(|prefix| context.starts_with(prefix))
        })
        .map_or(text, |(_, choice)| choice.trim());
    if let Some(choice) = ["那就", "就", "只要"]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix))
    {
        let choice = choice.trim().trim_end_matches('吧').trim();
        return choice
            .split_once(['个', '個'])
            .is_some_and(|(quantity, noun)| {
                matches!(quantity, "一" | "两" | "兩" | "二" | "三" | "1" | "2" | "3")
                    && matches!(
                        noun,
                        "字段"
                            | "欄位"
                            | "选项"
                            | "選項"
                            | "按钮"
                            | "按鈕"
                            | "入口"
                            | "版本"
                            | "方案"
                            | "参数"
                            | "參數"
                    )
            });
    }
    let choice = text
        .strip_prefix("then just ")
        .or_else(|| text.strip_prefix("just "));
    choice
        .and_then(|choice| choice.split_once(' '))
        .is_some_and(|(quantity, noun)| {
            matches!(quantity, "one" | "two" | "three" | "1" | "2" | "3")
                && matches!(
                    noun.trim_end_matches('s'),
                    "field" | "option" | "button" | "version" | "parameter"
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Override, presets::preset_by_id};

    fn preset_config(id: &str) -> ModelConfig {
        let mut config = ModelConfig::default();
        preset_by_id(id).unwrap().apply(&mut config);
        config
    }

    #[test]
    fn short_reply_tunes_known_simple_turns_and_preserves_provider_settings() {
        let config = preset_config("glm-5.3-flash");
        for question in [
            "嗯，现在确实还没开 GitHub 的 Sponsors 功能。那就一个字段吧。",
            "GFM 是什么意思？",
            "GFM 是什麼意思？",
            "什么是 Markdown？",
            "What does GFM mean?",
            "What is Markdown?",
            "Just one field.",
            "谢谢！",
        ] {
            let mut options = ModelOptions {
                provider_options: json!({"stream_options":{"include_usage":true}}),
                ..ModelOptions::default()
            };
            assert_eq!(
                tune(&config, &ModelItem::user_text(question), &mut options),
                Some(ThinkingLevel::Low),
                "{question}"
            );
            assert_eq!(
                options.provider_options,
                json!({"reasoning_effort":"low","stream_options":{"include_usage":true}})
            );
            assert_eq!(config.extra_body["reasoning_effort"], "high");
            assert_eq!(config.extra_body["thinking"]["clear_thinking"], false);
        }
    }

    #[test]
    fn short_reply_respects_user_settings_and_leaves_work_unsimplified() {
        let config = preset_config("glm-5.3-flash");
        let unsupported_low = preset_config("opencode-deepseek-v4-pro");
        let mut pinned = config.clone();
        pinned.overrides.thinking_level = Override::Set(ThinkingLevel::Max);
        let simple = ModelItem::user_text("你好");
        let attached = ModelItem::User {
            content: vec![
                ContentPart::Text("你好".into()),
                ContentPart::Image {
                    path: "image.png".into(),
                    media_type: "image/png".into(),
                },
            ],
        };
        let shared = ModelItem::User {
            content: vec![
                ContentPart::Text("你好".into()),
                ContentPart::Text("Shared history".into()),
            ],
        };
        for (config, item) in [
            (&config, ModelItem::user_text("好，继续")),
            (&config, ModelItem::user_text("What is wrong?")),
            (&config, ModelItem::user_text("修一下缓存")),
            (&config, ModelItem::user_text("你好\n开始修改")),
            (&config, attached),
            (&config, shared),
            (&pinned, simple.clone()),
            (&unsupported_low, simple),
        ] {
            let mut options = ModelOptions::default();
            assert_eq!(tune(config, &item, &mut options), None);
            assert!(options.provider_options.is_null());
        }
    }
}
