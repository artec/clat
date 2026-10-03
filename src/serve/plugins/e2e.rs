//! Test-only scripted model: the installed tool still crosses real permissions,
//! real MCP transport, the compiled recipe and the loopback provider fixture.
pub(crate) struct PluginSearchScript;
impl crate::test_support::TestModelScript for PluginSearchScript {
    fn stream(
        &self,
        request: crate::ModelRequest<'_>,
        events: &mut dyn crate::ModelEventSink,
    ) -> Result<crate::ModelResponse, crate::ModelError> {
        let tool = request
            .tools
            .iter()
            .find(|t| t.name.ends_with("_web_search"));
        let result = request
            .items
            .last()
            .is_some_and(|item| matches!(item, crate::ModelItem::ToolResult(_)));
        let calls = if result {
            Vec::new()
        } else {
            tool.map(|tool| {
                vec![crate::ToolCall {
                    id: "plg2-fixture-search".into(),
                    name: tool.name.clone(),
                    arguments: serde_json::json!({"queries":["clat"]}),
                }]
            })
            .unwrap_or_default()
        };
        if calls.is_empty() {
            events.emit(crate::ModelEvent::TextDelta {
                delta: "done".into(),
            });
        }
        Ok(crate::ModelResponse {
            text: if calls.is_empty() {
                "done".into()
            } else {
                String::new()
            },
            finish_reason: if calls.is_empty() {
                crate::FinishReason::Completed
            } else {
                crate::FinishReason::ToolCalls
            },
            tool_calls: calls,
            usage: None,
            provider_response_id: None,
            provider_state: Vec::new(),
            reasoning: None,
        })
    }
}
