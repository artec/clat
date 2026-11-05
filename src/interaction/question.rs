//! Question payload and explicit client-owned presentation.
use super::AskOption;

/// 单问单答的问题（v1：一次调用一问；多问由模型多次调用）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AskQuestion {
    pub question: String,
    pub options: Vec<AskOption>,
    /// 是否提供自由输入。无选项时前端直接进入输入模式。
    pub allow_custom: bool,
    /// CLAT-owned copy only; model questions and stable answer values stay raw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<AskQuestionPresentation>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AskQuestionPresentation {
    pub question: crate::i18n::UiMessage,
    pub options: Vec<crate::i18n::UiMessage>,
}
impl AskQuestion {
    pub fn question_text(&self, locale: crate::i18n::LocaleContext) -> String {
        self.presentation
            .as_ref()
            .map_or_else(|| self.question.clone(), |p| locale.render(&p.question))
    }
    pub fn option_text(&self, index: usize, locale: crate::i18n::LocaleContext) -> String {
        self.presentation
            .as_ref()
            .and_then(|p| p.options.get(index))
            .map_or_else(
                || {
                    self.options
                        .get(index)
                        .map(|o| o.label.clone())
                        .unwrap_or_default()
                },
                |m| locale.render(m),
            )
    }
}

/// Model-facing argument contract. Frontend question DTOs remain backward compatible.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AskUserInput {
    question: String,
    #[serde(default)]
    options: Vec<AskUserOption>,
    #[serde(default = "allow_custom_default")]
    allow_custom: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserOption {
    label: String,
    description: Option<String>,
}

fn allow_custom_default() -> bool {
    true
}

impl AskUserInput {
    pub(crate) fn into_question(self) -> Result<AskQuestion, &'static str> {
        if !nonblank_within(&self.question, 2000) {
            return Err("question must be a non-empty string of at most 2000 characters");
        }
        if self.options.len() > 4 || (!self.allow_custom && self.options.is_empty()) {
            return Err(
                "provide up to four options and allow custom answers if there are no options",
            );
        }
        let mut seen = std::collections::HashSet::new();
        let mut options = Vec::new();
        for option in self.options {
            if !nonblank_within(&option.label, 100) || !seen.insert(option.label.trim().to_owned())
            {
                return Err(
                    "option labels must be distinct non-empty strings of at most 100 characters",
                );
            }
            if option
                .description
                .as_ref()
                .is_some_and(|text| text.chars().count() > 400)
            {
                return Err("option descriptions must be at most 400 characters");
            }
            options.push(AskOption {
                label: option.label,
                description: option.description,
            });
        }
        Ok(AskQuestion {
            question: self.question,
            options,
            allow_custom: self.allow_custom,
            presentation: None,
        })
    }
}

fn nonblank_within(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, LocaleContext, UiMessage};
    #[test]
    fn model_workflow_question_localization_preserves_wire_values_and_old_questions() {
        let mut question: AskQuestion = serde_json::from_value(serde_json::json!({
            "question":"original model question", "options":[{"label":"confirm","description":null}], "allow_custom":true
        })).unwrap();
        assert!(question.presentation.is_none());
        let presentation = AskQuestionPresentation {
            question: UiMessage::new("workflow.model_request_plan").arg("detail", "用户原文"),
            options: vec![UiMessage::new("workflow.model_confirm")],
        };
        question.presentation = Some(presentation.clone());
        for locale in [Locale::English, Locale::Simplified, Locale::Traditional] {
            let locale = LocaleContext::new(locale);
            assert_eq!(
                question.question_text(locale),
                locale.render(&presentation.question)
            );
            assert_eq!(
                question.option_text(0, locale),
                locale.render(&presentation.options[0])
            );
            assert_eq!(question.options[0].label, "confirm");
            assert_eq!(question.question, "original model question");
        }
    }
}
