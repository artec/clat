//! Presentation decoding of the existing context wire shape, never estimation.
use serde_json::Value;

const INVALID: &str = "Host returned an invalid context estimate";

pub(super) fn display(value: &Value) -> Result<String, String> {
    let mut lines = vec![format!("Context estimate · {}", string(value, "unit")?)];
    for (field, label) in [
        ("base_prompt", "Base prompt"),
        ("project_instructions", "Project instructions"),
        ("plan_policy", "Plan policy"),
        ("skill_catalog", "Skill catalog"),
        ("invoked_skill", "Invoked skill"),
        ("goal_policy", "Goal policy"),
        ("tool_schemas", "Tool schemas"),
        ("history", "History / compaction view"),
        ("image_count", "Images"),
        ("image_original_count", "Images before projection"),
        ("image_offloaded_count", "Older images omitted"),
        ("image_bytes", "Image bytes"),
        ("image_tokens", "Visual token estimate"),
        ("output_reserve", "Output reserve"),
        ("input", "Input estimate"),
        ("total", "Total estimate"),
    ] {
        lines.push(format!("{label}: {}", number(value, field)?));
    }
    lines.push(format!(
        "Visual safety factor: {}.0x",
        number(value, "image_safety_factor")?
    ));
    lines.push(format!(
        "Memory injection: {} / {} bytes",
        number(value, "memory")?,
        number(value, "memory_budget_bytes")?
    ));
    lines.push(format!("Estimator: {}", string(value, "estimator")?));
    for (field, label) in [("tools", "Tools"), ("skills", "Skills")] {
        let names = value[field]
            .as_array()
            .ok_or(INVALID)?
            .iter()
            .map(|item| item.as_str().ok_or(INVALID))
            .collect::<Result<Vec<_>, _>>()?;
        lines.push(format!("{label}: {}", names.join(", ")));
    }
    lines.push("Skill diagnostics".into());
    let diagnostics = value["skill_diagnostics"].as_array().ok_or(INVALID)?;
    if diagnostics.is_empty() {
        lines.push("  none".into());
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
