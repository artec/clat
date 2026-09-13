//! Non-secret tuning form projection; host owns validation and vendor mapping.
use super::*;
use crate::Override;

impl ModelEditor {
    pub(super) fn read_remote_tuning(
        config: &mut ModelConfig,
        value: &Value,
    ) -> Result<(), String> {
        if value.is_null() {
            return Ok(());
        }
        let tuning: crate::ModelOverrides =
            serde_json::from_value(value.clone()).map_err(|_| "invalid host tuning fields")?;
        config.temperature = match tuning.temperature {
            Override::Set(value) => Some(value),
            _ => None,
        };
        config.parallel_tool_calls = match tuning.parallel_tool_calls {
            Override::Set(value) => value,
            _ => true,
        };
        config.thinking_level = match tuning.thinking_level {
            Override::Set(value) => Some(value),
            _ => None,
        };
        config.overrides.temperature = tuning.temperature;
        config.overrides.parallel_tool_calls = tuning.parallel_tool_calls;
        config.overrides.thinking_level = tuning.thinking_level;
        Ok(())
    }

    pub(super) fn remote_tuning_values(&self) -> Result<Value, String> {
        let temperature = if self.override_is_clear(RowKind::Temperature) {
            None
        } else {
            parse_optional_f64(&self.temperature, "Temperature")?
        };
        if temperature.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err("Temperature must be a finite non-negative number".into());
        }
        let parallel = if self.override_is_clear(RowKind::Parallel) {
            Override::Clear
        } else {
            Override::Set(self.parallel_tool_calls)
        };
        let thinking = if self.override_is_clear(RowKind::Thinking) {
            Override::Clear
        } else {
            self.thinking_level.map_or(Override::Clear, Override::Set)
        };
        Ok(serde_json::json!({
            "temperature":temperature.map_or(Override::Clear, Override::Set),
            "parallel_tool_calls":parallel, "thinking_level":thinking
        }))
    }

    pub(super) fn append_remote_tuning(&self, params: &mut Value) -> Result<(), String> {
        let Some(original) = &self.remote_tuning else {
            return Ok(());
        };
        let mut current = self.remote_tuning_values()?;
        if self.remote_body == Some(true) {
            current
                .as_object_mut()
                .expect("tuning object")
                .remove("thinking_level");
        }
        if original["route"] == self.remote_route_identity() {
            current
                .as_object_mut()
                .expect("tuning object")
                .retain(|key, value| original["values"].get(key) != Some(value));
        }
        if current.as_object().is_some_and(|fields| !fields.is_empty()) {
            params["tuning"] = current;
        }
        Ok(())
    }

    pub(super) fn remote_route_identity(&self) -> Value {
        serde_json::json!([
            self.profile.as_ref().map(|profile| profile.name.trim()),
            self.protocol,
            self.model.trim(),
            self.endpoint.trim(),
            self.request_path.trim()
        ])
    }
}
