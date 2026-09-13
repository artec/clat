use super::*;
use serde_json::json;

fn wait_question(client: &mut SseClient) -> serde_json::Value {
    let deadline = Instant::now() + WAIT;
    loop {
        client.consume_available();
        if let Some(frame) = client.frames.iter().find(|frame| {
            frame.event.as_deref() == Some("notice")
                && ctl_of(frame)["kind"] == "question_requested"
        }) {
            return ctl_of(frame)["payload"].clone();
        }
        assert!(
            !client
                .frames
                .iter()
                .any(|frame| frame.event.as_deref() == Some("prompt.settled")),
            "host ask-user must wait for a human instead of returning unavailable"
        );
        assert!(Instant::now() < deadline, "question was not published");
        client.pump_once();
    }
}

#[test]
fn host_question_reconnect_and_first_valid_answer_win() {
    let behavior = TestBehavior::AskUser(Arc::new(crate::test_support::ScriptedAsker {
        selected: "not-used".into(),
        asked: Mutex::new(vec![]),
    }));
    let (handle, storage, project) = spawn_serve("host-question", behavior);
    let mut first = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "ask me");
    let question = wait_question(&mut first);
    let mut second = SseClient::connect(handle.addr);
    assert_eq!(wait_question(&mut second), question);
    let params = |value| {
        json!({"rpcId":question["rpc_id"],"answer":{"kind":"selected","value":value}}).to_string()
    };
    assert!(
        post(
            handle.addr,
            TEST_TOKEN,
            "question.respond",
            &params("invented")
        )
        .1
        .is_err()
    );
    post(
        handle.addr,
        TEST_TOKEN,
        "question.respond",
        &params("stable"),
    )
    .1
    .unwrap();
    assert!(
        post(handle.addr, TEST_TOKEN, "question.respond", &params("beta"))
            .1
            .is_err()
    );
    second.wait_settled();
    let (_, history) = post(handle.addr, TEST_TOKEN, "session.history", "{}");
    assert!(history.unwrap().to_string().contains("stable"));
    cleanup(handle, &storage, &project);
}

fn host_selection(client: &crate::client_ports::host::HostClient) -> u64 {
    let mut events = client.events().unwrap();
    loop {
        let (kind, payload) = events.next_frame().unwrap();
        if kind == "subscribed" {
            return payload["ctl"]["selection_generation"].as_u64().unwrap();
        }
    }
}

#[test]
fn native_image_submission_uses_uploads_and_keeps_admission_receipt() {
    use crate::client_ports::host::HostClient;
    let (handle, storage, project) = spawn_serve("native-upload", TestBehavior::Success);
    let client = HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    let mut sse = SseClient::connect(handle.addr);
    let selection = host_selection(&client);
    let image = project.join("native-image.png");
    std::fs::write(&image, png_bytes(24, 16)).unwrap();
    let accepted = client
        .submit_message("", std::slice::from_ref(&image), false, selection)
        .unwrap();
    assert_eq!(accepted["receipt"]["state"], "committed");
    assert_eq!(
        accepted["receipt"]["attachment_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sse.wait_settled();
    let history = client.call("session.history", &json!({})).unwrap();
    assert!(history.to_string().contains("image/png"));
    let rejected = client.submit_message(
        "must not be admitted",
        &[project.join("missing.png")],
        false,
        selection,
    );
    assert!(rejected.is_err());
    let after = client.call("session.history", &json!({})).unwrap();
    assert_eq!(
        history, after,
        "failed local upload must not admit a prompt"
    );
    let stale_running = client
        .submit_message(
            "late steering",
            std::slice::from_ref(&image),
            true,
            selection,
        )
        .unwrap_err();
    assert_eq!(stale_running.code.as_deref(), Some("not-running"));
    assert!(!stale_running.committed());
    assert_eq!(history, client.call("session.history", &json!({})).unwrap());
    assert!(
        !history
            .to_string()
            .contains(&image.to_string_lossy().to_string())
    );
    cleanup(handle, &storage, &project);
}

#[test]
fn reconnect_represents_each_admitted_input_once() {
    let (handle, storage, project) = spawn_serve("reconnect-input-once", TestBehavior::RunCommand);
    let mut first = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    first.wait_for("approval.requested", WAIT);
    let mut second = SseClient::connect(handle.addr);
    second.wait_for_run_event("tool_requested", WAIT);
    let historical_inputs = second
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .filter(|frame| replay_kind_of(frame) == "user_message")
        .count();
    let live_inputs = second
        .run_events()
        .iter()
        .filter(|event| crate::wire::wire_event_type_tag(event) == "run_started")
        .count();
    // This fresh session has exactly one admitted message. Count source facts,
    // not equal text or SSE ids, after the deterministic approval barrier.
    let observed = historical_inputs + live_inputs;
    cleanup(handle, &storage, &project);
    assert_eq!(
        observed, 1,
        "one admission must have one replay/live representation"
    );
}

#[test]
fn reconnect_keeps_identical_prior_input_and_settled_history() {
    let (handle, storage, project) = spawn_serve("reconnect-identical", TestBehavior::RunCommand);
    let mut first = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    let approval = first.wait_for("approval.requested", WAIT);
    let (_, answer) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &json!({
            "rpcId": ctl_of(&approval)["rpc_id"], "decision": "allow",
        })
        .to_string(),
    );
    answer.unwrap();
    first.wait_settled();
    prompt_send(handle.addr, "run echo");
    let approval = first.wait_for("approval.requested", WAIT);
    let mut second = SseClient::connect(handle.addr);
    second.wait_for_run_event("tool_requested", WAIT);
    let historical = second
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .filter(|frame| replay_kind_of(frame) == "user_message")
        .count();
    let live = second
        .run_events()
        .iter()
        .filter(|event| crate::wire::wire_event_type_tag(event) == "run_started")
        .count();
    assert_eq!(
        (historical, live),
        (1, 1),
        "equal text is two distinct admissions"
    );
    let (_, answer) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &json!({
            "rpcId": ctl_of(&approval)["rpc_id"], "decision": "allow",
        })
        .to_string(),
    );
    answer.unwrap();
    second.wait_settled();
    let third = SseClient::connect(handle.addr);
    let historical = third
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .filter(|frame| replay_kind_of(frame) == "user_message")
        .count();
    assert_eq!(
        historical, 2,
        "settled runs must return to journal-only replay"
    );
    assert!(third.run_events().is_empty());
    cleanup(handle, &storage, &project);
}

