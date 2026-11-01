//! Opt-in private-copy exercise. No source data, credentials or fixtures enter the repository.
use super::tests::{mount, mount_result, run};
use super::*;
use crate::test_support::{TestBehavior, configure_test_model_with_budget};
#[test]
#[ignore = "requires an owner-authorized isolated copy under output/verification/plugin-phase8"]
fn phase8_private_history_copies_continue_compact_cancel_and_reopen() {
    let Some(storage) = std::env::var_os("CLAT_PHASE8_HISTORY_COPY") else {
        // Owner-authorized private assets are never part of ordinary CI inputs.
        return;
    };
    let storage = std::path::PathBuf::from(storage);
    let project = Project::new(env!("CARGO_MANIFEST_DIR"))
        .root()
        .ancestors()
        .find(|path| path.join("AGENTS.md").exists())
        .map(Project::new)
        .expect("repository root");
    assert!(
        storage.starts_with(project.root().join("output/verification/plugin-phase8")),
        "private assets must be isolated inside ignored output"
    );
    let backend = crate::session::persistence::JsonlBackend::new(
        storage.join("sessions"),
        crate::session::persistence::JsonlCompression::Zstd,
        false,
    );
    let headers = backend.list_headers().unwrap();
    assert_eq!(headers.len(), 3);
    // Static configuration lives solely in this isolated copy.
    std::fs::write(
        storage.join("builtin-plugins.json"),
        serde_json::to_vec(
            &serde_json::json!({"unit":{"name":"builtin-plugins","version":1},"disabled":[
                "builtin.mcp_adapter",
                "builtin.wasm_adapter",
                "builtin.utility_model",
                "builtin.project_instructions",
                "builtin.scheduler",
                "builtin.jobs",
                "builtin.development",
            ]}),
        )
        .unwrap(),
    )
    .unwrap();
    for header in headers {
        let mut app = mount(&project, &storage, TestBehavior::Success);
        configure_test_model_with_budget(&app, u32::MAX);
        app.switch_session(header.id.clone()).unwrap();
        run(
            &mut app,
            "Continue the existing task using the retained facts.",
        )
        .unwrap();
        app.compact_session()
            .unwrap()
            .join_report()
            .unwrap()
            .unwrap();
        let after = app.sessions.surface_nodes().unwrap();
        assert!(!after.is_empty());
        app.close().unwrap();
        let mut app = mount(&project, &storage, TestBehavior::Cancel);
        app.switch_session(header.id.clone()).unwrap();
        let (completion, receiver) = mpsc::channel();
        let request = ApplicationRunRequest {
            message: crate::message::PendingMessage::text("Continue pending work."),
            approver: Arc::new(|_: crate::PermissionRequest, _: &crate::CancelToken| {
                crate::PermissionDecision::Deny {
                    reason: "offline private-copy verification".into(),
                }
            }),
            asker: None,
            events: Box::new(Vec::new()),
            completion,
        };
        let handle = app.start_run(request).unwrap();
        let _ = app.steer(crate::message::PendingMessage::text(
            "Preserve pending work; do not claim unverified success.",
        ));
        handle.cancel();
        handle.join().unwrap();
        let _ = receiver.try_recv().unwrap();
        app.close().unwrap();
        let mut app = crate::test_support::reopen_after_close(|| {
            mount_result(&project, &storage, TestBehavior::Success)
        });
        app.switch_session(header.id).unwrap();
        run(
            &mut app,
            "Continue after reopening; preserve the checkpoint and durable restrictions.",
        )
        .unwrap();
        app.close().unwrap();
    }
}
