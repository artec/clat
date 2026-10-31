use super::*;
use crate::host_client::HostClient;
fn start(storage: &Path, project: &Path) -> ServeHandle {
    crate::serve::serve_with_with_queue(
        Project::new(project),
        Some(storage.to_owned()),
        ServeArgs {
            port: 0,
            token: Some(TEST_TOKEN.into()),
            no_open: true,
            ..Default::default()
        },
        |bootstrap| {
            bootstrap
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Success,
                }))
        },
        Arc::new(AtomicBool::new(false)),
        super::super::super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .unwrap()
}
fn set_policy(storage: &Path, disabled: &[&str]) {
    std::fs::write(
        storage.join("builtin-plugins.json"),
        json!({"unit":{"name":"builtin-plugins","version":1},"disabled":disabled}).to_string(),
    )
    .unwrap();
}
fn call(client: &HostClient, method: &str, params: Value) -> Value {
    client.call_with_receipt(method, &params).unwrap()
}
#[test]
fn phase4_reopen_keeps_history_and_committed_image_receipt_but_denies_new_images() {
    let (storage, root, project) = setup("phase4-image-reopen");
    prepare_storage(&project, &storage, TestBehavior::Success);
    let image = root.join("original.png");
    std::fs::write(&image, png_bytes(12, 12)).unwrap();
    let first = start(&storage, &root);
    let client = HostClient::connect(first.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    let mut stream = SseClient::connect(first.addr);
    let scope = call(&client, "draft.open", json!({"clientDraftId":"phase4"}));
    let (status, upload) = upload_image(
        first.addr,
        scope["draftScopeId"].as_str().unwrap(),
        "image/png",
        "original.png",
        &png_bytes(12, 12),
    );
    assert_eq!(status, 200);
    let upload = upload["value"]["uploadId"].clone();
    let request = json!({"text":"original text","clientMessageId":"retained-image","attachments":[upload],"draftScopeId":scope["draftScopeId"]});
    let accepted = call(&client, "prompt.send", request.clone());
    stream.wait_settled();
    let history = call(&client, "session.history", json!({}));
    let attachment = accepted["receipt"]["attachment_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    drop(stream);
    drop(client);
    first.shutdown();
    let exit = first.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    set_policy(&storage, &["builtin.development", "builtin.draft_images"]);
    let second = start(&storage, &root);
    let client = HostClient::connect(second.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    assert!(client.existing_clipboard_drafts().is_none());
    assert!(client.clipboard_drafts().is_err());
    assert!(client.existing_clipboard_drafts().is_none());
    assert_eq!(
        call(&client, "prompt.send", request.clone())["receipt"],
        accepted["receipt"]
    );
    assert_eq!(call(&client, "session.history", json!({})), history);
    let mut forged = request.clone();
    forged["clientMessageId"] = json!("new-image");
    assert!(client.call_with_receipt("prompt.send", &forged).is_err());
    assert!(client.call_with_receipt("steer.send", &forged).is_err());
    let selection = call(&client, "workbench.info", json!({}))["selection_generation"]
        .as_u64()
        .unwrap();
    assert!(
        client
            .submit_message(
                "retain local text",
                &[root.join("must-not-read.png")],
                false,
                selection
            )
            .unwrap_err()
            .presentation
            .is_some()
    );
    for method in ["editor.open", "git.snapshot", "terminal.list", "draft.open"] {
        let error = client
            .call_with_receipt(
                method,
                &json!({"clientDraftId":"late","path":"must-not-read"}),
            )
            .unwrap_err();
        assert_eq!(
            error.presentation.as_deref().map(|p| p.key.as_str()),
            Some("extension.disabled_builtin"),
            "{method}: {error:?}"
        );
    }
    assert_eq!(call(&client, "session.history", json!({})), history);
    let raw = get_raw_response(
        second.addr,
        &format!("/api/attachments/{attachment}"),
        TEST_TOKEN,
    );
    let (status, _, bytes) = parse_raw_response_bytes(&raw);
    assert_eq!(status, 200);
    assert!(bytes.starts_with(b"\x89PNG"));
    assert!(image.exists());
    drop(client);
    second.shutdown();
    let exit = second.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    let reopened = start(&storage, &root);
    let client = HostClient::connect(reopened.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    assert_eq!(
        call(&client, "workbench.info", json!({}))["builtin_availability"]["builtin.draft_images"]
            ["enabled"],
        false
    );
    assert_eq!(
        call(&client, "prompt.send", request.clone())["receipt"],
        accepted["receipt"]
    );
    let mut stream = SseClient::connect(reopened.addr);
    call(
        &client,
        "prompt.send",
        json!({"text":"plain text after disabled reopen","clientMessageId":"disabled-plain-text"}),
    );
    stream.wait_settled();
    drop(stream);
    drop(client);
    reopened.shutdown();
    let exit = reopened.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    set_policy(&storage, &[]);
    let third = start(&storage, &root);
    let client = HostClient::connect(third.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    assert!(
        client.call_with_receipt("prompt.send", &forged).is_err(),
        "old upload authority cannot revive"
    );
    assert!(
        client
            .call_with_receipt("draft.open", &json!({"clientDraftId":"fresh"}))
            .is_ok()
    );
    drop(client);
    cleanup(third, &storage, &root);
}
