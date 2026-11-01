use super::phase2_tests::{mount, policy, run};
use crate::Project;

#[test]
fn core_inventory_explains_every_protected_owner_and_leaves_contributions_optional() {
    let (storage, root) = crate::test_support::roots("phase6-inventory");
    std::fs::create_dir_all(&root).unwrap();
    let mut host = mount(&Project::new(&root), &storage);
    let value = host.plugin_list().unwrap();
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
    for plugin in value["builtins"].as_array().unwrap() {
        for instance in plugin["instances"].as_array().unwrap() {
            let roots = instance["required_by"]
                .as_array()
                .expect("every owner must expose its core protection sources");
            assert_eq!(
                instance["configurable"],
                roots.is_empty(),
                "{}",
                plugin["id"]
            );
            let ids: Vec<_> = roots
                .iter()
                .map(|r| r["service_id"].as_str().unwrap())
                .collect();
            assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
            for root in roots {
                let message: crate::i18n::UiMessage =
                    serde_json::from_value(root["reason_presentation"].clone()).unwrap();
                for locale in [
                    crate::i18n::Locale::English,
                    crate::i18n::Locale::Simplified,
                    crate::i18n::Locale::Traditional,
                ] {
                    assert_ne!(
                        crate::i18n::LocaleContext::new(locale).render(&message),
                        message.key
                    );
                }
            }
        }
    }
}

fn optional_ids(value: &serde_json::Value) -> Vec<String> {
    value["builtins"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["id"] != "test.application_provider"
                && row["instances"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|i| i["configurable"] == true)
        })
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn minimal_core_host_keeps_chat_sessions_and_process_lifecycle_without_exec_tools() {
    let (storage, root) = crate::test_support::roots("phase6-minimal");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    let mut host = mount(&project, &storage);
    let disabled = optional_ids(&host.plugin_list().unwrap());
    host.close().unwrap();
    drop(host);
    policy(
        &storage,
        &disabled.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let mut host = mount(&project, &storage);
    let attached = host.attach(project).unwrap();
    {
        let mut app = attached.lock().unwrap();
        crate::test_support::configure_test_model(&app);
        assert!(!app.builtin_available("builtin.exec_tools"));
        assert!(!app.builtin_available("builtin.native_read"));
        assert!(!app.builtin_available("builtin.default_prompt"));
        assert!(app.builtin_available("builtin.process"));
        assert!(app.builtin_available("builtin.sandbox.policy"));
        run(&mut app);
        let first = app.current_session_id().unwrap();
        let cancel = crate::CancelToken::new();
        let generation = app
            .process_service
            .bind_run(first.as_str(), cancel.clone())
            .unwrap();
        let reader = app.task_reader().unwrap();
        assert_eq!(reader.list()["state"], "available");
        cancel.cancel();
        app.process_service.unbind_run(generation).unwrap();
        assert_eq!(reader.list()["state"], "unavailable");
        app.new_session().unwrap();
        run(&mut app);
        assert_ne!(app.current_session_id().unwrap(), first);
        app.switch_session(first.clone()).unwrap();
        assert_eq!(app.current_session_id(), Some(first));
        let inventory = app.composition.inventory(app.project().root()).unwrap();
        let rows = serde_json::to_value(inventory).unwrap();
        for row in rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["id"] != "test.application_provider")
        {
            let instance = &row["instances"][0];
            assert_eq!(
                instance["phase"] == "active",
                !instance["required_by"].as_array().unwrap().is_empty()
            );
            assert!(instance["tools"].as_array().unwrap().is_empty());
        }
    }
    drop(attached);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}

#[test]
fn zero_model_adapters_keep_host_configuration_and_session_control_without_dispatch() {
    let (storage, root) = crate::test_support::roots("phase6-no-models");
    std::fs::create_dir_all(&root).unwrap();
    let project = Project::new(&root);
    policy(
        &storage,
        &[
            "builtin.openai_responses",
            "builtin.openai_compatible",
            "builtin.utility_model",
        ],
    );
    let mut app = crate::BootstrapApplication::open(project, storage.clone())
        .unwrap()
        .authorize_and_mount(crate::ProjectAuthorization::grant())
        .unwrap();
    crate::test_support::configure_test_model(&app);
    let (mut config, credentials) = app.model_state().unwrap();
    assert!(app.providers.descriptors(&credentials).is_empty());
    config.model = "saved-without-adapters".into();
    app.save_model_state(&config, &credentials).unwrap();
    assert_eq!(app.model_state().unwrap().0.model, config.model);
    assert!(app.list_model_profiles().is_ok());
    app.new_session().unwrap();
    let (request, receive) = super::phase2_tests::request();
    let failure = match app.start_run(request) {
        Ok(worker) => {
            let result = receive
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            worker.join().unwrap();
            result
                .expect_err("missing adapters cannot dispatch a model")
                .error
        }
        Err(error) => error.to_string(),
    };
    assert!(failure.contains("no provider for"), "{failure}");
    assert!(app.workbench_snapshot().is_ok());
    app.close().unwrap();
    crate::test_support::cleanup_tree(storage.parent().unwrap());
}
