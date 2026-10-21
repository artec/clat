//! Classify credential values for structural disclosure. Unlike diagnostic
//! redaction, ordinary private configuration words are not schema secrets.
use serde_json::Value;
/// One known-value set for all public text: private config plus credential env.
/// Structural checks deliberately keep the narrower credential-only set below.
pub(super) fn public_values(env: &[(String, String)]) -> Vec<String> {
    let mut public = crate::redact::plugin_config_values(env);
    public.extend(values(env));
    public.sort_by_key(|s| std::cmp::Reverse(s.len()));
    public.dedup();
    public
}

pub(super) fn values(env: &[(String, String)]) -> Vec<String> {
    let mut values = Vec::new();
    for (key, value) in env {
        if key == "CLAT_PLUGIN_CONFIG" {
            if let Ok(value) = serde_json::from_str(value) {
                collect(&value, false, &mut values);
            }
        } else if crate::redact::credential_name(key) && !value.is_empty() {
            values.push(value.clone());
        }
    }
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    values.dedup();
    values
}
fn collect(value: &Value, sensitive: bool, values: &mut Vec<String>) {
    match value {
        Value::String(text) if sensitive && !text.is_empty() => values.push(text.clone()),
        Value::Array(items) => {
            for value in items {
                collect(value, sensitive, values);
            }
        }
        Value::Object(items) => {
            for (key, value) in items {
                collect(
                    value,
                    sensitive || crate::redact::credential_name(key),
                    values,
                );
            }
        }
        _ => {}
    }
}
pub(super) fn contains(text: &str, values: &[String]) -> bool {
    crate::tool::capabilities::schema::marked_secret(text)
        || values.iter().any(|value| text.contains(value.as_str()))
}
pub(super) fn private_header(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) || crate::redact::credential_name(key)
}
