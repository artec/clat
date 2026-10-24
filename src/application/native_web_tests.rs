use super::*;
#[test]
fn native_web_configuration_is_shared_secret_write_only_and_discoverable() {
    let (storage, project) = roots("native-web-application");
    std::fs::create_dir_all(&project).unwrap();
    let app = mount(&Project::new(&project), &storage, TestBehavior::Success);
    assert!(!app.web_settings().unwrap().search_available);
    assert!(app.command_catalog().iter().any(|c| c.name == "web"));
    let before = app
        .capability_query(&json!({"name":"web_search","limit":1}))
        .unwrap();
    assert_eq!(
        before["candidates"][0]["availability"]["state"],
        "not_configured"
    );
    app.save_web_settings(crate::web_tools::WebSettingsEdit {
        fetch_provider: crate::web_tools::FetchProvider::Http,
        use_model_key: false,
        api_key: Some("owned-web-key".into()),
        clear_key: false,
    })
    .unwrap();
    let after = app
        .capability_query(&json!({"name":"web_search","limit":1}))
        .unwrap();
    assert_eq!(after["candidates"][0]["availability"]["state"], "available");
    let mut host = HostApplication::new(app);
    let other = project.with_file_name(format!("second-native-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&other).unwrap();
    let app = host
        .authorize_and_attach(Project::new(&other), ProjectAuthorization::grant())
        .unwrap();
    assert!(app.lock().unwrap().web_settings().unwrap().search_available);
    assert!(
        !serde_json::to_string(&app.lock().unwrap().web_settings().unwrap())
            .unwrap()
            .contains("owned-web-key")
    );
    app.lock()
        .unwrap()
        .save_web_settings(crate::web_tools::WebSettingsEdit {
            fetch_provider: crate::web_tools::FetchProvider::Http,
            use_model_key: false,
            api_key: None,
            clear_key: true,
        })
        .unwrap();
    assert!(
        !host
            .attach(Project::new(&project))
            .unwrap()
            .lock()
            .unwrap()
            .web_settings()
            .unwrap()
            .search_available
    );
    drop(app);
    host.close().unwrap();
    drop(host);
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
    crate::test_support::cleanup_tree(&other);
}
#[test]
fn native_web_bad_configuration_is_localized_and_does_not_create_conversation() {
    let (storage, project) = roots("native-web-invalid-settings");
    std::fs::create_dir_all(&project).unwrap();
    let app = mount(&Project::new(&project), &storage, TestBehavior::Success);
    std::fs::write(storage.join("web.json"), "{broken").unwrap();
    let error = app.web_settings().unwrap_err();
    assert_eq!(error.presentation().key, "webtool.settings_failed");
    assert!(app.current_session_id().is_none());
    app.close().unwrap();
    crate::test_support::cleanup_tree(&storage);
    crate::test_support::cleanup_tree(&project);
}
