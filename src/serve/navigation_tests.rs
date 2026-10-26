use super::*;
use serde_json::{Value, json};
fn call(handle: &ServeHandle, method: &str, params: Value) -> Value {
    let (status, value) = post_rpc_json(handle.addr, TEST_TOKEN, method, &params.to_string());
    assert_eq!(status, 200, "{method}: {value:?}");
    assert_eq!(value["ok"], true, "{method}: {value:?}");
    value["value"].clone()
}
#[test]
fn navigation_historical_images_and_bookmarks_follow_the_proven_message_not_selection() {
    let (handle, storage, root) = spawn_serve("navigation-history-library", TestBehavior::Success);
    let mut events = SseClient::connect(handle.addr);
    let draft = open_draft(handle.addr, "navigation-images");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let (_, uploaded) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "history-image.png",
        &png_bytes(24, 16),
    );
    call(
        &handle,
        "prompt.send",
        json!({"text":"","draftScopeId":scope,"attachments":[uploaded["value"]["uploadId"]],"clientMessageId":"navigation-original"}),
    );
    events.wait_settled();
    let session = call(&handle, "session.info", json!({}))["session_id"].clone();
    let reference = json!({"project":root.canonicalize().unwrap().to_string_lossy(),"session":session,"seq":null});
    let read = call(
        &handle,
        "history.conversation",
        json!({"reference":reference}),
    );
    let original = &read["messages"][0];
    assert_eq!(original["client_message_id"], "navigation-original");
    let reference = original["reference"].clone();
    let attachment = original["attachments"][0]["attachment_id"].clone();
    call(
        &handle,
        "library.bookmark.add",
        json!({"reference":reference}),
    );
    call(&handle, "session.new", json!({}));
    let selected = call(&handle, "session.info", json!({}))["session_id"].clone();
    let image = call(
        &handle,
        "history.attachment",
        json!({"reference":reference,"attachment_id":attachment}),
    );
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image["base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(crate::media::image_dimensions_bytes(&bytes), Some((24, 16)));
    assert_eq!(
        call(&handle, "session.info", json!({}))["session_id"],
        selected
    );
    let mut wrong = reference.clone();
    wrong["seq"] = json!(99999);
    assert!(
        post(
            handle.addr,
            TEST_TOKEN,
            "history.attachment",
            &json!({"reference":wrong,"attachment_id":attachment}).to_string()
        )
        .1
        .is_err()
    );
    assert!(
        post(
            handle.addr,
            TEST_TOKEN,
            "history.attachment",
            &json!({"reference":reference,"attachment_id":"../../credentials.json"}).to_string()
        )
        .1
        .is_err()
    );
    assert_eq!(
        call(&handle, "library.list", json!({}))["entries"][0]["reference"],
        reference
    );
    cleanup(handle, &storage, &root);
}

#[test]
fn navigation_bound_cancel_cannot_stop_an_unrelated_conversation() {
    let (handle, storage, root) = spawn_serve(
        "navigation-cancel-target",
        TestBehavior::TimedDeltas {
            count: 100,
            interval_ms: 100,
        },
    );
    let mut events = SseClient::connect(handle.addr);
    call(
        &handle,
        "prompt.send",
        json!({"text":"Stay running until explicitly cancelled"}),
    );
    events.wait_for_run_event("run_started", WAIT);
    let session = call(&handle, "session.info", json!({}))["session_id"].clone();
    let (_, rejected) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "run.cancel",
        &json!({"session_id":uuid::Uuid::new_v4().to_string()}).to_string(),
    );
    assert_eq!(rejected["ok"], false);
    assert!(call(&handle, "workbench.info", json!({}))["active_run"].is_object());
    call(&handle, "run.cancel", json!({"session_id":session}));
    events.wait_settled();
    cleanup(handle, &storage, &root);
}