#[test]
fn reconnect_goal_admission_has_one_representation() {
    let (handle, storage, project) = spawn_serve("reconnect-goal", TestBehavior::RunCommand);
    let mut first = SseClient::connect(handle.addr);
    let (_, result) = post(
        handle.addr,
        TEST_TOKEN,
        "command.run",
        r#"{"command":"/goal create reconnect --run --rounds 1 --accept user"}"#,
    );
    result.unwrap();
    first.wait_for("approval.requested", WAIT);
    let mut second = SseClient::connect(handle.addr);
    second.wait_for_run_event("tool_requested", WAIT);
    let historical = second
        .frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("replay"))
        .filter(|frame| replay_kind_of(frame) == "user_message")
        .count();
    let live = second
        .run_events()
        .iter()
        .filter(|event| crate::wire::wire_event_type_tag(event) == "run_started")
        .count();
    cleanup(handle, &storage, &project);
    assert_eq!((historical, live), (0, 1));
}

#[test]
fn submission_transport_keeps_receipt_from_real_rpc_envelope() {
    use crate::client_ports::host::HostClient;
    let (handle, storage, project) =
        spawn_serve_with_start_receive_failure("native-committed-error");
    let client = HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    let selection = host_selection(&client);
    let error = client.submit_text("hello", false, selection).unwrap_err();
    assert!(
        error.committed(),
        "post-commit failure must keep its durable receipt: {error:?}"
    );
    assert_eq!(error.code.as_deref(), Some("internal"));
    let receipt = error.receipt.unwrap();
    assert!(receipt.committed_message_id.is_some());
    assert!(!receipt.client_message_id.is_empty());
    assert!(!receipt.retryable);
    let history = client.call("session.history", &json!({})).unwrap();
    assert!(
        history.to_string().contains("hello"),
        "receipt must describe actual durable input"
    );
    cleanup(handle, &storage, &project);
}

#[test]
fn subscription_prefix_survives_run_replacement_without_taking_the_next_run() {
    let (storage, project_root, project) = setup("subscription-prefix-owner");
    prepare_storage(&project, &storage, TestBehavior::Success);
    let app = BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .into_trusted_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    let shared = ServeShared::new(Arc::new(Mutex::new(app)), "unit".into(), 0);
    let event = |text| crate::RunEvent::SteeringApplied {
        message: crate::message::MessageContent::text(text),
        client_message_id: None,
        request_digest: None,
        receipt: None,
    };
    assert!(shared.try_claim_run("first", 0));
    shared.fanout_run_event(&event("first-prefix"));
    let (_, queue, prefix) = shared.register_subscriber();
    shared.release_run_claim();
    assert!(shared.try_claim_run("second", 1));
    shared.fanout_run_event(&event("second-live"));
    let prefix = prefix.unwrap();
    assert_eq!(
        prefix,
        vec![super::super::shapes::realtime_data(&event("first-prefix"))]
    );
    assert_eq!(
        queue.try_recv().unwrap().data,
        super::super::shapes::realtime_data(&event("second-live"))
    );
    assert!(queue.try_recv().is_err());
    shared.release_run_claim();
    drop(shared);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project_root);
}

