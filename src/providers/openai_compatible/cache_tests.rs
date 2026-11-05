use super::*;
use crate::model::ModelOptions;
use crate::tool::{ToolDefinition, ToolEffect};

fn body(
    endpoint: &str,
    thinking: Value,
    options: Value,
    tools: bool,
    items: &[ModelItem],
) -> Value {
    let config = ModelConfig {
        endpoint: endpoint.into(),
        extra_body: json!({"thinking": thinking}),
        ..Default::default()
    };
    let model = OpenAiCompatibleModel::from_runtime_fields(vec![], &config).unwrap();
    let definitions = [ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type":"object"}),
        effect: ToolEffect::Read,
        strict: false,
    }];
    model
        .request_body(ModelRequest {
            instructions: Some("stable system"),
            items,
            tools: if tools { &definitions } else { &[] },
            options: &ModelOptions {
                provider_options: options,
                ..Default::default()
            },
            cancel: &CancelToken::new(),
        })
        .unwrap()
}

fn history() -> Vec<ModelItem> {
    vec![
        ModelItem::user_text("first"),
        ModelItem::assistant_with_reasoning("answer", Some("exact reasoning\n  unchanged".into())),
        ModelItem::user_text("next"),
    ]
}

#[test]
fn cache_replay_deepseek_keeps_plain_answer_reasoning_with_tools() {
    let request = body(
        "https://api.deepseek.com",
        json!({"type":"enabled"}),
        json!({}),
        true,
        &history(),
    );
    assert_eq!(
        request["messages"][2]["reasoning_content"],
        "exact reasoning\n  unchanged"
    );
}

#[test]
fn cache_replay_glm_preserved_thinking_includes_plain_answers_without_tools() {
    let request = body(
        "https://open.bigmodel.cn/api/coding/paas/v4",
        json!({"type":"enabled","clear_thinking":false}),
        json!({}),
        false,
        &history(),
    );
    assert_eq!(
        request["messages"][2]["reasoning_content"],
        "exact reasoning\n  unchanged"
    );
}

#[test]
fn cache_replay_uses_effective_options_and_keeps_unknown_endpoints_conservative() {
    for (endpoint, thinking, options, tools) in [
        (
            "https://api.deepseek.com",
            json!({"type":"enabled"}),
            json!({}),
            false,
        ),
        (
            "https://open.bigmodel.cn/api/paas/v4",
            json!({"clear_thinking":false}),
            json!({"thinking":{"clear_thinking":true}}),
            true,
        ),
        (
            "https://example.test",
            json!({"clear_thinking":false}),
            json!({}),
            true,
        ),
    ] {
        let request = body(endpoint, thinking, options, tools, &history());
        assert!(request["messages"][2].get("reasoning_content").is_none());
    }
}

#[test]
fn cache_replay_append_only_history_preserves_prior_message_prefix() {
    let mut items = history();
    items.pop();
    let first = body(
        "https://api.deepseek.com",
        json!({"type":"enabled"}),
        json!({}),
        true,
        &items,
    );
    items.push(ModelItem::user_text("next"));
    let next = body(
        "https://api.deepseek.com",
        json!({"type":"enabled"}),
        json!({}),
        true,
        &items,
    );
    let prefix = first["messages"].as_array().unwrap();
    assert_eq!(
        &next["messages"].as_array().unwrap()[..prefix.len()],
        prefix
    );
    assert_eq!(first["tools"], next["tools"]);
}

#[test]
fn cache_replay_coding_default_and_explicit_standard_endpoint_opt_in() {
    for (endpoint, thinking) in [
        ("https://open.bigmodel.cn/api/coding/paas/v4", json!({})),
        ("https://api.z.ai/api/coding/paas/v4", json!({})),
        (
            "https://api.z.ai/api/paas/v4",
            json!({"clear_thinking":false}),
        ),
    ] {
        let request = body(endpoint, thinking, json!({}), false, &history());
        assert_eq!(
            request["messages"][2]["reasoning_content"],
            "exact reasoning\n  unchanged"
        );
    }
}

#[test]
fn cache_replay_does_not_fabricate_missing_reasoning_or_match_lookalike_hosts() {
    let items = [
        ModelItem::user_text("hi"),
        ModelItem::assistant_text("answer"),
    ];
    let request = body(
        "https://api.deepseek.com",
        json!({}),
        json!({}),
        true,
        &items,
    );
    assert!(request["messages"][2].get("reasoning_content").is_none());
    let request = body(
        "https://api.deepseek.com.example.test",
        json!({}),
        json!({}),
        true,
        &history(),
    );
    assert!(request["messages"][2].get("reasoning_content").is_none());
}

#[test]
fn request_tail_preserves_carried_messages_when_scratch_changes() {
    let config = ModelConfig::default();
    let model = OpenAiCompatibleModel::from_runtime_fields(vec![], &config).unwrap();
    let mut items = vec![ModelItem::user_text("first")];
    let request = |items: &[ModelItem], path: &str| {
        model
            .request_body(ModelRequest {
                instructions: Some("stable system"),
                items,
                tools: &[],
                options: &ModelOptions {
                    request_tail: Some(path.into()),
                    ..Default::default()
                },
                cancel: &CancelToken::new(),
            })
            .unwrap()
    };
    let first = request(&items, "Host run context: scratch-a");
    items.extend([
        ModelItem::assistant_text("answer"),
        ModelItem::user_text("next"),
    ]);
    let next = request(&items, "Host run context: scratch-b");
    let first = first["messages"].as_array().unwrap();
    let next = next["messages"].as_array().unwrap();
    assert_eq!(&first[..2], &next[..2]);
    assert_eq!(first[2]["content"], "Host run context: scratch-a");
    assert_eq!(next[4]["content"], "Host run context: scratch-b");
    assert!(
        next.iter()
            .all(|item| item["content"] != "Host run context: scratch-a")
    );
}
