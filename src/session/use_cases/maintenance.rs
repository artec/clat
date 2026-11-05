//! Current projected messages for core-owned maintenance decisions; no plugin or file access.
use super::*;
impl SessionService {
    pub(crate) fn measure_request(
        &self,
        config: &crate::ModelConfig,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[crate::ToolDefinition],
    ) -> crate::model::RequestMeasurement {
        let session = self
            .active_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        self.meter.lock().expect("request meter").measure(
            &session,
            config,
            instructions,
            items,
            tools,
        )
    }
    pub(crate) fn observe_request(
        &self,
        config: &crate::ModelConfig,
        instructions: Option<&str>,
        tools: &[crate::ToolDefinition],
        usage: &crate::Usage,
        estimated_input: u64,
    ) {
        let session = self
            .active_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        self.meter.lock().expect("request meter").observe(
            &session,
            config,
            instructions,
            tools,
            usage,
            estimated_input,
        );
    }

    pub(crate) fn compaction_nodes(&self) -> Result<Vec<(u64, ModelItem, bool)>, SessionError> {
        let checkpoints = self.compaction_checkpoints();
        Ok(self
            .surface_nodes()?
            .into_iter()
            .map(|(seq, item)| (seq, item, checkpoints.contains_key(&seq)))
            .collect())
    }
    /// Positions, rather than seq ordering: system replacements can be newer than adjacent history.
    pub(crate) fn compaction_boundaries(&self) -> Result<Vec<usize>, SessionError> {
        let nodes = self.surface_nodes()?;
        let mut counts = std::collections::HashMap::new();
        for (seq, _) in &nodes {
            *counts.entry(*seq).or_insert(0usize) += 1;
        }
        let mut offset = 0;
        let mut boundaries = Vec::new();
        for event in self.maintenance_events() {
            if matches!(
                event.event_type.as_str(),
                "system/message" | "developer/message"
            ) {
                boundaries.push(offset);
            }
            offset += counts.get(&event.seq).copied().unwrap_or(0);
        }
        Ok(boundaries)
    }
    pub(crate) fn compaction_checkpoints(&self) -> std::collections::BTreeMap<u64, String> {
        self.maintenance_events()
            .into_iter()
            .filter(crate::session::catalog::is_context_checkpoint)
            .filter_map(|event| {
                let content = event.data["content"].as_array()?;
                if content.len() != 1 || content[0]["type"] != "text" {
                    return None;
                }
                Some((event.seq, content[0]["text"].as_str()?.to_owned()))
            })
            .collect()
    }
    pub(crate) fn offloaded_images(&self) -> u64 {
        fn count(value: &Value) -> u64 {
            match value {
                Value::Object(object)
                    if object.get("type").and_then(Value::as_str) == Some("image") =>
                {
                    u64::from(object.get("offloaded") == Some(&Value::Bool(true)))
                }
                Value::Object(object) => object.values().map(count).sum(),
                Value::Array(values) => values.iter().map(count).sum(),
                _ => 0,
            }
        }
        self.maintenance_events()
            .iter()
            .map(|event| count(&event.data))
            .sum()
    }
    pub(crate) fn maintenance_events(&self) -> Vec<SessionEvent> {
        let active = self.active.lock().expect("active");
        active
            .as_ref()
            .map(|active| {
                active
                    .projections
                    .lock()
                    .expect("projections")
                    .maintenance_events()
            })
            .unwrap_or_default()
    }
}
