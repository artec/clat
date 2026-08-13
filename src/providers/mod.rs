pub mod openai;
pub mod openai_compatible;

use crate::{Model, ModelConfig, ModelError, ModelProtocol};

pub use openai::OpenAiModel;
pub use openai_compatible::OpenAiCompatibleModel;

#[derive(Clone, Default)]
pub struct ProviderRuntime {
    values: Vec<String>,
}

impl ProviderRuntime {
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

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.values
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )
    }

    pub(crate) fn from_json(protocol: ModelProtocol, value: &serde_json::Value) -> Self {
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
            (ModelProtocol::OpenAiResponses | ModelProtocol::OpenAiCompatible, 0) => "API Key",
            _ => "Provider value",
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

    pub fn build_model(&self, config: &ModelConfig) -> Result<Box<dyn Model>, ModelError> {
        match config.protocol {
            ModelProtocol::OpenAiResponses => Ok(Box::new(OpenAiModel::from_runtime_fields(
                self.values.clone(),
                config.model.trim(),
                config.endpoint.trim(),
            )?)),
            ModelProtocol::OpenAiCompatible => Ok(Box::new(
                OpenAiCompatibleModel::from_runtime_fields(self.values.clone(), config)?,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_provider_runtime_values() {
        let mut runtime = ProviderRuntime::for_protocol(ModelProtocol::OpenAiResponses);
        runtime.push_str(0, "abcdef");
        assert_eq!(runtime.masked_value(0), "••••••");
        assert_eq!(
            ProviderRuntime::field_label(ModelProtocol::OpenAiResponses, 0),
            "API Key"
        );
    }
}
