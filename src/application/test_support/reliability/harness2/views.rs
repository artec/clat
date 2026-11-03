//! Read-only export at the actual replacement carrier, never the log-only summary event.
use super::*;

pub(in crate::application::reliability_tests) fn export(
    storage: &Path,
    output: &Path,
    session: &str,
) {
    let events = super::super::super::tests::load_events(storage);
    let mut views = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if event.event_type != "user/message"
            || event.surface_op.is_none()
            || index == 0
            || events[index - 1].event_type != "compaction/summary"
        {
            continue;
        }
        let before = items(&events[..index]);
        let after = items(&events[..=index]);
        let candidate = after
            .iter()
            .find(|node| node["seq"] == event.seq)
            .expect("replacement must be visible at this boundary");
        let selected = event.source_event_seqs.clone().unwrap_or_default();
        let body = candidate["text"].as_str().unwrap_or_default();
        let valid = body
            .split_once("<compacted-summary>")
            .and_then(|(_, body)| body.strip_suffix("</compacted-summary>"))
            .is_some_and(|body| crate::plugins::valid_checkpoint_structure(body.trim()));
        views.push(json!({"session":session,"revision":event.seq,"selected_seqs":selected,"before":before,"after":after,"candidate":candidate,"structure_valid":valid}));
    }
    write_new(
        &output.join("maintenance-views.json"),
        &json!({"views":views,"boundary":"actual durable replacement carrier"}),
    );
}
fn items(events: &[crate::session::event::SessionEvent]) -> Vec<Value> {
    let surface = crate::session::surface::Surface::fold(events).unwrap();
    crate::session::adapter::surface_to_model_items_with_seq(events, &surface)
        .unwrap()
        .into_iter()
        .map(|(seq, item)| {
            let (role, text) = match &item {
                crate::ModelItem::User { content } => ("user", parts(content)),
                crate::ModelItem::Assistant { content, .. } => ("assistant", parts(content)),
                crate::ModelItem::ToolCall(call) => {
                    ("tool_call", serde_json::to_string(call).unwrap())
                }
                crate::ModelItem::ToolResult(result) => ("tool_result", result.output.to_string()),
                crate::ModelItem::ProviderState(_) => (
                    "provider_state",
                    "[opaque state; not factual evidence]".into(),
                ),
            };
            json!({"seq":seq,"role":role,"text":text})
        })
        .collect()
}
fn parts(parts: &[crate::ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            crate::ContentPart::Text(text) => text.as_str(),
            crate::ContentPart::Image { .. } => {
                "[image part; this textual export cannot establish its pixels]"
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