#[test]
fn late_client_receives_pending_approval_and_both_clients_observe_resolution() {
    let (handle, storage, project) = spawn_serve("late-approval", TestBehavior::RunCommand);
    let mut first = SseClient::connect(handle.addr);
    prompt_send(handle.addr, "run echo");
    let requested = first.wait_for("approval.requested", WAIT);
    let id = approval_rpc_id(&requested);
    let mut second = SseClient::connect(handle.addr);
    let recovered = second.wait_for("approval.requested", WAIT);
    assert_eq!(approval_rpc_id(&recovered), id);
    let (_, result) = post(
        handle.addr,
        TEST_TOKEN,
        "approval.respond",
        &json!({"rpcId":id,"decision":"deny"}).to_string(),
    );
    result.unwrap();
    for client in [&mut first, &mut second] {
        loop {
            let resolved = client.wait_for("notice", WAIT);
            let data: serde_json::Value = serde_json::from_str(&resolved.data).unwrap();
            if data["ctl"]["kind"] == "approval_resolved" {
                assert_eq!(data["ctl"]["payload"]["rpc_id"], id);
                break;
            }
        }
    }
    first.wait_settled();
    cleanup(handle, &storage, &project);
}

#[test]
fn stale_selection_rejects_mutation_before_changing_the_new_session() {
    let (handle, storage, project) = spawn_serve("selection-fence", TestBehavior::Success);
    let (_, created) = post(handle.addr, TEST_TOKEN, "session.new", "{}");
    created.unwrap();
    let (_, stale) = post(
        handle.addr,
        TEST_TOKEN,
        "permission.set",
        r#"{"mode":"read-only","expected_selection_generation":0}"#,
    );
    assert_eq!(stale.unwrap_err(), ErrorCode::Busy);
    let (_, current) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    assert_ne!(current.unwrap()["permission"]["mode"], "read-only");
    cleanup(handle, &storage, &project);
}

