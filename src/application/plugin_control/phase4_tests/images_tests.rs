use super::super::phase2_tests::request;
use super::*;
use crate::message::PendingMessage;
#[test]
fn disabled_new_images_cannot_materialize_session_or_mutate_queue_or_model() {
    let (root, _) = crate::test_support::roots("phase4-image-admission");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    policy(&state, &["builtin.draft_images"]);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let model = app.model_state().unwrap().0;
        let before = app.current_session_id();
        for message in image_shapes(&root) {
            let (mut request, _) = request();
            request.message = message.clone();
            let error = app.start_run(request).err().expect("image rejected");
            assert_eq!(error.presentation().key, "extension.disabled_builtin");
            assert_eq!(app.current_session_id(), before);
            assert!(app.pending_inputs().items.is_empty());
            assert!(
                app.enqueue_input("queue-image".into(), message.clone(), Default::default())
                    .is_err()
            );
            assert!(matches!(
                app.steer(message.clone()),
                SteerOutcome::Refused { .. }
            ));
        }
        assert!(app.start_vision_probe().is_err());
        assert_eq!(app.model_state().unwrap().0, model);
        assert_eq!(app.current_session_id(), before);
        assert!(!state.join("drafts").exists());
        run(&mut app);
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        let facts = app.workflow_details().unwrap();
        let (mut request, _) = request();
        request.message = image_shapes(&root).pop().unwrap();
        assert!(app.start_run(request).is_err());
        assert_eq!(app.workflow_details().unwrap(), facts);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_image_input_preserves_historical_context_and_native_view_image() {
    let (root, _) = crate::test_support::roots("phase4-history-context");
    let project = Project::new(root.join("project"));
    std::fs::create_dir_all(project.root()).unwrap();
    let state = root.join("state");
    let source = project.root().join("source.png");
    std::fs::write(&source, crate::test_support::png_bytes(8, 6, [4, 5, 6])).unwrap();
    let mut host = mount(&project, &state);
    let attached = host.attach(project.clone()).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        let (mut request, receiver) = request();
        request.message =
            PendingMessage::from_front_end("retain this image", None, vec![source.clone()]);
        let handle = app.start_run(request).unwrap();
        handle.join().unwrap();
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    policy(&state, &FEATURES);
    let mut host = mount(&project, &state);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
        run(&mut app);
        assert_eq!(app.context_snapshot().unwrap().image_count, 1);
        app.view_image.begin_run();
        let result = app
            .tools
            .get("view_image")
            .unwrap()
            .invoke(
                &serde_json::json!({"project_relative_path":"source.png"}),
                app.project(),
                &crate::CancelToken::new(),
            )
            .unwrap();
        assert_eq!(result["viewed"], true);
        app.view_image.clear();
        assert!(source.exists());
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

fn image_shapes(root: &std::path::Path) -> Vec<PendingMessage> {
    let raw = PendingMessage::from_front_end(
        "keep original text",
        Some("image-attempt".into()),
        vec![root.join("unreadable-image.png")],
    );
    let mut resolved = raw.clone();
    resolved.resolve_staged_attachments(vec![root.join("already-prepared.png")]);
    let descriptor=serde_json::from_value::<crate::message::AttachmentDescriptor>(serde_json::json!({"attachment_id":"forged-image","media_type":"image/png","width":1,"height":1,"bytes":1})).unwrap();
    let mut direct = PendingMessage::text("text and direct image");
    direct
        .content
        .blocks
        .push(crate::message::ContentBlock::Image {
            attachment: descriptor.clone(),
        });
    let mut prepared = direct.clone();
    prepared.admitted_images.push(crate::message::JournalImage {
        descriptor,
        path: root
            .join("prepared-but-forbidden.png")
            .to_string_lossy()
            .into_owned(),
    });
    vec![raw, resolved, direct, prepared]
}

#[test]
fn rejected_images_preserve_running_text_turn_and_explicit_cancellation() {
    for cancel in [false, true] {
        let (root, _) = crate::test_support::roots("phase4-running-rejection");
        let project = Project::new(root.join("project"));
        std::fs::create_dir_all(project.root()).unwrap();
        let state = root.join("state");
        policy(&state, &FEATURES);
        let gate = std::sync::Arc::new(crate::test_support::SteerGate::default());
        let mut app = BootstrapApplication::open(project, state)
            .unwrap()
            .with_permission_modes()
            .authorize_and_mount_with_provider(std::sync::Arc::new(
                crate::test_support::TestProviderPlugin {
                    behavior: crate::test_support::TestBehavior::Steer(gate.clone()),
                },
            ))
            .unwrap();
        crate::test_support::configure_test_model(&app);
        app.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        let (initial, receiver) = request();
        let handle = app.start_run(initial).unwrap();
        gate.wait_entered();
        let facts = app.workflow_details().unwrap();
        for message in image_shapes(&root) {
            let (mut attempt, _) = request();
            attempt.message = message.clone();
            assert_eq!(
                app.start_run(attempt).err().unwrap().presentation().key,
                "extension.disabled_builtin"
            );
            assert!(matches!(
                app.steer(message.clone()),
                SteerOutcome::Refused { .. }
            ));
            assert!(
                app.enqueue_input("blocked".into(), message, Default::default())
                    .is_err()
            );
        }
        assert!(app.pending_inputs().items.is_empty());
        assert_eq!(app.workflow_details().unwrap()["version"], facts["version"]);
        if cancel {
            app.cancel_active_run();
        }
        gate.release();
        handle.join().unwrap();
        let done = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(done.cancelled, cancel);
        app.close().unwrap();
        crate::test_support::cleanup_tree(&root);
    }
}
