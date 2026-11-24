//! Provider calibration scoped to one session, route and effective request prefix.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RequestCalibration {
    key: String,
    estimate: u64,
    actual: u64,
}

impl RequestCalibration {
    pub(crate) const FIELD: &str = "clatRequestCalibration";

    pub(crate) fn observe(
        session: &str,
        config: &ModelConfig,
        instructions: Option<&str>,
        tools: &[ToolDefinition],
        usage: &Usage,
        estimated_input: u64,
    ) -> Option<Self> {
        (usage.input_tokens > 0).then(|| Self {
            key: key(session, config, instructions, tools),
            estimate: estimated_input,
            actual: usage.input_tokens,
        })
    }

    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        let value = payload.get(Self::FIELD)?;
        let calibration = Self::deserialize(value).ok()?;
        (calibration.actual > 0).then_some(calibration)
    }
}

pub(crate) fn measure_request(
    calibration: Option<&RequestCalibration>,
    session: &str,
    config: &ModelConfig,
    instructions: Option<&str>,
    items: &[ModelItem],
    tools: &[ToolDefinition],
    tail_tokens: u64,
) -> RequestMeasurement {
    let mut measured = RequestMeasurement::new(config, instructions, items, tools);
    measured.input = measured.input.saturating_add(tail_tokens);
    if let Some(anchor) = calibration
        && anchor.key == key(session, config, instructions, tools)
    {
        let input = if measured.input >= anchor.estimate {
            anchor
                .actual
                .saturating_add(measured.input - anchor.estimate)
        } else {
            anchor
                .actual
                .saturating_sub(anchor.estimate - measured.input)
        };
        measured.input = measured.input.max(input);
        measured.calibrated = true;
    }
    measured
}

fn key(
    session: &str,
    config: &ModelConfig,
    instructions: Option<&str>,
    tools: &[ToolDefinition],
) -> String {
    let mut digest = Sha256::new();
    // Bump the domain when the estimator or request identity changes incompatibly.
    digest.update(b"clat-request-calibration-v1");
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
    let tools: Vec<_> = tools.iter().map(|tool| serde_json::json!({
        "name": tool.name, "description": tool.description, "schema": tool.input_schema, "strict": tool.strict,
    })).collect();
    digest.update(serde_json::to_vec(&serde_json::json!({
        "tools": tools, "output": config.output_limit, "body": config.extra_body, "headers": config.extra_headers,
        "parallel_tool_calls": config.parallel_tool_calls,
    })).expect("plain request prefix"));
    format!("{:x}", digest.finalize())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase8_anchor_never_crosses_session_route_or_effective_header() {
        let mut config = ModelConfig::default();
        let items = [ModelItem::user_text("hello")];
        let baseline = RequestMeasurement::new(&config, Some("current"), &items, &[]).input;
        let estimated_tail = 120;
        let anchor = RequestCalibration::observe(
            "a",
            &config,
            Some("current"),
            &[],
            &Usage {
                input_tokens: baseline + estimated_tail + 1000,
                ..Default::default()
            },
            baseline + estimated_tail,
        );
        let measured = measure_request(
            anchor.as_ref(),
            "a",
            &config,
            Some("current"),
            &items,
            &[],
            0,
        );
        assert_eq!(measured.input, baseline + 1000);
        assert!(measured.calibrated);
        assert_eq!(
            measure_request(
                anchor.as_ref(),
                "a",
                &config,
                Some("current"),
                &items,
                &[],
                estimated_tail + 10
            )
            .input,
            baseline + estimated_tail + 1010
        );
        for (session, header) in [("b", "current"), ("a", "changed")] {
            let measured = measure_request(
                anchor.as_ref(),
                session,
                &config,
                Some(header),
                &items,
                &[],
                0,
            );
            assert_eq!(
                measured.input,
                RequestMeasurement::new(&config, Some(header), &items, &[]).input
            );
            assert!(!measured.calibrated);
        }
        config.endpoint = "https://different.invalid".into();
        let measured = measure_request(
            anchor.as_ref(),
            "a",
            &config,
            Some("current"),
            &items,
            &[],
            0,
        );
        assert_eq!(measured.input, baseline);
        assert!(!measured.calibrated);
    }
}
