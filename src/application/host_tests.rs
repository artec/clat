use super::*;
use crate::test_support::{TestBehavior, TestProviderPlugin};

fn fixture(label: &str) -> (PathBuf, HostApplication, Project, Project) {
    let (root, _) = crate::test_support::roots(label);
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let first = Project::new(first);
    let second = Project::new(second);
    let application = BootstrapApplication::open(first.clone(), root.join("state"))
        .unwrap()
        .with_permission_modes()
        .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
            behavior: TestBehavior::Success,
        }))
        .unwrap();
    (root, HostApplication::new(application), first, second)
}

#[test]
fn model_profile_auth_patch_preserves_secrets_and_rejects_invalid_writes() {
    let (root, mut host, first, _) = fixture("profile-auth");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let config = crate::ModelConfig {
            model: "test".into(),
            endpoint: "https://example.invalid/v1".into(),
            auth_header: "X-Private".into(),
            auth_prefix: "hidden-prefix ".into(),
            extra_body: serde_json::json!({"foreign":"hidden-body"}),
            ..Default::default()
        };
        let mut credentials = crate::ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, "private-key".into());
        app.save_model_profile("auth", &config, &credentials)
            .unwrap();
        let view = serde_json::to_value(app.model_profile_view("auth").unwrap()).unwrap();
        assert_eq!(
            view["auth_edit_supported"], true,
            "host must advertise write-only auth editing"
        );
        for secret in ["X-Private", "hidden-prefix", "hidden-body", "private-key"] {
            assert!(!view.to_string().contains(secret));
        }
        app.activate_model_profile("auth").unwrap().unwrap();
        let mut params = serde_json::json!({
            "name":"auth", "protocol":config.protocol, "model":config.model,
            "endpoint":config.endpoint, "request_path":config.request_path,
            "auth":{"prefix":"Token "}
        });
        let save = |params: &serde_json::Value| {
            app.edit_model_profile(serde_json::from_value(params.clone()).unwrap())
        };
        save(&params).unwrap();
        let (saved, key) = app.load_model_profile("auth").unwrap().unwrap();
        assert_eq!(saved.auth_prefix, "Token ");
        assert_eq!(saved.auth_header, "X-Private");
        assert_eq!(saved.extra_body, config.extra_body);
        assert_eq!(key.value(0), Some("private-key"));
        assert_eq!(app.model_state().unwrap().0.auth_prefix, "hidden-prefix ");
        for auth in [
            serde_json::json!({"header":"bad header"}),
            serde_json::json!({"prefix":"secret\r\nInjected: yes"}),
        ] {
            params["auth"] = auth;
            let error = save(&params).unwrap_err().to_string();
            assert!(!error.contains("secret"));
            assert_eq!(
                app.load_model_profile("auth")
                    .unwrap()
                    .unwrap()
                    .0
                    .auth_prefix,
                "Token "
            );
        }
        params["auth"] = serde_json::json!({"header":"", "prefix":""});
        save(&params).unwrap();
        params.as_object_mut().unwrap().remove("auth");
        save(&params).unwrap();
        let saved = app.load_model_profile("auth").unwrap().unwrap().0;
        assert!(saved.auth_header.is_empty() && saved.auth_prefix.is_empty());
    }
    drop(project);
    host.close().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_profile_tuning_preserves_hidden_state_and_changes_only_explicit_fields() {
    let (root, mut host, first, _) = fixture("profile-tuning");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let config = crate::ModelConfig {
            model: "test".into(),
            endpoint: "https://api.deepseek.com/v1".into(),
            temperature: Some(0.5),
            parallel_tool_calls: false,
            thinking_level: Some(crate::ThinkingLevel::High),
            extra_body: serde_json::json!({"foreign":"hidden-body"}),
            extra_headers: serde_json::json!({"X-Private":"hidden-header"}),
            ..Default::default()
        };
        let mut credentials = crate::ProviderCredentials::for_protocol(config.protocol);
        credentials.set_value(0, "private-key".into());
        app.save_model_profile("tuning", &config, &credentials)
            .unwrap();
        let view =
            serde_json::to_value(app.model_profile_view("tuning").unwrap().unwrap()).unwrap();
        assert!(
            view["tuning"].is_object(),
            "host must expose non-secret tuning"
        );
        for secret in ["hidden-body", "hidden-header", "private-key"] {
            assert!(!view.to_string().contains(secret));
        }
        app.activate_model_profile("tuning").unwrap().unwrap();
        let frozen = app.model_state().unwrap().0;
        let mut params = serde_json::json!({
            "name":"tuning", "protocol":config.protocol, "model":config.model,
            "endpoint":config.endpoint, "request_path":config.request_path,
            "tuning":{"temperature":{"set":0.75}}
        });
        let save = |params: &serde_json::Value| {
            app.edit_model_profile(serde_json::from_value(params.clone()).unwrap())
        };
        save(&params).unwrap();
        let edited = app.load_model_profile("tuning").unwrap().unwrap().0;
        assert_eq!(edited.temperature, Some(0.75));
        assert!(!edited.parallel_tool_calls);
        assert_eq!(edited.thinking_level, Some(crate::ThinkingLevel::High));
        assert_eq!(edited.extra_headers, config.extra_headers);
        assert_eq!(edited.extra_body["foreign"], "hidden-body");
        assert_eq!(
            app.model_state().unwrap().0.temperature,
            Some(0.5),
            "save does not activate"
        );
        params["tuning"] = serde_json::json!({"temperature":{"set":-0.1}});
        assert!(save(&params).is_err());
        assert_eq!(
            app.load_model_profile("tuning")
                .unwrap()
                .unwrap()
                .0
                .temperature,
            Some(0.75)
        );
        params["tuning"] = serde_json::json!({"temperature":"clear","parallel_tool_calls":"clear","thinking_level":"clear"});
        save(&params).unwrap();
        app.activate_model_profile("tuning").unwrap().unwrap();
        let current = app.model_state().unwrap().0;
        assert_eq!(current.temperature, None);
        assert_eq!(current.request_parallel_tool_calls(), None);
        assert_eq!(current.thinking_level, None);
        assert!(current.extra_body.get("reasoning_effort").is_none());
        assert_eq!(frozen.temperature, Some(0.5));
        assert_eq!(frozen.thinking_level, Some(crate::ThinkingLevel::High));
        params.as_object_mut().unwrap().remove("tuning");
        save(&params).unwrap();
        let saved = app.load_model_profile("tuning").unwrap().unwrap().0;
        assert_eq!(saved.overrides.temperature, crate::Override::Clear);
        assert_eq!(saved.overrides.parallel_tool_calls, crate::Override::Clear);
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_profile_limits_roundtrip_preserve_and_clear_without_activation() {
    let (root, mut host, first, _) = fixture("profile-limits");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let mut params = serde_json::json!({
            "name":"limits", "protocol":"open_ai_compatible", "model":"test",
            "endpoint":"https://example.invalid/v1", "request_path":"/chat/completions",
            "api_key":"private-key",
            "limits":{"output_limit":8192,"max_context_tokens":131072,"run_token_budget":0}
        });
        let save = |value: &serde_json::Value| {
            app.edit_model_profile(serde_json::from_value(value.clone()).unwrap())
        };
        save(&params).unwrap();
        assert!(app.active_model_profile().unwrap().is_none());
        let view =
            serde_json::to_value(app.model_profile_view("limits").unwrap().unwrap()).unwrap();
        assert_eq!(view["limits"], params["limits"]);
        assert!(!view.to_string().contains("private-key"));
        let mut legacy_view = view.clone();
        legacy_view.as_object_mut().unwrap().remove("limits");
        let legacy_view: model_settings::ModelRouteView =
            serde_json::from_value(legacy_view).unwrap();
        assert!(
            serde_json::to_value(legacy_view)
                .unwrap()
                .get("limits")
                .is_none()
        );
        let original = params["limits"].clone();
        params.as_object_mut().unwrap().remove("limits");
        params.as_object_mut().unwrap().remove("api_key");
        save(&params).unwrap();
        let view =
            serde_json::to_value(app.model_profile_view("limits").unwrap().unwrap()).unwrap();
        assert_eq!(view["limits"], original, "old client must preserve limits");
        params["limits"] =
            serde_json::json!({"output_limit":0,"max_context_tokens":131072,"run_token_budget":0});
        assert!(save(&params).is_err());
        params["limits"] =
            serde_json::json!({"output_limit":8192,"max_context_tokens":4095,"run_token_budget":0});
        assert!(save(&params).is_err());
        assert_eq!(
            app.load_model_profile("limits")
                .unwrap()
                .unwrap()
                .0
                .output_limit,
            Some(8192)
        );
        params["limits"] = serde_json::json!({"output_limit":null,"max_context_tokens":null,"run_token_budget":null});
        save(&params).unwrap();
        app.activate_model_profile("limits").unwrap().unwrap();
        let (config, credentials) = app.model_state().unwrap();
        assert_eq!(config.output_limit, None);
        assert_eq!(config.max_context_tokens, None);
        assert_eq!(config.run_token_budget, None);
        assert_eq!(credentials.value(0), Some("private-key"));
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_profile_read_edit_preserves_exact_endpoint_and_credentials() {
    let (root, mut host, first, _) = fixture("profile-roundtrip");
    let a = host.attach(first).unwrap();
    {
        let a = a.lock().unwrap();
        let endpoint = "https://example.invalid/v1/";
        a.edit_model_profile(ModelProfileEdit {
            name: "roundtrip".into(),
            protocol: crate::ModelProtocol::OpenAiCompatible,
            model: "model".into(),
            endpoint: endpoint.into(),
            request_path: "/chat/completions".into(),
            api_key: Some("roundtrip-secret".into()),
            auth: None,
            extra_headers: None,
            extra_body: None,
            limits: None,
            tuning: None,
        })
        .unwrap();
        let view = a.model_profile_view("roundtrip").unwrap().unwrap();
        assert_eq!(
            view.endpoint, endpoint,
            "safe endpoint must round-trip byte-exactly"
        );
        assert!(!view.route_redacted);
        a.edit_model_profile(ModelProfileEdit {
            name: "roundtrip".into(),
            protocol: view.protocol,
            model: view.model,
            endpoint: view.endpoint,
            request_path: view.request_path,
            api_key: None,
            auth: None,
            extra_headers: None,
            extra_body: None,
            limits: None,
            tuning: None,
        })
        .unwrap();
        assert!(
            a.model_profile_view("roundtrip")
                .unwrap()
                .unwrap()
                .credential_set
        );
        let (mut config, credentials) = a.load_model_profile("roundtrip").unwrap().unwrap();
        config.endpoint =
            "https://person:hidden-secret@example.invalid/v1/?key=hidden-secret".into();
        a.save_model_profile("legacy", &config, &credentials)
            .unwrap();
        let view = a.model_profile_view("legacy").unwrap().unwrap();
        assert!(
            view.route_redacted,
            "lossy summaries must not become an editable route"
        );
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains("hidden-secret")
        );
    }
    drop(a);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn thinking_cycle_uses_shared_current_model_and_keeps_frozen_snapshot() {
    let (root, mut host, first, second) = fixture("host-thinking");
    let a = host.attach(first).unwrap();
    let b = host
        .authorize_and_attach(second, ProjectAuthorization::grant())
        .unwrap();
    {
        let a = a.lock().unwrap();
        let b = b.lock().unwrap();
        a.select_model_preset(crate::presets::MODEL_PRESETS[0].id, None)
            .unwrap();
        let (mut frozen, credentials) = a.model_state().unwrap();
        frozen.thinking_level = Some(crate::ThinkingLevel::High);
        a.save_model_state(&frozen, &credentials).unwrap();
        let (tx, rx) = mpsc::channel();
        b.subscribe(tx);
        assert_eq!(a.cycle_model_thinking().unwrap(), crate::ThinkingLevel::Max);
        assert_eq!(b.cycle_model_thinking().unwrap(), crate::ThinkingLevel::Low);
        assert_eq!(frozen.thinking_level, Some(crate::ThinkingLevel::High));
        let (mut current, _) = a.model_state().unwrap();
        current.apply_overrides();
        assert_eq!(
            crate::effective_thinking_level(&current),
            Some(crate::ThinkingLevel::Low)
        );
        assert!(
            rx.try_iter()
                .any(|event| event == ApplicationEvent::ModelsUpdated)
        );
        current.endpoint = "https://example.invalid/v1".into();
        current.preset = None;
        a.save_model_state(&current, &credentials).unwrap();
        let before = serde_json::to_value(&a.model_state().unwrap().0).unwrap();
        assert!(b.cycle_model_thinking().is_err());
        assert_eq!(
            serde_json::to_value(&a.model_state().unwrap().0).unwrap(),
            before
        );
    }
    drop((a, b));
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_changes_notify_every_project_and_never_return_secrets() {
    let (root, mut host, first, second) = fixture("host-models");
    let a = host.attach(first).unwrap();
    let b = host
        .authorize_and_attach(second, ProjectAuthorization::grant())
        .unwrap();
    {
        let a = a.lock().unwrap();
        let b = b.lock().unwrap();
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        a.subscribe(tx_a);
        b.subscribe(tx_b);
        let edit = |endpoint: &str, key: Option<&str>| ModelProfileEdit {
            name: "shared".into(),
            protocol: crate::ModelProtocol::OpenAiCompatible,
            model: "model-test".into(),
            endpoint: endpoint.into(),
            request_path: "/chat/completions".into(),
            api_key: key.map(str::to_owned),
            auth: None,
            extra_headers: None,
            extra_body: None,
            limits: None,
            tuning: None,
        };
        a.edit_model_profile(edit(
            "https://example.invalid/v1",
            Some("secret-host-model"),
        ))
        .unwrap();
        a.activate_model_profile("shared").unwrap().unwrap();
        assert_eq!(b.active_model_profile().unwrap().as_deref(), Some("shared"));
        assert_eq!(b.model_state().unwrap().0.model, "model-test");
        for receiver in [&rx_a, &rx_b] {
            assert!(
                receiver
                    .try_iter()
                    .any(|event| event == ApplicationEvent::ModelsUpdated)
            );
        }
        let view = serde_json::to_string(&b.model_settings_view().unwrap()).unwrap();
        assert!(!view.contains("secret-host-model"));
        assert!(
            b.model_profile_view("shared")
                .unwrap()
                .unwrap()
                .credential_set
        );
        a.edit_model_profile(edit("https://example.invalid/v1", None))
            .unwrap();
        assert!(
            a.model_profile_view("shared")
                .unwrap()
                .unwrap()
                .credential_set
        );
        a.edit_model_profile(edit("https://different.invalid/v1", None))
            .unwrap();
        assert!(
            !a.model_profile_view("shared")
                .unwrap()
                .unwrap()
                .credential_set,
            "route replacement cannot forward the previous endpoint's credential"
        );
        a.delete_model_profile_with_fallback("shared").unwrap();
        assert!(b.active_model_profile().unwrap().is_none());
        assert!(b.model_state().unwrap().1.value(0).unwrap().is_empty());
    }
    drop((a, b));
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn utility_settings_notify_every_attached_project() {
    let (root, mut host, first, second) = fixture("utility-settings-notify");
    let a = host.attach(first).unwrap();
    let b = host
        .authorize_and_attach(second, ProjectAuthorization::grant())
        .unwrap();
    {
        let a = a.lock().unwrap();
        let b = b.lock().unwrap();
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        a.subscribe(tx_a);
        b.subscribe(tx_b);
        a.edit_utility_settings(UtilitySettingsEdit {
            naming_enabled: false,
            suggestions_enabled: true,
            profile: None,
        })
        .unwrap();
        for receiver in [&rx_a, &rx_b] {
            assert!(
                receiver
                    .try_iter()
                    .any(|event| event == ApplicationEvent::ModelsUpdated),
                "utility settings changes must refresh every attached frontend"
            );
        }
        assert!(!b.utility_settings_view().unwrap().naming_enabled);
        assert!(b.utility_settings_view().unwrap().suggestions_enabled);
    }
    drop((a, b));
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_profile_headers_replace_preserve_and_reject_without_writes() {
    let (root, mut host, first, _) = fixture("profile-headers");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let mut params = serde_json::json!({"name":"headers", "protocol":"open_ai_compatible",
            "model":"test", "endpoint":"https://example.invalid/v1", "request_path":"/chat/completions",
            "api_key":"key-secret", "extra_headers":{"X-Private":"header-secret"}});
        let save = |value: serde_json::Value| {
            app.edit_model_profile(serde_json::from_value(value).unwrap())
        };
        save(params.clone()).unwrap();
        app.activate_model_profile("headers").unwrap();
        let frozen = app.model_state().unwrap().0;
        let view = serde_json::to_value(app.model_profile_view("headers").unwrap()).unwrap();
        assert_eq!(view["extra_headers_edit_supported"], true);
        assert!(!view.to_string().contains("X-Private"));
        assert!(!view.to_string().contains("header-secret"));
        params.as_object_mut().unwrap().remove("api_key");
        params.as_object_mut().unwrap().remove("extra_headers");
        save(params.clone()).unwrap();
        assert_eq!(
            app.load_model_profile("headers")
                .unwrap()
                .unwrap()
                .0
                .extra_headers,
            frozen.extra_headers
        );
        params["extra_headers"] = serde_json::Value::Null;
        save(params.clone()).unwrap();
        assert_eq!(
            app.load_model_profile("headers")
                .unwrap()
                .unwrap()
                .0
                .extra_headers,
            frozen.extra_headers
        );
        for invalid in [
            serde_json::json!([]),
            serde_json::json!({"X":"bad\nsecret"}),
        ] {
            params["extra_headers"] = invalid;
            assert!(save(params.clone()).is_err());
            assert_eq!(
                app.load_model_profile("headers")
                    .unwrap()
                    .unwrap()
                    .0
                    .extra_headers,
                frozen.extra_headers
            );
        }
        params["extra_headers"] = serde_json::json!({"X-New":"replacement"});
        save(params.clone()).unwrap();
        let (saved, key) = app.load_model_profile("headers").unwrap().unwrap();
        assert_eq!(saved.extra_headers, params["extra_headers"]);
        assert_eq!(key.value(0), Some("key-secret"));
        assert_eq!(saved.extra_body, frozen.extra_body);
        assert_eq!(
            app.model_state().unwrap().0.extra_headers,
            frozen.extra_headers,
            "save does not activate"
        );
        params["endpoint"] = "https://other.invalid/v1".into();
        params.as_object_mut().unwrap().remove("extra_headers");
        save(params.clone()).unwrap();
        let (saved, key) = app.load_model_profile("headers").unwrap().unwrap();
        assert_eq!(saved.extra_headers, serde_json::json!({}));
        assert!(key.value(0).is_none_or(str::is_empty));
        params["extra_headers"] = serde_json::json!({"X":"one"});
        save(params.clone()).unwrap();
        params["extra_headers"] = serde_json::json!({});
        save(params).unwrap();
        assert_eq!(
            app.load_model_profile("headers")
                .unwrap()
                .unwrap()
                .0
                .extra_headers,
            serde_json::json!({})
        );
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn model_profile_body_survives_activation_and_rejects_conflicting_thinking() {
    let (root, mut host, first, _) = fixture("profile-body");
    let project = host.attach(first).unwrap();
    {
        let app = project.lock().unwrap();
        let mut params = serde_json::json!({"name":"body", "protocol":"open_ai_compatible",
            "model":"test", "endpoint":"https://api.deepseek.com/v1", "request_path":"/chat/completions",
            "api_key":"key-secret", "tuning":{"thinking_level":{"set":"max"}}});
        let save = |value: serde_json::Value| {
            app.edit_model_profile(serde_json::from_value(value).unwrap())
        };
        save(params.clone()).unwrap();
        app.activate_model_profile("body").unwrap();
        let frozen = app.model_state().unwrap().0;
        assert_eq!(frozen.extra_body["reasoning_effort"], "max");
        params.as_object_mut().unwrap().remove("api_key");
        let raw = serde_json::json!({"reasoning_effort":"low", "metadata":{"private":"body-secret"}, "stop":null});
        params["extra_body"] = raw.clone();
        for conflict in [
            serde_json::json!({"set":"high"}),
            serde_json::json!("clear"),
        ] {
            params["tuning"]["thinking_level"] = conflict;
            assert!(save(params.clone()).is_err());
            assert_eq!(
                app.load_model_profile("body")
                    .unwrap()
                    .unwrap()
                    .0
                    .extra_body,
                frozen.extra_body
            );
        }
        params.as_object_mut().unwrap().remove("tuning");
        for invalid in [
            serde_json::json!([]),
            serde_json::json!({"private":"x".repeat(65536)}),
        ] {
            params["extra_body"] = invalid;
            assert!(save(params.clone()).is_err());
            assert_eq!(
                app.load_model_profile("body")
                    .unwrap()
                    .unwrap()
                    .0
                    .extra_body,
                frozen.extra_body
            );
        }
        params["extra_body"] = raw.clone();
        save(params.clone()).unwrap();
        let (saved, key) = app.load_model_profile("body").unwrap().unwrap();
        assert_eq!(saved.extra_body, raw);
        assert!(saved.thinking_level.is_none());
        assert!(saved.overrides.thinking_level.is_inherit());
        assert_eq!(key.value(0), Some("key-secret"));
        assert_eq!(
            app.model_state().unwrap().0.extra_body,
            frozen.extra_body,
            "save is not activation"
        );
        app.activate_model_profile("body").unwrap();
        assert_eq!(
            app.model_state().unwrap().0.extra_body,
            raw,
            "load must not rewrite raw thinking"
        );
        let view = serde_json::to_value(app.model_profile_view("body").unwrap()).unwrap();
        assert_eq!(view["extra_body_edit_supported"], true);
        assert!(!view.to_string().contains("body-secret"));
        params.as_object_mut().unwrap().remove("extra_body");
        save(params.clone()).unwrap();
        assert_eq!(
            app.load_model_profile("body")
                .unwrap()
                .unwrap()
                .0
                .extra_body,
            raw
        );
        params["extra_body"] = serde_json::Value::Null;
        save(params.clone()).unwrap();
        assert_eq!(
            app.load_model_profile("body")
                .unwrap()
                .unwrap()
                .0
                .extra_body,
            raw
        );
        params["endpoint"] = "https://other.invalid/v1".into();
        save(params.clone()).unwrap();
        assert_eq!(
            app.load_model_profile("body")
                .unwrap()
                .unwrap()
                .0
                .extra_body,
            serde_json::json!({})
        );
        params["extra_body"] = raw;
        save(params.clone()).unwrap();
        params["extra_body"] = serde_json::json!({});
        save(params).unwrap();
        app.activate_model_profile("body").unwrap();
        assert_eq!(
            app.model_state().unwrap().0.extra_body,
            serde_json::json!({})
        );
    }
    drop(project);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn control_model_state_and_profile_pointer_commit_together() {
    let (root, mut host, first, _) = fixture("model-atomic-pointer");
    let application = host.attach(first).unwrap();
    {
        let app = application.lock().unwrap();
        let (mut config, credentials) = app.model_state().unwrap();
        config.model = "profile-model".into();
        app.control
            .save_profile("atomic", &config, &credentials)
            .unwrap();
        app.control.activate_profile("atomic").unwrap().unwrap();
        assert_eq!(
            app.control.active_profile().unwrap().as_deref(),
            Some("atomic")
        );
        config.model = "direct-model".into();
        app.control.save_model_state(&config, &credentials).unwrap();
        assert!(
            app.control.active_profile().unwrap().is_none(),
            "a direct save must clear the pointer without a second write"
        );
        let on_disk = ControlStorage::open_ready(&root.join("state")).unwrap();
        assert!(on_disk.active_profile().unwrap().is_none());
        assert_eq!(
            on_disk.load_model_state().unwrap().unwrap().0.model,
            "direct-model"
        );
    }
    drop(application);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn host_reuses_one_project_writer_and_preserves_trust_boundary() {
    let (root, mut host, first, second) = fixture("host-identities");
    assert!(host.attach(second.clone()).is_err());
    assert_eq!(host.project_roots().len(), 1);
    let a = host.attach(first.clone()).unwrap();
    let alias = host.attach(Project::new(first.root().join("."))).unwrap();
    assert!(Arc::ptr_eq(&a, &alias));
    let b = host
        .authorize_and_attach(second.clone(), ProjectAuthorization::grant())
        .unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    assert!(host.close().is_err());
    assert_eq!(
        host.project_roots().len(),
        2,
        "busy close must preserve registry"
    );
    drop((a, alias, b));
    host.close().unwrap();
    drop(host);
    let reopened = BootstrapApplication::open(second, root.join("state")).unwrap();
    assert!(reopened.is_trusted().unwrap());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn host_projects_share_control_updates_but_not_permissions_or_drafts() {
    let (root, mut host, first, second) = fixture("host-isolation");
    let a = host.attach(first).unwrap();
    let b = host
        .authorize_and_attach(second, ProjectAuthorization::grant())
        .unwrap();
    {
        let a = a.lock().unwrap();
        let b = b.lock().unwrap();
        let (mut config, credentials) = a.model_state().unwrap();
        config.preset = None;
        config.model = "first-profile".into();
        a.save_model_profile("first", &config, &credentials)
            .unwrap();
        config.model = "second-profile".into();
        b.save_model_profile("second", &config, &credentials)
            .unwrap();
        assert_eq!(a.list_model_profiles().unwrap().len(), 2);
        assert_eq!(
            b.load_model_profile("first").unwrap().unwrap().0.model,
            "first-profile"
        );
        a.set_permission_mode(crate::PermissionMode::FullAccess)
            .unwrap();
        assert_ne!(a.permission_mode(), b.permission_mode());
        assert!(!Arc::ptr_eq(&a.draft_image_store(), &b.draft_image_store()));
        assert!(a.current_session_id().is_none());
        assert!(b.current_session_id().is_none());
    }
    drop((a, b));
    host.close().unwrap();
    drop(host);
    let control = ControlStorage::open_ready(&root.join("state")).unwrap();
    assert_eq!(control.list_profiles().unwrap().len(), 2);
    drop(control);
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn project_handle_keeps_root_exclusive_after_host_is_dropped() {
    let (root, mut host, first, second) = fixture("host-lease-lifetime");
    let a = host.attach(first.clone()).unwrap();
    let b = host
        .authorize_and_attach(second, ProjectAuthorization::grant())
        .unwrap();
    drop(host);
    drop(a);
    // Different thread also covers Windows named-mutex recursion semantics.
    let storage = root.join("state");
    let blocked = std::thread::spawn(move || {
        BootstrapApplication::open(first, storage)
            .unwrap()
            .into_trusted()
            .is_err()
    })
    .join()
    .unwrap();
    assert!(blocked, "a surviving project must retain the root lease");
    drop(b);
    crate::test_support::cleanup_tree(&root);
}
