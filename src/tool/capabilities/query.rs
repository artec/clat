use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
pub(super) const MAX_REPLY: usize = 4096;
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Query {
    pub text: String,
    pub name: String,
    pub source: String,
    pub input: String,
    pub output: String,
    pub scope: String,
    pub id: String,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub schema_offset: usize,
}
impl Query {
    pub fn parse(value: &Value) -> Result<Self, ToolError> {
        if serde_json::to_vec(value).map_err(err)?.len() > 2048 {
            return Err(err("capability request exceeds 2048 bytes"));
        }
        let query: Self = serde_json::from_value(value.clone())
            .map_err(|_| err("invalid capability request fields"))?;
        if query.text.len() > 256
            || query.text.split_whitespace().count() > 8
            || [
                &query.name,
                &query.source,
                &query.input,
                &query.output,
                &query.scope,
                &query.id,
            ]
            .iter()
            .any(|s| s.len() > 128)
            || query.cursor.as_ref().is_some_and(|s| s.len() > 256)
            || query.limit.is_some_and(|n| n == 0 || n > 4)
            || query.schema_offset > 65536
        {
            return Err(err("capability request limit exceeded"));
        }
        Ok(query)
    }
    pub fn fingerprint(&self) -> String {
        let mut value = serde_json::to_value(self).expect("query JSON");
        value.as_object_mut().unwrap().remove("cursor");
        digest(&serde_json::to_vec(&value).unwrap())
    }
    pub fn matches(&self, entry: &super::view::Entry) -> Option<Vec<String>> {
        let d = &entry.definition;
        let m = &entry.metadata;
        if !self.name.is_empty() && self.name != d.name {
            return None;
        }
        if !self.source.is_empty()
            && self.source != m.source
            && self.source != m.adapter
            && self.source != entry.owner.as_str()
        {
            return None;
        }
        let mut reasons = Vec::new();
        if !self.name.is_empty() {
            reasons.push("name:exact match".into());
        }
        if !self.source.is_empty() {
            reasons.push("source:exact declaration/owner match".into());
        }
        let text = format!(
            "{} {} {}",
            d.name,
            clip(&d.description, 16384),
            m.tags.join(" ")
        )
        .to_lowercase();
        for term in self.text.to_lowercase().split_whitespace() {
            if !text.contains(term) {
                return None;
            }
            reasons.push(format!("text:{}", clip(term, 64)));
        }
        for (key, wanted, actual) in [
            ("input", &self.input, &m.inputs),
            ("output", &self.output, &m.outputs),
        ] {
            if wanted.is_empty() {
                continue;
            }
            if actual.is_empty() {
                reasons.push(format!("{key}:undeclared; needs confirmation"));
            } else if actual.iter().any(|v| v.eq_ignore_ascii_case(wanted)) {
                reasons.push(format!("{key}:declared match"));
            } else {
                return None;
            }
        }
        if !self.scope.is_empty() {
            if m.scope.is_empty() || m.scope == "unknown" {
                reasons.push("scope:unknown; needs confirmation".into());
            } else if m.scope != self.scope {
                return None;
            } else {
                reasons.push("scope:declared match".into());
            }
        }
        Some(reasons)
    }
}
pub(super) fn err(e: impl std::fmt::Display) -> ToolError {
    ToolError::new(e.to_string())
}
pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))[..16].into()
}
pub(super) fn clip(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let redacted = crate::redact::redact_secrets(&text[..end]);
    let mut end = redacted.len().min(bytes);
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    redacted[..end].into()
}

/// Preview budgets count actual JSON escaping; incomplete fields stay labelled partial.
pub(super) fn preview(text: &str, serialized_bytes: usize) -> String {
    let text = clip(text, serialized_bytes);
    let mut result = String::new();
    for ch in text.chars() {
        let old = result.len();
        result.push(ch);
        if serde_json::to_vec(&result).expect("string JSON").len() > serialized_bytes {
            result.truncate(old);
            break;
        }
    }
    result
}
pub(super) fn labels(labels: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for label in labels {
        let label = preview(label, 64);
        result.push(label);
        if serde_json::to_vec(&result).expect("labels JSON").len() > 128 {
            result.pop();
            break;
        }
    }
    result
}
