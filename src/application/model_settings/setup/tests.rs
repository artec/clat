use super::*;
use serde_json::json;

fn custom(revision: &str) -> Value {
    json!({"operation_id":uuid::Uuid::new_v4().to_string(),"expected_revision":revision,
        "profile":{"name":"Local", "protocol":"open_ai_compatible", "model":"local-test",
        "endpoint":"http://127.0.0.1:11434/v1", "request_path":"/chat/completions"}})
}
#[test]
fn setup_configuration_and_activation_are_shared_without_creating_a_session() {
    let (root, mut host, first, second) = crate::application::host_tests::fixture("setup-config");
    let first = host.attach(first).unwrap();
    let second = host
        .authorize_and_attach(second, crate::application::ProjectAuthorization::grant())
        .unwrap();
    let scope;
    {
        let app = first.lock().unwrap();
        let view = app.model_setup_view().unwrap();
        assert_eq!(view.state, "needs_model");
        scope = view.scope;
        let before = app.current_session_id();
        let req = custom(&view.revision);
        let op = app.model_setup_start(req.clone(), true).unwrap();
        assert!(op.saved);
        assert_eq!(op.state, "saved");
        assert_eq!(
            app.model_setup_start(req.clone(), true).unwrap().revision,
            op.revision
        );
        assert!(app.model_setup_start(custom(&view.revision), true).is_err());
        let mut reused = req;
        reused["verify"] = json!(true);
        assert!(app.model_setup_start(reused, true).is_err());
        assert_eq!(app.current_session_id(), before);
        let disk = crate::control_storage::ControlStorage::open_ready(&root.join("state")).unwrap();
        assert_eq!(disk.active_profile().unwrap().as_deref(), Some("Local"));
        assert_eq!(
            disk.load_model_state().unwrap().unwrap().0.model,
            "local-test"
        );
    }
    {
        let app = second.lock().unwrap();
        let view = app.model_setup_view().unwrap();
        assert_eq!(view.scope, scope);
        assert_eq!(view.state, "configured");
        assert!(!view.settings.current.credential_set);
        assert_eq!(view.settings.active_profile.as_deref(), Some("Local"));
    }
    drop(first);
    drop(second);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn setup_missing_key_and_failed_save_leave_the_previous_route_intact() {
    let (root, mut host, first, _) = crate::application::host_tests::fixture("setup-failure");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        app.save_model_state(
            &ModelConfig::default(),
            &ProviderCredentials::for_protocol(crate::ModelProtocol::OpenAiCompatible),
        )
        .unwrap();
        let view = app.model_setup_view().unwrap();
        let result = app
            .model_setup_start(
                json!({"operation_id":uuid::Uuid::new_v4().to_string(),
            "expected_revision":view.revision,"preset":"deepseek-flash"}),
                true,
            )
            .unwrap();
        assert!(!result.saved);
        assert_eq!(app.model_setup_view().unwrap().revision, view.revision);
        // A real atomic-file failure must roll back both profile and active route.
        let path = root.join("state/settings.json");
        let backup = path.with_extension("before-setup");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        let failed = app.model_setup_start(custom(&view.revision), true).unwrap();
        assert!(!failed.saved);
        assert_eq!(app.model_setup_view().unwrap().state, "needs_model");
        assert!(app.model_profile_view("Local").unwrap().is_none());
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(backup, path).unwrap();
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn setup_validation_receipts_become_stale_after_another_client_changes_the_model() {
    let (root, mut host, first, _) = crate::application::host_tests::fixture("setup-verify");
    let project = host.attach(first).unwrap();
    let id;
    {
        let app = project.lock().unwrap();
        let mut req = custom(&app.model_setup_view().unwrap().revision);
        req["verify"] = json!(true);
        id = req["operation_id"].as_str().unwrap().to_owned();
        let result = app.model_setup_start(req.clone(), true).unwrap();
        assert!(result.saved);
        for _ in 0..100 {
            if app.model_setup_status(&id).unwrap().state != "checking" {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(app.model_setup_status(&id).unwrap().state, "verified");
        let changed = app
            .model_setup_start(custom(&result.revision), true)
            .unwrap();
        assert!(changed.saved);
        assert_eq!(app.model_setup_status(&id).unwrap().state, "stale");
        assert_eq!(app.model_setup_start(req, true).unwrap().operation_id, id);
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

struct InspectModel {
    error: Option<crate::model::ModelErrorKind>,
    empty: bool,
    calls: usize,
}

#[test]
fn setup_concurrent_clients_cannot_overwrite_the_same_configuration_version() {
    let (root, mut host, first, second) = crate::application::host_tests::fixture("setup-race");
    let a = host.attach(first).unwrap();
    let b = host
        .authorize_and_attach(second, crate::application::ProjectAuthorization::grant())
        .unwrap();
    let revision = a.lock().unwrap().model_setup_view().unwrap().revision;
    let mut workers = Vec::new();
    let mut starts = Vec::new();
    for (app, name) in [(a.clone(), "first"), (b.clone(), "second")] {
        let (start, ready) = std::sync::mpsc::channel();
        starts.push(start);
        let mut request = custom(&revision);
        request["profile"]["name"] = json!(name);
        workers.push(std::thread::spawn(move || {
            ready
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            app.lock().unwrap().model_setup_start(request, true)
        }));
    }
    for start in starts {
        start.send(()).unwrap();
    }
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().is_ok_and(|op| op.saved))
            .count(),
        1
    );
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    {
        let app = a.lock().unwrap();
        let view = app.model_setup_view().unwrap();
        assert_eq!(view.settings.profiles.len(), 1);
        assert_eq!(
            view.settings.active_profile,
            Some(view.settings.profiles[0].clone())
        );
        assert!(app.current_session_id().is_none());
    }
    drop(a);
    drop(b);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
impl crate::Model for InspectModel {
    fn provider(&self) -> &str {
        "test"
    }
    fn model_id(&self) -> &str {
        "test"
    }
    fn stream(
        &mut self,
        req: crate::model::ModelRequest<'_>,
        _: &mut dyn crate::model::ModelEventSink,
    ) -> Result<crate::model::ModelResponse, crate::ModelError> {
        self.calls += 1;
        assert!(req.tools.is_empty());
        assert!(req.instructions.is_none());
        assert_eq!(req.items.len(), 1);
        assert_eq!(
            format!("{:?}", req.items),
            format!(
                "{:?}",
                [crate::model::ModelItem::user_text(
                    "Reply with OK. This is a connection check."
                )]
            )
        );
        if let Some(kind) = self.error {
            return Err(crate::ModelError::with_kind(kind, "secret-provider-body"));
        }
        Ok(crate::model::ModelResponse {
            text: if self.empty { "" } else { "OK" }.into(),
            tool_calls: vec![],
            finish_reason: crate::model::FinishReason::Completed,
            usage: None,
            provider_response_id: None,
            provider_state: vec![],
            reasoning: None,
        })
    }
}
#[test]
fn setup_connection_check_is_tool_free_single_attempt_and_never_echoes_errors() {
    use crate::model::ModelErrorKind as K;
    let cancel = CancelToken::new();
    for (kind, code) in [
        (K::Authentication, "setup.authentication"),
        (K::RateLimited, "setup.rate_limited"),
        (K::Transport, "setup.network"),
        (K::Decode, "setup.protocol_error"),
        (K::Client, "setup.request_rejected"),
    ] {
        let mut model = InspectModel {
            error: Some(kind),
            empty: false,
            calls: 0,
        };
        assert_eq!(
            worker::check(&mut model, &ModelConfig::default(), &cancel),
            Err(code)
        );
        assert_eq!(model.calls, 1);
    }
    let mut model = InspectModel {
        error: None,
        empty: true,
        calls: 0,
    };
    assert_eq!(
        worker::check(&mut model, &ModelConfig::default(), &cancel),
        Err("setup.empty_response")
    );
    cancel.cancel();
    assert_eq!(
        worker::check(&mut model, &ModelConfig::default(), &cancel),
        Err("setup.cancelled")
    );
    assert_eq!(model.calls, 1);
}

#[test]
fn setup_cold_reopen_preserves_configuration_but_not_check_receipts() {
    let (root, mut host, first, _) = crate::application::host_tests::fixture("setup-reopen");
    let project = host.attach(first.clone()).unwrap();
    let (id, revision, scope);
    {
        let app = project.lock().unwrap();
        let view = app.model_setup_view().unwrap();
        let req = custom(&view.revision);
        id = req["operation_id"].as_str().unwrap().to_owned();
        let result = app.model_setup_start(req, true).unwrap();
        revision = result.revision;
        scope = view.scope;
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    let app = crate::application::BootstrapApplication::open(first, root.join("state"))
        .unwrap()
        .authorize_and_mount_with_provider(Arc::new(crate::test_support::TestProviderPlugin {
            behavior: crate::test_support::TestBehavior::Success,
        }))
        .unwrap();
    let view = app.model_setup_view().unwrap();
    assert_eq!(view.state, "configured");
    assert_eq!(view.scope, scope);
    assert_ne!(view.revision, revision);
    assert!(app.model_setup_status(&id).is_err());
    assert!(app.current_session_id().is_none());
    drop(app);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn setup_cancellation_wins_over_late_success_and_secrets_never_enter_views() {
    let (root, mut host, first, _) = crate::application::host_tests::fixture("setup-secrets");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let view = app.model_setup_view().unwrap();
        let result = app.model_setup_start(json!({"operation_id":uuid::Uuid::new_v4().to_string(),
            "expected_revision":view.revision,"preset":"deepseek-flash","api_key":"private-fixture-key"}), true).unwrap();
        let op = app.setup_operation(&result.operation_id).unwrap();
        op.status.lock().unwrap().state = "checking".into();
        app.model_setup_cancel(&result.operation_id).unwrap();
        worker::finish(&op, "verified", None);
        assert_eq!(
            app.model_setup_status(&result.operation_id).unwrap().state,
            "cancelled"
        );
        let output = serde_json::to_string(&app.model_setup_view().unwrap()).unwrap();
        assert!(!output.contains("private-fixture-key"));
        assert!(app.current_session_id().is_none());
        assert!(
            app.model_setup_start(
                json!({"operation_id":uuid::Uuid::new_v4().to_string(),
            "expected_revision":view.revision}),
                false
            )
            .is_err()
        );
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}