#[test]
fn native_client_checks_host_identity_and_detaches_without_stopping_it() {
    use crate::client_ports::host::HostClient;
    let (handle, storage, project) = spawn_serve("native-host-client", TestBehavior::Success);
    assert!(HostClient::connect(handle.addr.port(), "wrong".into(), &storage).is_err());
    assert!(HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &project).is_err());
    let client = HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage).unwrap();
    assert!(!client.instance_id().is_empty());
    let project_client = client.open_project(&project, false).unwrap();
    let info = project_client.call("workbench.info", &json!({})).unwrap();
    assert_eq!(
        info["project"]["root"],
        project.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let mut events = project_client.events().unwrap();
    let mut subscribed = false;
    for _ in 0..100 {
        let (event, _) = events.next_frame().unwrap();
        if event == "subscribed" {
            subscribed = true;
            break;
        }
    }
    assert!(subscribed);
    drop((events, project_client, client));
    let (_, response) = post(handle.addr, TEST_TOKEN, "host.describe", "{}");
    assert!(response.is_ok(), "detaching must not stop the host");
    handle.shutdown();
    let exit = handle.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn thinking_cycle_rpc_advances_host_state_and_rejects_client_supplied_levels() {
    let (handle, storage, project) = spawn_serve("thinking-cycle", TestBehavior::Success);
    let preset = crate::presets::MODEL_PRESETS[0];
    let (_, response) = post(
        handle.addr,
        TEST_TOKEN,
        "model.preset.select",
        &json!({"id":preset.id}).to_string(),
    );
    response.unwrap();
    let (_, response) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    let before: crate::ThinkingLevel =
        serde_json::from_value(response.unwrap()["model"]["thinking_level"].clone()).unwrap();
    let mut config = crate::ModelConfig::default();
    preset.apply(&mut config);
    let expected = crate::next_thinking_level(config.vendor(), before).unwrap();
    let (_, response) = post(handle.addr, TEST_TOKEN, "model.thinking.cycle", "{}");
    assert_eq!(response.unwrap()["thinking_level"], json!(expected));
    let (_, response) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    assert_eq!(
        response.unwrap()["model"]["thinking_level"],
        json!(expected)
    );
    let (_, response) = post(
        handle.addr,
        TEST_TOKEN,
        "model.thinking.cycle",
        r#"{"thinking_level":"low"}"#,
    );
    assert!(response.is_err());
    let (_, response) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    assert_eq!(
        response.unwrap()["model"]["thinking_level"],
        json!(expected)
    );
    handle.shutdown();
    let exit = handle.join();
    assert!(exit.accept.is_ok());
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn model_rpc_keeps_keys_write_only_and_supports_profile_lifecycle() {
    let (handle, storage, project) = spawn_serve("model-settings", TestBehavior::Success);
    let request = json!({"name":"web", "protocol":"open_ai_compatible",
        "model":"test-model", "endpoint":"https://example.invalid/v1",
        "request_path":"/chat/completions", "api_key":"do-not-return-this-key"});
    for (method, params) in [
        ("model.profile.save", request),
        ("model.profile.activate", json!({"name":"web"})),
        ("model.settings.get", json!({})),
        ("model.profile.get", json!({"name":"web"})),
    ] {
        let (_, response) = post(handle.addr, TEST_TOKEN, method, &params.to_string());
        let value = response.unwrap();
        assert!(!value.to_string().contains("do-not-return-this-key"));
        if method == "model.settings.get" {
            assert_eq!(value["current"]["model"], "test-model");
            assert_eq!(value["current"]["credential_set"], true);
            assert_eq!(value["active_profile"], "web");
        }
    }
    let client =
        crate::host_client::HostClient::connect(handle.addr.port(), TEST_TOKEN.into(), &storage)
            .unwrap();
    let choices = client.model_choices().unwrap();
    assert_eq!(choices.settings.active_profile.as_deref(), Some("web"));
    let (_, profile) = choices
        .profiles
        .iter()
        .find(|(name, _)| name == "web")
        .expect("saved profile must be listed");
    assert_eq!(profile.model, "test-model");
    assert!(profile.credential_set);
    assert!(
        !serde_json::to_string(&choices.settings)
            .unwrap()
            .contains("do-not-return-this-key")
    );
    let (_, response) = post(
        handle.addr,
        TEST_TOKEN,
        "model.profile.delete",
        r#"{"name":"web"}"#,
    );
    assert!(response.is_ok());
    let (_, response) = post(handle.addr, TEST_TOKEN, "model.settings.get", "{}");
    let value = response.unwrap();
    assert_eq!(value["current"]["credential_set"], false);
    assert_ne!(
        value["active_profile"], "web",
        "deleted profile cannot remain active"
    );
    handle.shutdown();
    let exit = handle.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn host_routes_isolate_project_permissions_and_reject_untrusted_mounts() {
    let (handle, storage, project) = spawn_serve("workspace-routing", TestBehavior::Success);
    let other = project.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let (_, description) = post(handle.addr, TEST_TOKEN, "host.describe", "{}");
    assert_eq!(description.unwrap()["protocol_version"], 1);
    let request = json!({"root": other}).to_string();
    let (_, denied) = post(handle.addr, TEST_TOKEN, "workspace.open", &request);
    assert!(
        denied.is_err(),
        "host authentication must not implicitly trust projects"
    );
    let request = json!({"root": other, "trust": true}).to_string();
    let (_, opened) = post(handle.addr, TEST_TOKEN, "workspace.open", &request);
    let opened = opened.unwrap();
    let prefix = opened["api_prefix"].as_str().unwrap();
    let (_, repeated) = post(handle.addr, TEST_TOKEN, "workspace.open", &request);
    assert_eq!(
        opened,
        repeated.unwrap(),
        "one project must have one route/writer"
    );
    let (_, changed) = post(
        handle.addr,
        TEST_TOKEN,
        &format!("{prefix}/api/permission.set"),
        r#"{"mode":"read-only"}"#,
    );
    assert!(changed.is_ok());
    let (_, first) = post(handle.addr, TEST_TOKEN, "workbench.info", "{}");
    let (_, second) = post(
        handle.addr,
        TEST_TOKEN,
        &format!("{prefix}/api/workbench.info"),
        "{}",
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first["permission"]["mode"], second["permission"]["mode"]);
    assert_ne!(first["project"]["root"], second["project"]["root"]);
    assert_eq!(second["permission"]["mode"], "read-only");
    let (status, _) = post(
        handle.addr,
        "wrong-token",
        &format!("{prefix}/api/workbench.info"),
        "{}",
    );
    assert_eq!(status, 401);
    let (status, _) = post(
        handle.addr,
        TEST_TOKEN,
        "/workspace/missing/api/workbench.info",
        "{}",
    );
    assert_eq!(
        status, 404,
        "stale routes cannot fall back to the default project"
    );
    handle.shutdown();
    let exit = handle.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}

#[test]
fn authenticated_host_stop_closes_the_owner_cleanly() {
    let (handle, storage, project) = spawn_serve("host-stop", TestBehavior::Success);
    let (_, stopped) = post(handle.addr, TEST_TOKEN, "host.stop", "{}");
    assert_eq!(stopped.unwrap()["stopping"], true);
    let exit = handle.join();
    assert!(exit.accept.is_ok());
    assert_eq!(exit.close, Some(Ok(())));
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
