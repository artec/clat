use super::*;
use serde_json::{Value, json};
fn rpc(handle: &ServeHandle, method: &str, params: Value) -> Value {
    let (status, result) = post(handle.addr, TEST_TOKEN, method, &params.to_string());
    assert_eq!(status, 200, "{method}: {result:?}");
    result.unwrap()
}
fn seed(handle: &ServeHandle, text: &str) -> String {
    let mut stream = SseClient::connect(handle.addr);
    rpc(handle, "prompt.send", json!({"text":text}));
    stream.wait_settled();
    rpc(handle, "session.info", json!({}))["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn schedule(handle: &ServeHandle, session: &str, at: i64) -> Value {
    let created = rpc(
        handle,
        "schedule.create",
        json!({"session":session,"instruction":"scheduled original-session evidence",
        "trigger":{"kind":"time","rule":{"kind":"at","at_ms":at}},"model_profile":null,
        "budget":{"max_runs":1,"max_tokens":100000,"max_seconds":30,"run_tokens":100000,"run_seconds":30,"max_paid_web_calls":0,"run_paid_web_calls":0}}),
    );
    let r = &created["rule"];
    let identity = json!({"id":r["id"],"revision":r["revision"]});
    let p = rpc(handle, "schedule.review", identity)["review"].clone();
    rpc(
        handle,
        "schedule.confirm",
        json!({"id":r["id"],"revision":r["revision"],"nonce":p["nonce"],"digest":p["digest"]}),
    )["rule"]
        .clone()
}
fn wait_delivery(handle: &ServeHandle, id: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let value = rpc(handle, "schedule.deliveries", json!({}));
        if let Some(done) = value["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["rule_id"] == id && !d["finished_at"].is_null())
        {
            return done.clone();
        }
        assert!(Instant::now() < deadline, "delivery timeout: {value}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn scheduler_host_restart_preserves_rule_runs_origin_and_never_replays_completion() {
    let (handle, storage, root) = spawn_serve("schedule-restart", TestBehavior::Success);
    let origin = seed(&handle, "original session");
    rpc(&handle, "session.new", json!({}));
    let other = seed(&handle, "selected session");
    let rule = schedule(&handle, &origin, super::super::state::now_ms() + 3000);
    handle.shutdown();
    let closed = handle.join();
    assert!(closed.accept.is_ok());
    assert_eq!(closed.close, Some(Ok(())));
    let reopened = super::super::serve_with_with_queue(
        Project::new(&root),
        Some(storage.clone()),
        ServeArgs {
            port: 0,
            token: Some(TEST_TOKEN.into()),
            no_open: true,
            ..Default::default()
        },
        |b| {
            b.with_permission_modes()
                .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Success,
                }))
        },
        Arc::new(AtomicBool::new(false)),
        super::super::state::SUBSCRIBER_QUEUE_FRAMES,
    )
    .unwrap();
    let delivery = wait_delivery(&reopened, rule["id"].as_str().unwrap());
    assert_eq!(delivery["phase"], "completed");
    assert_eq!(delivery["scope"]["session"], origin);
    assert_eq!(delivery["receipt"]["state"], "committed");
    assert!(delivery["actual"]["tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        rpc(&reopened, "session.info", json!({}))["session_id"],
        other
    );
    assert_eq!(
        rpc(&reopened, "schedule.deliveries", json!({}))["deliveries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    cleanup(reopened, &storage, &root);
}
#[test]
fn history_job_schedule_cli_queries_use_mounted_project_without_implicit_mount() {
    let (handle, storage, root) = spawn_serve("query-project-route", TestBehavior::Success);
    let other = root.with_file_name(format!("query-project-other-{}", uuid::Uuid::new_v4()));
    let missing = root.with_file_name(format!("query-project-missing-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(&missing).unwrap();
    rpc(
        &handle,
        "workspace.open",
        json!({"root":other,"trust":true}),
    );
    let client =
        crate::host_client::HostClient::connect(handle.port(), TEST_TOKEN.into(), &storage)
            .unwrap();
    let locale = crate::i18n::LocaleContext::default();
    let routed = client.for_mounted_project(&other, locale).unwrap();
    assert_eq!(
        routed.call("workbench.info", &json!({})).unwrap()["project"]["root"],
        json!(other.canonicalize().unwrap())
    );
    let before = client.call("workspace.list", &json!({})).unwrap();
    assert!(client.for_mounted_project(&missing, locale).is_err());
    assert_eq!(
        client.call("workspace.list", &json!({})).unwrap(),
        before,
        "queries cannot implicitly mount or trust a project"
    );
    drop(routed);
    drop(client);
    cleanup(handle, &storage, &root);
    crate::test_support::cleanup_tree(&other);
    crate::test_support::cleanup_tree(&missing);
}
