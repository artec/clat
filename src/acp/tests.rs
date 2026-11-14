use super::*;
#[cfg(all(unix, feature = "runtime-tests"))]
use crate::ProjectAuthorization;
use crate::{BootstrapApplication, Project};
use serde_json::json;
use std::sync::Arc;

#[test]
fn acp_updates_preserve_message_boundaries_and_reference_only_final_answer() {
    let mut history = Vec::new();
    for (kind, id, text) in [
        ("agent_message_chunk", "m1", "previous"),
        ("agent_thought_chunk", "t", "secret thought"),
        ("agent_message_chunk", "m2", "final "),
        ("agent_message_chunk", "m2", "answer"),
    ] {
        updates::append(
            &mut history,
            json!({
                "sessionUpdate":kind,"messageId":id,"content":{"type":"text","text":text}
            }),
        )
        .unwrap();
    }
    assert_eq!(updates::final_text(&history).unwrap(), "final answer");
    assert_eq!(history.len(), 3);
}

#[test]
fn acp_final_answer_excludes_pre_tool_narration_and_nontext_blocks() {
    let chunks = [
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"I will check."}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"call"}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"resource_link","name":"result","text":"not the answer"}}),
    ];
    assert_eq!(updates::final_text(&chunks).unwrap(), "");
    let mut chunks = Vec::from(chunks);
    chunks.push(
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Finished."}}),
    );
    assert_eq!(updates::final_text(&chunks).unwrap(), "Finished.");
}

#[test]
fn acp_mode_notification_preserves_the_modes_available_at_setup() {
    let mut notification = serde_json::Map::new();
    updates::remember_options(
        &mut notification,
        &json!({"sessionUpdate":"current_mode_update","currentModeId":"edit"}),
    );
    let mut settings =
        json!({"modes":{"currentModeId":"ask","availableModes":[{"id":"ask"},{"id":"edit"}]}});
    updates::apply_options(&mut settings, &notification);
    assert_eq!(settings["modes"]["currentModeId"], "edit");
    assert_eq!(
        settings["modes"]["availableModes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn acp_executable_trust_requires_matching_bytes() {
    let (dir, _) = crate::test_support::roots("acp-trust");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test-agent");
    std::fs::write(&path, b"previous executable").unwrap();
    let edit = AgentEdit {
        label: "Test".into(),
        program: path.clone(),
        args: vec![],
        env: vec![],
    }
    .checked()
    .unwrap();
    let mut agent = Agent {
        id: "one".into(),
        revision: "one".into(),
        executable_fingerprint: state::fingerprint(&edit.program).unwrap(),
        edit,
        trusted: BTreeMap::new(),
    };
    let hash = state::fingerprint(&agent.edit.program).unwrap();
    agent.trusted.insert("project-one".into(), hash);
    assert!(agent.trusted("project-one").is_ok());
    assert!(agent.trusted("project-two").is_err());
    std::fs::write(path, b"modified executable").unwrap();
    assert!(agent.trusted("project-one").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
fn file_read_agent(project: &std::path::Path) -> Value {
    let script = project.join("fixture.sh");
    std::fs::write(&script, br#"
read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r session
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"external"}}'
read -r prompt
printf '%s%s%s\n' '{"jsonrpc":"2.0","id":"read-1","method":"fs/read_text_file","params":{"sessionId":"external","path":"' "$PWD" '/input.txt"}}'
read -r file
case "$file" in *'fixture text'*) ;; *) exit 17;; esac
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"verified"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
while read -r line; do :; done
"#).unwrap();
    json!({"label":"ACP fake","program":"/bin/sh","args":[script]})
}

#[cfg(unix)]
#[test]
fn acp_request_scoped_form_ignores_wrong_request_and_allows_review_edits() {
    use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
    use crate::permission::{PermissionDecision, PermissionRequest};

    struct Answers(std::sync::Mutex<std::collections::VecDeque<AskAnswer>>);
    impl UserAsker for Answers {
        fn ask(&self, question: AskQuestion, _: &CancelToken) -> AskAnswer {
            assert!(question.question.contains("Form Agent"));
            self.0.lock().unwrap().pop_front().unwrap()
        }
    }

    let (storage, project) = crate::test_support::roots("acp-forms");
    std::fs::create_dir_all(&project).unwrap();
    let script = project.join("forms.sh");
    std::fs::write(&script, br#"
read -r initialize
case "$initialize" in *'"elicitation":{"form":{}}'*) ;; *) exit 9;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r session
for request in 3 2; do
  printf '%s%s%s\n' '{"jsonrpc":"2.0","id":"qa","method":"elicitation/create","params":{"requestId":' "$request" ',"mode":"form","message":"Preferred label?","requestedSchema":{"type":"object","properties":{"label":{"type":"string"}}}}}'
  read -r answer
  case "$request,$answer" in 3,*'"code":-32602'*) ;; 2,*'"label":"edited"'*) ;; *) exit 17;; esac
