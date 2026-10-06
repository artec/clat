use super::*;
use serde_json::json;

#[test]
fn plg6_capability_commit_requires_explicit_approval_and_all_review_pages() {
    let client = crate::host::HostClient::fixture_for_frontend_tests(&std::env::temp_dir());
    let mut dialog = PluginDialog::new();
    dialog.review = Some(client.plugin_review_fixture_for_tests(json!({"review":{"packages":[]}})));
    dialog.view = View::Review;
    dialog.total = 30;
    dialog.reviewed = 30;
    assert!(
        !dialog.can_commit(),
        "Enter must not request activation without consent"
    );
    dialog.accepted = true;
    dialog.reviewed = 10;
    assert!(
        !dialog.can_commit(),
        "unread permissions cannot be approved"
    );
    dialog.reviewed = 30;
    assert!(dialog.can_commit());
    dialog.pending = true;
    assert!(!dialog.can_commit());
}

#[test]
fn plg6_configuration_is_masked_cleared_and_expired_without_history() {
    let mut dialog = PluginDialog::new();
    dialog.view = View::Config;
    dialog
        .config
        .insert_str("{\"plugin.id\":{\"key\":\"private-marker-47\"}}");
    let text = render::plugin_lines(&dialog, 60)
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
        .collect::<String>();
    assert!(!text.contains("private-marker-47"));
    assert!(text.contains("masked"));
    dialog.expires = Some(Instant::now() - Duration::from_secs(1));
    dialog.expire();
    assert!(dialog.config.text().is_empty());
    assert!(!dialog.accepted);
}

#[cfg(feature = "runtime-tests")]
fn shell() -> (App, PathBuf) {
    let (root, project) = crate::test_support::roots("plg6-ui");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let client = crate::host::HostClient::fixture_for_frontend_tests(&root);
    let mut app = App::open_native(Project::new(&project), client).unwrap();
    app.set_native_online_for_snapshot(true);
    (app, root)
}

#[test]
#[cfg(feature = "runtime-tests")]
fn plg6_reopen_rejects_late_review_and_close_leaves_composer_untouched() {
    let (mut app, root) = shell();
    let (tx, _rx) = mpsc::sync_channel(8);
    app.event_sender = Some(tx);
    app.input.insert_str("owner composer text");
    app.plugins.dialog = Some(PluginDialog::new());
    let first = app.plugin_request().unwrap().4;
    app.close_plugin_dialog();
    app.plugins.dialog = Some(PluginDialog::new());
    let second = app.plugin_request().unwrap().4;
    assert_ne!(first, second);
    let client = app.native_plugin_context().unwrap().0;
    let ticket = client.plugin_review_fixture_for_tests(json!({}));
    app.plugin_loaded(0, 0, first, PluginReply::Review(Ok(ticket)));
    assert!(app.plugins.dialog.as_ref().unwrap().review.is_none());
    assert_eq!(app.input.text(), "owner composer text");
    app.plugins
        .dialog
        .as_mut()
        .unwrap()
        .config
        .insert_str("private-config");
    app.close_plugin_dialog();
    assert!(app.plugins.dialog.is_none());
    drop(app);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(feature = "runtime-tests")]
fn plg6_review_resize_invalidates_consent_and_all_packages_are_visible() {
    use ratatui::{Terminal, backend::TestBackend};
    let (mut app, root) = shell();
    let ticket = app
        .native_plugin_context()
        .unwrap()
        .0
        .plugin_review_fixture_for_tests(json!({"review":{
            "packages":[{"id":"first","capabilities":{"hostTools":["read_file"]}},
            {"id":"last","capabilities":{"network":{"origins":["https://example.invalid"]}}}]
        }}));
    let mut dialog = PluginDialog::new();
    dialog.review = Some(ticket);
    dialog.view = View::Review;
    app.plugins.dialog = Some(dialog);
    let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
    terminal
        .draw(|frame| app.draw_plugin_dialog(frame))
        .unwrap();
    let dialog = app.plugins.dialog.as_mut().unwrap();
    dialog.reviewed = dialog.total;
    dialog.accepted = true;
    let mut short = Terminal::new(TestBackend::new(90, 18)).unwrap();
    short.draw(|frame| app.draw_plugin_dialog(frame)).unwrap();
    assert!(!app.plugins.dialog.as_ref().unwrap().accepted);
    app.plugins.dialog.as_mut().unwrap().accepted = true;
    let mut narrow = Terminal::new(TestBackend::new(40, 18)).unwrap();
    narrow.draw(|frame| app.draw_plugin_dialog(frame)).unwrap();
    assert!(!app.plugins.dialog.as_ref().unwrap().accepted);
    assert!(!app.plugins.dialog.as_ref().unwrap().can_commit());
    drop(app);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(feature = "runtime-tests")]
fn plg6_config_paste_and_close_never_modify_conversation_input() {
    let (mut app, root) = shell();
    app.input.insert_str("conversation draft");
    let mut dialog = PluginDialog::new();
    dialog.view = View::Config;
    app.plugins.dialog = Some(dialog);
    assert!(app.plugin_paste("{\"id\":{\"key\":\"secret-marker\"}}"));
    assert_eq!(app.input.text(), "conversation draft");
    assert!(app.plugin_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(
        app.plugins
            .dialog
            .as_ref()
            .unwrap()
            .config
            .text()
            .is_empty()
    );
    assert!(app.plugin_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    app.plugins.dialog.as_mut().unwrap().pending = true;
    assert!(app.plugin_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    assert!(app.plugins.dialog.is_none());
    assert_eq!(app.input.text(), "conversation draft");
    drop(app);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(feature = "runtime-tests")]
fn plg6_browse_scrolls_to_last_plugin_and_filters_by_id() {
    let (mut app, root) = shell();
    let mut dialog = PluginDialog::new();
    dialog.installed = (0..60)
        .map(|i| json!({"id":format!("plugin-{i:02}"),"health":"healthy"}))
        .collect();
    dialog.selected = 59;
    let lines = render::browse_viewport_for_tests(&dialog, 60, 12);
    assert!(lines.contains("plugin-59"));
    assert!(!lines.contains("plugin-00"));
    dialog.query.insert_str("plugin-42");
    assert_eq!(dialog.rows().len(), 1);
    assert_eq!(dialog.rows()[0]["id"], "plugin-42");
    app.plugins.dialog = Some(dialog);
    drop(app);
    crate::test_support::cleanup_tree(&root);
}

#[test]
#[cfg(feature = "runtime-tests")]
fn plg6_failed_generation_switch_never_offers_previous_market_packages() {
    let (mut app, root) = shell();
    let mut dialog = PluginDialog::new();
    dialog.view = View::Market;
    dialog.market = vec![json!({"id":"old-generation-package","manifest_version":1})];
    app.plugins.dialog = Some(dialog);
    app.plugin_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    let dialog = app.plugins.dialog.as_ref().unwrap();
    assert_eq!(dialog.market_generation, 2);
    assert!(
        dialog.rows().is_empty(),
        "an unavailable v2 index must not offer a v1 row under its heading"
    );
    drop(app);
    crate::test_support::cleanup_tree(&root);
}
