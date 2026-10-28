use super::*;
use serde_json::{Value, json};
fn rpc(addr: SocketAddr, method: &str, value: Value) -> Value {
    let (_, reply) = post_rpc_json(addr, TEST_TOKEN, method, &value.to_string());
    assert_eq!(reply["ok"], true, "{method}: {reply}");
    reply["value"].clone()
}
#[test]
fn workflow_rpc_shares_plan_state_and_replays_approval_without_starting_a_run() {
    let (handle, storage, root) = spawn_serve("workflow-rpc", TestBehavior::Success);
    let mut events = SseClient::connect(handle.addr);
    rpc(
        handle.addr,
        "input.submit",
        json!({"text":"seed","clientMessageId":"seed","operationId":"seed"}),
    );
    events.wait_settled();
    let mut value = rpc(handle.addr, "workflow.details", json!({}));
    let session = rpc(handle.addr, "session.info", json!({}))["session_id"].clone();
    rpc(
        handle.addr,
        "workflow.action",
        json!({"operationId":"plan-mode","session":session,"expectedVersion":value["version"],"expectedGeneration":value["generation"],"intent":{"action":"mode","mode":"plan"}}),
    );
    value = rpc(handle.addr, "workflow.details", json!({}));
    assert_eq!(value["plan_mode"], true);
    let doc = rpc(
        handle.addr,
        "workflow.action",
        json!({"operationId":"save-plan","session":session,"expectedVersion":value["version"],"expectedGeneration":value["generation"],"intent":{"action":"save_plan","id":null,"revision":0,"text":"# Network workflow\n\nBuild and validate."}}),
    );
    value = rpc(handle.addr, "workflow.details", json!({}));
    assert_eq!(value["documents"][0]["text"], doc["text"]);
    let request = json!({"operationId":"approve-plan","session":session,"expectedVersion":value["version"],"expectedGeneration":value["generation"],"intent":{"action":"approve","plan":doc["id"],"revision":1,"execute":false}});
    let first = rpc(handle.addr, "workflow.action", request.clone());
    assert_eq!(rpc(handle.addr, "workflow.action", request), first);
    value = rpc(handle.addr, "workflow.details", json!({}));
    assert_eq!(value["plan_mode"], false);
    assert_eq!(value["documents"][0]["approved"], true);
    assert!(value["goal"]["goal"].is_null());
    assert_eq!(value["handoff"], false);
    cleanup(handle, &storage, &root);
}
