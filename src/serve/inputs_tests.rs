use super::*;
use serde_json::{Value, json};

fn input_rpc(addr: SocketAddr, method: &str, value: Value) -> Value {
    let (_, reply) = post_rpc_json(addr, TEST_TOKEN, method, &value.to_string());
    assert_eq!(reply["ok"], true, "{method}: {reply}");
    reply["value"].clone()
}
fn submit(addr: SocketAddr, id: &str, text: &str) {
    input_rpc(
        addr,
        "input.submit",
        json!({"text":text,"clientMessageId":id,"operationId":format!("submit-{id}")}),
    );
}
fn pending(addr: SocketAddr) -> Value {
    input_rpc(addr, "input.pending", json!({}))
}
fn allow(addr: SocketAddr, approval: &ParsedSseFrame) {
    input_rpc(
        addr,
        "approval.respond",
        json!({"rpcId":ctl_of(approval)["rpc_id"],"decision":"allow"}),
    );
}

#[test]
fn pending_input_receives_one_per_step_with_images_and_preserves_fifo() {
    let (handle, storage, root) = spawn_serve("inputs-fifo", TestBehavior::RunCommandPerTurn);
    input_rpc(handle.addr, "permission.set", json!({"mode":"read-only"}));
    let mut events = SseClient::connect(handle.addr);
    submit(handle.addr, "initial", "current task");
    let first = events.wait_for("approval.requested", WAIT);
    let draft = open_draft(handle.addr, "input-photo");
    let scope = draft["draftScopeId"].as_str().unwrap();
    let (_, photo) = upload_image(
        handle.addr,
        scope,
        "image/png",
        "photo.png",
        &png_bytes(8, 6),
    );
    let upload = photo["value"]["uploadId"].as_str().unwrap();
    let quote = format!(
        "File snapshot `notes.md` lines 1-1 (version {}; fixed text, not a live path):\nfixed quote",
        "a".repeat(64)
    );
    input_rpc(
        handle.addr,
        "input.submit",
        json!({"text":format!("first queued\n{quote}"), "clientMessageId":"first", "operationId":"submit-first", "draftScopeId":scope,"attachments":[upload],"references":[{"path":"notes.md","version":"a".repeat(64),"quote":quote}]}),
    );
    submit(handle.addr, "second", "second queued");
    assert_eq!(pending(handle.addr)["items"].as_array().unwrap().len(), 2);
    assert_eq!(count_keyed_user_messages(&storage, "first"), 0);
    allow(handle.addr, &first);
    let second = events.wait_for("approval.requested", WAIT);
    let list = pending(handle.addr);
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["items"][0]["id"], "second");
    assert_eq!(count_keyed_user_messages(&storage, "first"), 1);
    assert_eq!(count_keyed_user_messages(&storage, "second"), 0);
    let session = input_rpc(handle.addr, "session.info", json!({}));
    let workbench = input_rpc(handle.addr, "workbench.info", json!({}));
    let history = input_rpc(
        handle.addr,
        "history.conversation",
        json!({"reference":{"project":workbench["project"]["root"],"session":session["session_id"],"seq":null}}),
    );
    let message = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["client_message_id"] == "first")
        .unwrap();
    assert_eq!(message["attachments"].as_array().unwrap().len(), 1);
    let library = input_rpc(handle.addr, "library.list", json!({}));
    assert!(
        library["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "snapshot" && e["path"] == "notes.md")
    );
    allow(handle.addr, &second);
    let third = events.wait_for("approval.requested", WAIT);
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    allow(handle.addr, &third);
    assert_eq!(settled_outcome_type(&events.wait_settled()), "completed");
    assert_eq!(count_keyed_user_messages(&storage, "first"), 1);
    assert_eq!(count_keyed_user_messages(&storage, "second"), 1);
    cleanup(handle, &storage, &root);
}

#[test]
fn pending_input_idle_send_precedes_paused_queue_then_resumes_fifo() {
    let (handle, storage, root) = spawn_serve("inputs-resume", TestBehavior::RunCommandPerTurn);
    input_rpc(handle.addr, "permission.set", json!({"mode":"read-only"}));
    let mut events = SseClient::connect(handle.addr);
    submit(handle.addr, "initial", "current task");
    events.wait_for("approval.requested", WAIT);
    submit(handle.addr, "old", "old queued");
    input_rpc(handle.addr, "run.cancel", json!({}));
    assert_eq!(settled_outcome_type(&events.wait_settled()), "cancelled");
    assert_eq!(pending(handle.addr)["paused"], true);
    submit(handle.addr, "fresh", "fresh draft");
    let fresh = events.wait_for("approval.requested", WAIT);
    assert_eq!(count_keyed_user_messages(&storage, "fresh"), 1);
    assert_eq!(count_keyed_user_messages(&storage, "old"), 0);
    allow(handle.addr, &fresh);
    let old = events.wait_for("approval.requested", WAIT);
    assert_eq!(count_keyed_user_messages(&storage, "old"), 1);
    allow(handle.addr, &old);
    assert_eq!(settled_outcome_type(&events.wait_settled()), "completed");
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    cleanup(handle, &storage, &root);
}

