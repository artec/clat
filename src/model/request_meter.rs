//! Ephemeral provider anchor, scoped to one session, route and exact effective request prefix.
use super::*;
use sha2::{Digest, Sha256};
#[derive(Default)]
pub(crate) struct RequestMeter {
    anchor: Option<Anchor>,
}
struct Anchor {
    key: Vec<u8>,
    items: u64,
    actual: u64,
}
impl RequestMeter {
    pub(crate) fn measure(
        &self,
        session: &str,
        config: &ModelConfig,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
    ) -> RequestMeasurement {
        let mut measured = RequestMeasurement::new(config, instructions, items, tools);
        if let Some(anchor) = &self.anchor
            && anchor.key == key(session, config, instructions, tools)
        {
            let input = if measured.input >= anchor.items {
                anchor.actual.saturating_add(measured.input - anchor.items)
            } else {
                anchor.actual.saturating_sub(anchor.items - measured.input)
            };
            measured.input = measured.input.max(input);
        }
        measured
    }
    pub(crate) fn observe(
        &mut self,
        session: &str,
        config: &ModelConfig,
        instructions: Option<&str>,
        items: &[ModelItem],
        tools: &[ToolDefinition],
        usage: &Usage,
    ) {
        if usage.input_tokens == 0 {
            return;
        }
        self.anchor = Some(Anchor {
            key: key(session, config, instructions, tools),
            items: estimate_request_tokens(instructions, items, tools),
            actual: usage.input_tokens,
        });
    }
}
fn key(
    session: &str,
    config: &ModelConfig,
    instructions: Option<&str>,
    tools: &[ToolDefinition],
) -> Vec<u8> {
    let mut digest = Sha256::new();
    for value in [
        session,
        &config.protocol.to_string(),
        &config.model,
        &config.endpoint,
        &config.request_path,
        instructions.unwrap_or_default(),
    ] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    digest.update(serde_json::to_vec(&serde_json::json!({"tools":tools.iter().map(|tool|serde_json::json!({"name":tool.name,"description":tool.description,"schema":tool.input_schema})).collect::<Vec<_>>(),"output":config.output_limit,"body":config.extra_body,"headers":config.extra_headers})).expect("plain request prefix"));
    digest.finalize().to_vec()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_anchor_never_crosses_session_route_or_effective_header() {
        let mut meter = RequestMeter::default();
        let mut config = ModelConfig::default();
        let items = [ModelItem::user_text("hello")];
        let baseline = RequestMeasurement::new(&config, Some("current"), &items, &[]).input;
        meter.observe(
            "a",
            &config,
            Some("current"),
            &items,
            &[],
            &Usage {
                input_tokens: baseline + 1000,
                ..Default::default()
            },
        );
        assert_eq!(
            meter
                .measure("a", &config, Some("current"), &items, &[])
                .input,
            baseline + 1000
        );
        for (session, header) in [("b", "current"), ("a", "changed")] {
            assert_eq!(
                meter
                    .measure(session, &config, Some(header), &items, &[])
                    .input,
                RequestMeasurement::new(&config, Some(header), &items, &[]).input
            );
        }
        config.endpoint = "https://different.invalid".into();
        assert_eq!(
            meter
                .measure("a", &config, Some("current"), &items, &[])
                .input,
            baseline
        );
    }
}
