//! Independent SSE reconstruction, deliberately not using the adapter parser.
use super::*;
use std::collections::BTreeMap;

pub(super) fn compare(wire: &Value, outcome: &Value) -> Value {
    if outcome["finish"].is_null() || outcome["finish"] == "Cancelled" {
        return json!({"checked":false,"passed":null,"reason":"no terminal parsed response to compare; preserve error or cancellation separately"});
    }
    let raw = wire["consumed_sse"].as_str().unwrap().replace("\r\n", "\n");
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = BTreeMap::<u64, (String, String, String)>::new();
    let mut finish = None;
    let mut valid = wire["overflow"] == false;
    for frame in raw.split("\n\n") {
        let data = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            valid = false;
            continue;
        };
        let choice = &value["choices"][0];
        let delta = &choice["delta"];
        text.push_str(delta["content"].as_str().unwrap_or_default());
        reasoning.push_str(delta["reasoning_content"].as_str().unwrap_or_default());
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let Some(index) = call["index"].as_u64() else {
                valid = false;
                continue;
            };
            let item = calls.entry(index).or_default();
            item.0.push_str(call["id"].as_str().unwrap_or_default());
            item.1
                .push_str(call["function"]["name"].as_str().unwrap_or_default());
            item.2
                .push_str(call["function"]["arguments"].as_str().unwrap_or_default());
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            finish = Some(match reason {
                "stop" => "Completed",
                "length" => "MaxTokens",
                "tool_calls" => "ToolCalls",
                "content_filter" => "Refused",
                _ => "Unknown",
            });
        }
    }
    let calls = calls
        .into_values()
        .map(|(id, name, args)| {
            let arguments = serde_json::from_str::<Value>(&args).ok();
            valid &= arguments.is_some();
            json!({"id":id,"name":name,"arguments":arguments})
        })
        .collect::<Vec<_>>();
    let emitted = wire["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "text")
        .filter_map(|e| e["delta"].as_str())
        .collect::<String>();
    let text_equal = outcome["text"] == text;
    let reasoning_equal = outcome["reasoning"].as_str().unwrap_or_default() == reasoning;
    let tools_equal = outcome["tool_calls"] == json!(calls);
    let finish_equal = finish.is_some_and(|f| outcome["finish"] == f);
    json!({"checked":true,"passed":valid&&text_equal&&reasoning_equal&&tools_equal&&finish_equal&&emitted==text,"text_equal":text_equal,"reasoning_equal":reasoning_equal,"tools_equal":tools_equal,"finish_equal":finish_equal,"events_equal":emitted==text,"valid_frames":valid})
}

#[test]
fn reliability_wire_comparison_detects_text_and_tool_argument_corruption() {
    let frames = [
        json!({"choices":[{"delta":{"content":"星河"}}]}),
        json!({"choices":[{"delta":{"reasoning_content":"thought","tool_calls":[{"index":0,"id":"a","function":{"name":"fixed","arguments":"{\"id\":2}"}}]},"finish_reason":"tool_calls"}]}),
    ];
    let raw = frames
        .iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>();
    let wire =
        json!({"consumed_sse":raw,"overflow":false,"events":[{"kind":"text","delta":"星河"}]});
    let mut outcome = json!({"text":"星河","reasoning":"thought","tool_calls":[{"id":"a","name":"fixed","arguments":{"id":2}}],"finish":"ToolCalls"});
    assert_eq!(compare(&wire, &outcome)["passed"], true);
    outcome["tool_calls"][0]["arguments"]["id"] = json!(3);
    assert_eq!(compare(&wire, &outcome)["tools_equal"], false);
    outcome["text"] = json!("bad");
    assert_eq!(compare(&wire, &outcome)["text_equal"], false);
    assert_eq!(
        compare(&wire, &json!({"error":"HTTP 500"}))["passed"],
        Value::Null
    );
    assert_eq!(
        compare(&wire, &json!({"finish":"Cancelled"}))["checked"],
        false
    );
}
