//! Presentation decoding of the existing context wire shape, never estimation.
use crate::i18n::{LocaleContext, UiMessage};
use serde_json::Value;

const INVALID: &str = "Host returned an invalid context estimate";

const FIELDS: &[&str] = &[
    "base_prompt",
    "project_instructions",
    "plan_policy",
    "skill_catalog",
    "invoked_skill",
    "goal_policy",
    "tool_schemas",
    "history",
    "image_count",
    "image_original_count",
    "image_offloaded_count",
    "image_bytes",
    "image_tokens",
    "output_reserve",
    "input",
    "total",
];

pub(super) fn display(value: &Value) -> Result<String, String> {
    display_localized(value, LocaleContext::default())
}
pub(super) fn display_localized(value: &Value, locale: LocaleContext) -> Result<String, String> {
    let mut lines = vec![
        locale.render(&UiMessage::new("cli.context_heading").arg("unit", string(value, "unit")?)),
    ];
    for field in FIELDS {
        lines.push(
            locale.render(
                &UiMessage::new("cli.context_number")
                    .arg("label", locale.text(&format!("context.{field}")))
                    .arg("value", number(value, field)?),
            ),
        );
    }
    lines.push(locale.render(
        &UiMessage::new("cli.context_safety").arg("factor", number(value, "image_safety_factor")?),
    ));
    lines.push(
        locale.render(
            &UiMessage::new("cli.context_memory")
                .arg("value", number(value, "memory")?)
                .arg("budget", number(value, "memory_budget_bytes")?),
        ),
    );
    lines.push(
        locale.render(
            &UiMessage::new("cli.context_estimator").arg("name", string(value, "estimator")?),
        ),
    );
    for field in ["tools", "skills"] {
        let names = value[field]
            .as_array()
            .ok_or(INVALID)?
            .iter()
            .map(|item| item.as_str().ok_or(INVALID))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(
            locale.render(
                &UiMessage::new("cli.context_number")
                    .arg("label", locale.text(&format!("context.{field}")))
                    .arg("value", names.join(", ")),
            ),
        );
    }
    lines.push(locale.text("cli.context_diagnostics_header").into());
    let diagnostics = value["skill_diagnostics"].as_array().ok_or(INVALID)?;
    if diagnostics.is_empty() {
        lines.push(locale.text("cli.context_diagnostics_none").into());
    }
    for item in diagnostics {
        let name = match &item["name"] {
            Value::Null => "-",
            Value::String(name) => name,
            _ => return Err(INVALID.into()),
        };
        lines.push(format!(
            "  {} / {name} / {}: {}",
            string(item, "source")?,
            string(item, "kind")?,
            string(item, "message")?
        ));
    }
    Ok(lines.join("\n"))
}

fn number(value: &Value, field: &str) -> Result<u64, String> {
    value[field].as_u64().ok_or_else(|| INVALID.into())
}

fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field].as_str().ok_or_else(|| INVALID.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_wire_display_preserves_estimates_and_rejects_missing_data() {
        let mut value = json!({"unit":"tokens", "estimator":"host-test", "tools":["read_file"], "skills":["review"],
            "skill_diagnostics":[{"source":"project", "name":null, "kind":"invalid", "message":"bad catalog"}],
            "base_prompt":1,"project_instructions":2,"plan_policy":3,"skill_catalog":4,"invoked_skill":5,"goal_policy":6,
            "tool_schemas":7,"history":8,"image_count":9,"image_original_count":10,"image_offloaded_count":11,
            "image_bytes":12,"image_tokens":13,"output_reserve":14,"input":15,"total":16,
            "image_safety_factor":2,"memory":17,"memory_budget_bytes":18});
        let text = crate::host_client::HostClient::info_dialog_text(
            &json!({"kind":"context", "context":value}),
        )
        .unwrap();
        for locale in [
            crate::i18n::Locale::Simplified,
            crate::i18n::Locale::Traditional,
        ] {
            let translated = crate::host_client::HostClient::info_dialog_text_localized(
                &json!({"kind":"context","context":value}),
                crate::i18n::LocaleContext::new(locale),
            )
            .unwrap();
            assert_ne!(translated, text);
            assert!(translated.contains("host-test"));
            assert!(translated.contains("read_file"));
            assert!(translated.contains("project / - / invalid: bad catalog"));
            let mut invalid = value.clone();
            invalid.as_object_mut().unwrap().remove("total");
            assert!(
                crate::host_client::HostClient::info_dialog_text_localized(
                    &json!({"kind":"context","context":invalid}),
                    crate::i18n::LocaleContext::new(locale)
                )
                .is_err()
            );
        }
        for expected in [
            "Total estimate: 16",
            "Visual token estimate: 13",
            "Memory injection: 17 / 18 bytes",
            "Older images omitted: 11",
            "Tools: read_file",
            "Skills: review",
            "project / - / invalid: bad catalog",
            "Estimator: host-test",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        value["total"] = Value::Null;
        assert_eq!(display(&value).unwrap_err(), INVALID);
        value["total"] = json!(-1);
        assert!(display(&value).is_err());
        value["total"] = json!(16);
        value["tools"] = json!([42]);
        assert!(display(&value).is_err());
    }
}