done
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"external"}}'
while read -r line; do :; done
"#).unwrap();
    let answers = Arc::new(Answers(std::sync::Mutex::new(
        [
            AskAnswer::Selected("skip".into()),
            AskAnswer::Selected("edit".into()),
            AskAnswer::Custom("edited".into()),
            AskAnswer::Selected("send".into()),
        ]
        .into(),
    )));
    let ui = || Interaction {
        approver: Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        asker: Some(answers.clone()),
    };
    let mut app = BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount(crate::ProjectAuthorization::grant())
        .unwrap();
    let agent = app
        .acp_client_call(
            "acp.agents.save",
            &json!({"agent":{
                "label":"Form Agent","program":"/bin/sh","args":[script],
            }}),
            ui(),
        )
        .unwrap();
    app.acp_client_call(
        "acp.agents.trust",
        &json!({"id":agent["id"],"revision":agent["revision"],"allow":true}),
        ui(),
    )
    .unwrap();
    let accepted = app
        .acp_client_call(
            "acp.sessions.start",
            &json!({"agent":agent["id"],"requestId":"form-fixture"}),
            ui(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        let status = app
            .acp_client_call("acp.sessions.status", &json!({"id":accepted["id"]}), ui())
            .unwrap();
        if status["phase"] != "starting" {
            assert_eq!(status["phase"], "idle", "{status}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(answers.0.lock().unwrap().is_empty());
    app.close().unwrap();
    let _ = std::fs::remove_dir_all(storage.parent().unwrap());
}

#[cfg(windows)]
fn file_read_agent(project: &std::path::Path) -> Value {
    use base64::Engine;
    let _ = project;
    let script = r#"
$null = [Console]::ReadLine()
[Console]::WriteLine('{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}')
$session = [Console]::ReadLine() | ConvertFrom-Json
[Console]::WriteLine('{"jsonrpc":"2.0","id":2,"result":{"sessionId":"external"}}')
$null = [Console]::ReadLine()
$read = @{jsonrpc='2.0';id='read-1';method='fs/read_text_file';params=@{
    sessionId='external';path=$session.params.cwd.TrimEnd('\')+'\input.txt'
}}
[Console]::WriteLine(($read | ConvertTo-Json -Compress -Depth 5))
$file = [Console]::ReadLine() | ConvertFrom-Json
if ($file.result.content -ne 'fixture text') { exit 17 }
[Console]::WriteLine('{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"external","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"verified"}}}}')
[Console]::WriteLine('{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}')
while ($null -ne [Console]::ReadLine()) {}
"#;
    let command = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let program = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    json!({"label":"ACP fake","program":program,"args":["-NoLogo","-NoProfile","-NonInteractive","-EncodedCommand",command]})
}

#[cfg(any(unix, windows))]
#[test]
fn acp_stdio_delegation_keeps_origin_and_runs_continuation_once() {
    use crate::permission::{PermissionDecision, PermissionRequest};
    use std::time::Duration;
    let (storage, project) = crate::test_support::roots("acp-stdio");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("input.txt"), "fixture text").unwrap();
    let mut app = BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Success,
        }))
        .unwrap();
    crate::test_support::configure_test_model(&app);
    let ui = || Interaction {
        approver: Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        asker: None,
    };
    let agent = app
        .acp_client_call(
            "acp.agents.save",
            &json!({"agent":file_read_agent(&project)}),
            ui(),
        )
        .unwrap();
    app.acp_client_call(
        "acp.agents.trust",
        &json!({"id":agent["id"],"revision":agent["revision"],"allow":true}),
        ui(),
    )
    .unwrap();
    let imported = app.acp_client_call(
        "acp.sessions.start",
        &json!({"agent":agent["id"],"requestId":"foreign-session","remote":"another-project"}),
        ui(),
    );
    assert!(imported.is_err());
    let params = json!({"agent":agent["id"],"requestId":"fixture-request","text":"Run","continueWithClat":true});
    let started = app
        .acp_client_call("acp.delegations.start", &params, ui())
        .unwrap();
    let retry = app
        .acp_client_call("acp.delegations.start", &params, ui())
        .unwrap();
    assert_eq!(started["id"], retry["id"]);
    let wait = std::time::Instant::now() + Duration::from_secs(6);
    let row = loop {
        let row = app
            .acp_client_call("acp.sessions.status", &json!({"id":started["id"]}), ui())
            .unwrap();
        if !matches!(row["phase"].as_str(), Some("starting" | "running")) {
            break row;
        }
        assert!(std::time::Instant::now() < wait, "{row}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(row["phase"], "completed", "{row}");
    assert_eq!(row["result"], "verified");
    assert_eq!(row["updates"].as_array().unwrap().len(), 2);
    assert_eq!(row["handoff"], "pending");
    let original = app.current_session_id().unwrap();
    app.new_session().unwrap();
    let (completion, receiver) = std::sync::mpsc::channel();
    let ports = ui();
    let (handle, target, _) = app
        .acp_client_continue(
            crate::ApplicationRunRequest {
                message: crate::message::PendingMessage::text(""),
                approver: ports.approver,
                asker: None,
                events: Box::new(crate::test_support::SharedEvents(Default::default())),
                completion,
            },
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(target, original.as_str());
    handle.join().unwrap();
    assert!(
        receiver
            .recv_timeout(crate::test_support::HANG)
            .unwrap()
            .is_ok()
    );
    assert!(app.current_session_id().is_none());
    let row = app
        .acp_client_call("acp.delegations.status", &json!({"id":started["id"]}), ui())
        .unwrap();
    assert_eq!(row["handoff"], "admitted");
    app.switch_session(original).unwrap();
    let page = app.session_history(None, 10).unwrap();
    assert!(page.events.iter().any(|event| matches!(event,
        crate::session::replay::ReplayEvent::UserMessage { text,.. } if text.contains("> verified")
    )));
    app.close().unwrap();
    let _ = std::fs::remove_dir_all(storage.parent().unwrap());
}

#[test]
fn acp_unnegotiated_optional_features_fail_before_payload_construction() {
    let json = json!({"agent":"fixture","requestId":"request","systemPrompt":"instructions"});
    let start: Start = serde_json::from_value(json).unwrap();
    assert!(parameters::session_parameters(&json!({}), "/project", &start).is_err());
    let session = parameters::session_parameters(
        &json!({"sessionCapabilities":{"systemPrompt":{}}}),
        "/project",
        &start,
    )
    .unwrap();
    assert_eq!(session["systemPrompt"]["mode"], "append");

    let image = json!({"type":"image","data":"AAAA","mimeType":"image/png"});
    assert!(
        parameters::checked_content(&json!({}), "use this", std::slice::from_ref(&image)).is_err()
    );
    let blocks = parameters::checked_content(
        &json!({"promptCapabilities":{"image":true}}),
        "use this",
        &[image],
    )
    .unwrap();
    assert_eq!(blocks.len(), 2);
}

#[test]
fn acp_continuation_quotes_only_external_final_text_in_original_scope() {
    let start: Start = serde_json::from_value(json!({
        "agent":"agent","requestId":"request","text":"Original instruction",
        "origin":"original-session","delegation":true,"continueWithClat":true
    }))
    .unwrap();
    let record = Record {
        id: "record".into(),
        project: "/test".into(),
        agent: "agent".into(),
        agent_label: "Test Agent".into(),
        revision: "first".into(),
        request: start,
        remote: Some("acp-session".into()),
        phase: Phase::Completed,
        handoff: Handoff::Pending,
        updates: vec![
            json!({"sessionUpdate":"agent_thought_chunk","content":{"text":"never quote this"}}),
        ],
        result: "Line one\nLine two".into(),
        stop: Some("end_turn".into()),
        error: None,
        capabilities: json!({}),
        session_options: json!({}),
        actions: BTreeMap::new(),
    };
    let text = handoff_text(&record);
    assert!(text.contains("Original instruction"));
    assert!(text.contains("untrusted reference material"));
    assert!(text.contains("> Line one\n> Line two"));
    assert!(!text.contains("never quote this"));
}

#[cfg(all(unix, feature = "runtime-tests"))]
#[test]
#[ignore = "requires the installed sibling DSH ACP SDK fixture"]
fn acp_interop_dsh_sdk_fixture() {
    use crate::permission::{PermissionDecision, PermissionRequest};
    let (storage, project) = crate::test_support::roots("acp-real-dsh-sdk");
    std::fs::create_dir_all(&project).unwrap();
    let fixture = std::env::var("CLAT_ACP_DSH_FIXTURE").expect("DSH fixture path");
    let program = std::env::var("CLAT_ACP_DSH_BUN").expect("Bun executable");
    let mut app = BootstrapApplication::open(Project::new(&project), storage.clone())
        .unwrap()
        .authorize_and_mount(ProjectAuthorization::grant())
        .unwrap();
    let ui = || Interaction {
        approver: Arc::new(|_: PermissionRequest, _: &CancelToken| PermissionDecision::Allow),
        asker: None,
    };
    let agent = app
        .acp_client_call(
            "acp.agents.save",
            &json!({"agent":{
                "label":"DSH SDK fixture","program":program,"args":[fixture],
            }}),
            ui(),
        )
        .unwrap();
    app.acp_client_call(
        "acp.agents.trust",
        &json!({"id":agent["id"],"revision":agent["revision"],"allow":true}),
        ui(),
    )
    .unwrap();
    let accepted = app
        .acp_client_call(
            "acp.sessions.start",
            &json!({
                "agent":agent["id"],"requestId":"dsh-sdk","text":"Check interoperability",
            }),
            ui(),
        )
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    let status = loop {
        let status = app
            .acp_client_call("acp.sessions.status", &json!({"id":accepted["id"]}), ui())
            .unwrap();
        if status["phase"] != "starting" && status["phase"] != "running" {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(status["phase"], "completed", "{status}");
    assert_eq!(status["result"], "mock child answer");
    app.close().unwrap();
    let _ = std::fs::remove_dir_all(storage.parent().unwrap());
}