#[test]
fn pending_input_image_edit_preserves_retained_images_and_rejects_foreign_uploads() {
    use base64::Engine as _;
    let gate = Arc::new(SteerGate::default());
    let (handle, storage, root) =
        spawn_serve("inputs-edit-images", TestBehavior::Steer(gate.clone()));
    submit(handle.addr, "initial", "current task");
    gate.wait_entered();
    let source = open_draft(handle.addr, "source");
    let source = source["draftScopeId"].as_str().unwrap();
    let (_, first) = upload_image(
        handle.addr,
        source,
        "image/png",
        "first.png",
        &png_bytes(8, 6),
    );
    let first = first["value"]["uploadId"].as_str().unwrap();
    input_rpc(
        handle.addr,
        "input.submit",
        json!({"text":"pictures","draftScopeId":source,"attachments":[first],"clientMessageId":"pictures","operationId":"pictures"}),
    );
    let added = open_draft(handle.addr, "editor");
    let added = added["draftScopeId"].as_str().unwrap();
    let (_, second) = upload_image(
        handle.addr,
        added,
        "image/png",
        "second.png",
        &png_bytes(20, 12),
    );
    let second = second["value"]["uploadId"].as_str().unwrap();
    let edit = json!({"id":"pictures","version":1,"text":"edited pictures","keepUploads":[first],"draftScopeId":added,"attachments":[second],"operationId":"edit-pictures"});
    input_rpc(handle.addr, "input.update", edit.clone());
    input_rpc(handle.addr, "input.update", edit);
    let list = pending(handle.addr);
    assert_eq!(list["items"][0]["images"].as_array().unwrap().len(), 2);
    for (index, size) in [(0, (8, 6)), (1, (20, 12))] {
        let image = input_rpc(
            handle.addr,
            "input.image",
            json!({"id":"pictures","version":2,"index":index}),
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image["base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(image::load_from_memory(&bytes).unwrap().dimensions(), size);
    }
    let (_, error)=post_rpc_json(handle.addr,TEST_TOKEN,"input.update",&json!({"id":"pictures","version":2,"text":"bad","keepUploads":["foreign"],"operationId":"bad-edit"}).to_string());
    assert_eq!(error["ok"], false);
    assert_eq!(pending(handle.addr)["items"][0]["version"], 2);
    input_rpc(
        handle.addr,
        "input.update",
        json!({"id":"pictures","version":2,"text":"one image","keepUploads":[second],"operationId":"remove-first"}),
    );
    assert_eq!(
        pending(handle.addr)["items"][0]["images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let deletion = json!({"id":"pictures","version":3,"operationId":"delete-pictures"});
    input_rpc(handle.addr, "input.delete", deletion.clone());
    input_rpc(handle.addr, "input.delete", deletion);
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    input_rpc(handle.addr, "run.cancel", json!({}));
    gate.release();
    cleanup(handle, &storage, &root);
}

struct AskThenComplete;
impl crate::test_support::TestModelScript for AskThenComplete {
    fn stream(
        &self,
        request: crate::model::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::model::ModelResponse, crate::model::ModelError> {
        if request.items.iter().any(|item| matches!(item,crate::model::ModelItem::User{content} if content.iter().any(|p|matches!(p,crate::model::ContentPart::Text(text) if text=="urgent")))) {
            return Ok(crate::test_support::response("continued",crate::model::FinishReason::Completed));
        }
        let mut response = crate::test_support::response("", crate::model::FinishReason::ToolCalls);
        response.tool_calls.push(crate::ToolCall {
            id: "ask-input".into(),
            name: "ask_user".into(),
            arguments: json!({"question":"choose","options":[{"label":"continue"}]}),
        });
        Ok(response)
    }
}

#[test]
fn pending_input_interrupt_cancels_an_active_tool_before_continuation() {
    let (handle, storage, root) = spawn_serve(
        "inputs-tool-interrupt",
        TestBehavior::Scripted(Arc::new(AskThenComplete)),
    );
    let mut events = SseClient::connect(handle.addr);
    submit(handle.addr, "initial", "current task");
    let deadline = Instant::now() + WAIT;
    loop {
        let frame = events.wait_for("notice", deadline.saturating_duration_since(Instant::now()));
        if ctl_of(&frame)["kind"] == "question_requested" {
            break;
        }
        assert!(Instant::now() < deadline, "ask tool did not start");
    }
    submit(handle.addr, "urgent", "urgent");
    input_rpc(
        handle.addr,
        "input.interrupt",
        json!({"id":"urgent","version":1,"operationId":"tool-interrupt"}),
    );
    assert_eq!(settled_outcome_type(&events.wait_settled()), "cancelled");
    assert_eq!(settled_outcome_type(&events.wait_settled()), "completed");
    assert_eq!(count_keyed_user_messages(&storage, "urgent"), 1);
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    cleanup(handle, &storage, &root);
}

#[test]
fn pending_input_interrupt_resumes_same_session_once_and_keeps_other_messages() {
    let gate = Arc::new(SteerGate::default());
    let (handle, storage, root) =
        spawn_serve("inputs-interrupt", TestBehavior::Steer(gate.clone()));
    let mut events = SseClient::connect(handle.addr);
    submit(handle.addr, "initial", "current task");
    gate.wait_entered();
    let session = input_rpc(handle.addr, "session.info", json!({}));
    for (id, text) in [
        ("first", "first queued"),
        ("urgent", "also run the tests"),
        ("last", "last queued"),
    ] {
        if id == "urgent" {
            let draft = open_draft(handle.addr, "urgent-photo");
            let scope = draft["draftScopeId"].as_str().unwrap();
            let (_, image) = upload_image(
                handle.addr,
                scope,
                "image/png",
                "urgent.png",
                &png_bytes(8, 6),
            );
            input_rpc(
                handle.addr,
                "input.submit",
                json!({"text":text,"clientMessageId":id,"operationId":"submit-urgent","draftScopeId":scope,"attachments":[image["value"]["uploadId"]]}),
            );
        } else {
            submit(handle.addr, id, text);
        }
    }
    let request = json!({"id":"urgent","version":1,"operationId":"interrupt-once"});
    let first = input_rpc(handle.addr, "input.interrupt", request.clone());
    assert_eq!(input_rpc(handle.addr, "input.interrupt", request), first);
    assert_eq!(settled_outcome_type(&events.wait_settled()), "cancelled");
    assert_eq!(settled_outcome_type(&events.wait_settled()), "completed");
    assert!(gate.saw_steering.load(std::sync::atomic::Ordering::Acquire));
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    let after = input_rpc(handle.addr, "session.info", json!({}));
    assert!(session["session_id"].is_string());
    assert_eq!(session["session_id"], after["session_id"]);
    let workbench = input_rpc(handle.addr, "workbench.info", json!({}));
    let history = input_rpc(
        handle.addr,
        "history.conversation",
        json!({"reference":{"project":workbench["project"]["root"],"session":after["session_id"],"seq":null}}),
    );
    let urgent = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["client_message_id"] == "urgent")
        .unwrap();
    assert_eq!(urgent["attachments"].as_array().unwrap().len(), 1);
    for id in ["initial", "urgent", "first", "last"] {
        assert_eq!(count_keyed_user_messages(&storage, id), 1);
    }
    cleanup(handle, &storage, &root);
}

#[test]
fn pending_input_recall_transfers_images_and_rejects_capacity_and_stale_edits() {
    let gate = Arc::new(SteerGate::default());
    let (handle, storage, root) = spawn_serve("inputs-recall", TestBehavior::Steer(gate.clone()));
    let mut events = SseClient::connect(handle.addr);
    let selection =
        input_rpc(handle.addr, "workbench.info", json!({}))["selection_generation"].clone();
    submit(handle.addr, "initial", "current task");
    gate.wait_entered();
    let source = open_draft(handle.addr, "source");
    let source = source["draftScopeId"].as_str().unwrap();
    let (_, photo) = upload_image(
        handle.addr,
        source,
        "image/png",
        "photo.png",
        &png_bytes(8, 6),
    );
    let upload = photo["value"]["uploadId"].as_str().unwrap();
    const QUOTE: &str = "File snapshot `notes.md` lines 1-1 (version aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; fixed text, not a live path):\nfixed quote";
    let reference = json!({"path":"notes.md","version":"a".repeat(64),"quote":QUOTE});
    input_rpc(
        handle.addr,
        "input.submit",
        json!({"text":QUOTE,"attachments":[upload],"draftScopeId":source,"references":[reference],"clientMessageId":"photo","operationId":"submit-photo"}),
    );
    input_rpc(
        handle.addr,
        "input.update",
        json!({"id":"photo","version":1,"text":format!("edited {QUOTE}"),"operationId":"edit-photo"}),
    );
    let (_, stale) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "input.delete",
        &json!({"id":"photo","version":1,"operationId":"stale"}).to_string(),
    );
    assert_eq!(stale["ok"], false);
    let full = open_draft(handle.addr, "full");
    let full = full["draftScopeId"].as_str().unwrap();
    for _ in 0..8 {
        assert_eq!(
            upload_image(
                handle.addr,
                full,
                "image/png",
                "existing.png",
                &png_bytes(8, 6)
            )
            .0,
            200
        );
    }
    let request = json!({"id":"photo","version":2,"draftScopeId":full,"operationId":"recall-full"});
    let (_, refused) = post_rpc_json(
        handle.addr,
        TEST_TOKEN,
        "input.recall",
        &request.to_string(),
    );
    assert_eq!(refused["ok"], false);
    assert_eq!(pending(handle.addr)["items"][0]["version"], 2);
    // Removed composer images may remain staged; only the current selection counts.
    let request = json!({"id":"photo","version":2,"draftScopeId":full,"draftUploads":[],"operationId":"recall-photo","expected_selection_generation":selection});
    let recalled = input_rpc(handle.addr, "input.recall", request.clone());
    assert_eq!(recalled["item"]["text"], format!("edited {QUOTE}"));
    assert_eq!(recalled["item"]["draft"]["references"][0], reference);
    assert_eq!(
        input_rpc(handle.addr, "input.recall", request.clone()),
        recalled
    );
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    assert_eq!(count_keyed_user_messages(&storage, "photo"), 0);
    input_rpc(handle.addr, "run.cancel", json!({}));
    gate.release();
    events.wait_settled();
    input_rpc(handle.addr, "session.new", json!({}));
    assert_eq!(input_rpc(handle.addr, "input.recall", request), recalled);
    let (_, rejected) = post_rpc_json(handle.addr, TEST_TOKEN, "input.delete", &json!({"id":"photo","version":2,"operationId":"unknown-old-request","expected_selection_generation":selection}).to_string());
    assert_eq!(rejected["ok"], false);
    assert!(pending(handle.addr)["items"].as_array().unwrap().is_empty());
    cleanup(handle, &storage, &root);
}

struct NonCooperativeModel {
    entered: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    calls: std::sync::atomic::AtomicUsize,
}
impl crate::test_support::TestModelScript for NonCooperativeModel {
    fn stream(
        &self,
        _: crate::model::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::model::ModelResponse, crate::model::ModelError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv_timeout(WAIT).unwrap();
        }
        Ok(crate::test_support::response(
            "done",
            crate::model::FinishReason::Completed,
        ))
    }
}

#[test]
fn pending_input_esc_cancels_handoff_without_starting_an_overlapping_worker() {
    let (entered, observed) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let script = Arc::new(NonCooperativeModel {
        entered,
        release: Mutex::new(released),
        calls: Default::default(),
    });
    let (handle, storage, root) = spawn_serve(
        "inputs-stop-handoff",
        TestBehavior::Scripted(script.clone()),
    );
    let mut events = SseClient::connect(handle.addr);
    submit(handle.addr, "initial", "current task");
    observed.recv_timeout(WAIT).unwrap();
    submit(handle.addr, "urgent", "stop then resume");
    input_rpc(
        handle.addr,
        "input.interrupt",
        json!({"id":"urgent","version":1,"operationId":"interrupt"}),
    );
    assert_eq!(script.calls.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(pending(handle.addr)["interrupting"], true);
    let session = input_rpc(handle.addr, "session.info", json!({}));
    input_rpc(
        handle.addr,
        "run.cancel",
        json!({"session_id":session["session_id"]}),
    );
    assert_eq!(pending(handle.addr)["interrupting"], false);
    release.send(()).unwrap();
    assert_eq!(settled_outcome_type(&events.wait_settled()), "cancelled");
    assert_eq!(script.calls.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(count_keyed_user_messages(&storage, "urgent"), 0);
    assert_eq!(pending(handle.addr)["paused"], true);
    cleanup(handle, &storage, &root);
}
