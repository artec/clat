//! credentials implementation behind the stable model contract.
use super::*;

/// Provider-neutral persisted credentials. The JSON representation remains
/// the legacy string array so existing databases round-trip unchanged.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderCredentials {
    values: Vec<String>,
}

impl ProviderCredentials {
    pub fn for_protocol(protocol: ModelProtocol) -> Self {
        let field_count = match protocol {
            ModelProtocol::OpenAiResponses | ModelProtocol::OpenAiCompatible => 1,
        };
        Self {
            values: vec![String::new(); field_count],
        }
    }

    pub fn field_count(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn to_json(&self) -> Value {
        Value::Array(self.values.iter().cloned().map(Value::String).collect())
    }

    pub(crate) fn from_json(protocol: ModelProtocol, value: &Value) -> Self {
        let expected = Self::for_protocol(protocol).field_count();
        let mut values = value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        values.resize(expected, String::new());
        values.truncate(expected);
        Self { values }
    }

    pub fn field_label(protocol: ModelProtocol, index: usize) -> &'static str {
        match (protocol, index) {
            (ModelProtocol::OpenAiResponses, 0) | (ModelProtocol::OpenAiCompatible, 0) => "API Key",
            (ModelProtocol::OpenAiResponses, _) | (ModelProtocol::OpenAiCompatible, _) => {
                "Provider value"
            }
        }
    }

    pub fn masked_value(&self, index: usize) -> String {
        let Some(value) = self.values.get(index) else {
            return String::new();
        };
        if value.is_empty() {
            "<optional>".into()
        } else {
            "•".repeat(value.chars().count().min(48))
        }
    }

    pub fn push_char(&mut self, index: usize, ch: char) {
        if let Some(value) = self.values.get_mut(index) {
            value.push(ch);
        }
    }

    pub fn push_str(&mut self, index: usize, text: &str) {
        if let Some(value) = self.values.get_mut(index) {
            value.push_str(text);
        }
    }

    pub fn value(&self, index: usize) -> Option<&str> {
        self.values.get(index).map(String::as_str)
    }

    pub fn values(&self) -> &[String] {
        &self.values
    }

    pub fn set_value(&mut self, index: usize, value: String) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value;
        }
    }

    pub fn pop(&mut self, index: usize) {
        if let Some(value) = self.values.get_mut(index) {
            value.pop();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFieldKind {
    Secret,
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFieldDescriptor {
    pub key: String,
    pub label: String,
    pub kind: ProviderFieldKind,
    pub required: bool,
    pub sensitive: bool,
    pub has_value: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub protocol: ModelProtocol,
    pub display_name: String,
    pub fields: Vec<ProviderFieldDescriptor>,
}
