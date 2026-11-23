//! Small, redacted facts from a structured tool observation.
use serde_json::{Map, Value};

const FIELD_BUDGET: usize = 512;

pub(crate) fn tool_facts(output: &Value) -> Map<String, Value> {
    let parsed;
    let object = if let Some(text) = output.as_str() {
        parsed = serde_json::from_str::<Value>(text).ok();
        parsed.as_ref().and_then(Value::as_object)
    } else {
        output.as_object()
    };
    let Some(object) = object else {
        return Map::new();
    };
    // Keep test/process conclusions even if the output contains many incidental scalars.
    let priority = [
        "exit_code",
        "status",
        "result",
        "success",
        "passed",
        "failed",
        "summary",
    ];
    let mut facts = Map::new();
    let mut bytes = 2;
    if let Some(receipt) = object
        .get("verification_receipt")
        .and_then(Value::as_object)
    {
        let receipt = verification_facts(receipt);
        if !receipt.is_empty() {
            let value = Value::Object(receipt);
            bytes += value.to_string().len() + r#""verification_receipt":,"#.len();
            facts.insert("verification_receipt".into(), value);
        }
    }
    for key in priority.into_iter().chain(
        object
            .keys()
            .map(String::as_str)
            .filter(|key| !priority.contains(key)),
    ) {
        let Some(value) = object.get(key) else {
            continue;
        };
        if key.len() > 64
            || crate::redact::credential_name(key)
            || matches!(key, "stdout" | "stderr" | "pty" | "command" | "diagnostics")
        {
            continue;
        }
        let value = match value {
            Value::String(text) if text.len() <= 160 => {
                Value::String(crate::redact::redact_secrets(text))
            }
            Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
            _ => continue,
        };
        let size = serde_json::to_string(key).expect("string").len() + value.to_string().len() + 2;
        if bytes + size > FIELD_BUDGET {
            continue;
        }
        bytes += size;
        facts.insert(key.to_owned(), value);
    }
    facts
}

fn verification_facts(receipt: &Map<String, Value>) -> Map<String, Value> {
    let mut facts = Map::new();
    const COUNTS: &[&str] = &["executed", "passed", "failed", "ignored", "flaky"];
    if let Some(status) = receipt.get("status").and_then(Value::as_str)
        && matches!(status, "passed" | "failed" | "inconclusive" | "running")
    {
        facts.insert("status".into(), Value::from(status));
    }
    for &key in COUNTS {
        if let Some(count) = receipt.get(key).and_then(Value::as_u64) {
            facts.insert(key.into(), Value::from(count));
        }
    }
    for key in ["authenticated", "scan_complete"] {
        if let Some(value) = receipt.get(key).and_then(Value::as_bool) {
            facts.insert(key.into(), Value::from(value));
        }
    }
    facts
}
