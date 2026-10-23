use super::*;
#[test]
fn languages_are_per_client_and_explicit_script_wins() {
    assert_eq!(Locale::parse("zh_Hant_CN.UTF-8"), Some(Locale::Traditional));
    assert_eq!(Locale::parse("zh-Hans-TW"), Some(Locale::Simplified));
    for tag in ["zh-TW", "zh-HK", "zh-MO"] {
        assert_eq!(Locale::parse(tag), Some(Locale::Traditional));
    }
    let a = LocaleContext::resolve(LanguagePreference::System, ["fr".into(), "zh-CN".into()]);
    let b = LocaleContext::new(Locale::English);
    assert_eq!(a.locale, Locale::Simplified);
    assert_eq!(b.locale, Locale::English);
}
#[test]
fn unknown_message_uses_its_fallback_and_parameters_are_not_reinterpreted() {
    let msg = UiMessage {
        key: "unknown".into(),
        args: BTreeMap::new(),
        arg_presentations: BTreeMap::new(),
        fallback: "diagnostic".into(),
    }
    .arg("path", "{other}");
    assert_eq!(LocaleContext::default().render(&msg), "diagnostic");
    let msg = UiMessage {
        fallback: "Failed: {path}".into(),
        ..msg
    };
    assert_eq!(LocaleContext::default().render(&msg), "Failed: {other}");
}
#[test]
fn preference_merge_preserves_other_fields_and_corruption_is_not_overwritten() {
    let (root, _) = crate::test_support::roots("locale-prefs");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("preferences.json");
    std::fs::write(&path, r#"{"autoCheck":false}"#).unwrap();
    save_preference(&path, LanguagePreference::Fixed(Locale::Traditional)).unwrap();
    assert_eq!(
        load_preference(&path).unwrap(),
        LanguagePreference::Fixed(Locale::Traditional)
    );
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["autoCheck"], false);
    std::fs::write(&path, "broken").unwrap();
    assert!(save_preference(&path, LanguagePreference::System).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    crate::test_support::cleanup_tree(&root);
}
#[test]
fn global_language_does_not_consume_prompt_or_tool_arguments() {
    let parse =
        |args: &[&str]| leading_language(args.iter().map(|s| s.to_string()).collect()).unwrap();
    let (language, rest) = parse(&["--lang", "zh-Hans", "exec", "--lang", "verbatim prompt"]);
    assert_eq!(language.as_deref(), Some("zh-Hans"));
    assert_eq!(rest, ["exec", "--lang", "verbatim prompt"]);
    assert_eq!(parse(&["exec", "--lang=zh-Hant"]).0, None);
    assert_eq!(
        parse(&["--", "--lang", "zh-Hant"]).1,
        ["--", "--lang", "zh-Hant"]
    );
    assert!(leading_language(vec!["--lang".into()]).is_err());
    assert!(matches!(
        language_command("/language"),
        Some(Ok(LanguageAction::Choose))
    ));
    assert!(language_command("/language en extra").unwrap().is_err());
    assert!(language_command("/language-other").is_none());
}

#[test]
fn priority_does_not_read_preferences_when_an_override_is_valid() {
    let no_read = || panic!("explicit language must not read UI preferences");
    assert_eq!(
        terminal::resolve_terminal_sources(Some("en"), Some("zh-Hant"), no_read)
            .unwrap()
            .preference,
        LanguagePreference::Fixed(Locale::English)
    );
    assert_eq!(
        terminal::resolve_terminal_sources(None, Some("zh-Hant"), no_read)
            .unwrap()
            .preference,
        LanguagePreference::Fixed(Locale::Traditional)
    );
    let resolved = terminal::resolve_terminal_sources(None, Some("invalid"), || {
        Ok(LanguagePreference::Fixed(Locale::Simplified))
    })
    .unwrap();
    assert_eq!(resolved.warnings.len(), 1);
    assert_eq!(resolved.context.locale, Locale::Simplified);
    let broken =
        terminal::resolve_terminal_sources(None, None, || Err("broken JSON".into())).unwrap();
    assert_eq!(broken.warnings.len(), 1);
    assert_eq!(broken.preference, LanguagePreference::System);
}

#[test]
fn simultaneous_ui_writers_merge_independent_preferences() {
    let (root, _) = crate::test_support::roots("locale-merge");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("preferences.json");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut starts = Vec::new();
    let jobs = [
        serde_json::json!({"autoCheck":false}),
        serde_json::json!({"language":"zh-Hant"}),
    ]
    .map(|patch| {
        let path = path.clone();
        let ready = ready_tx.clone();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        starts.push(start_tx);
        std::thread::spawn(move || {
            ready.send(()).unwrap();
            start_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            save_ui_preferences(&path, &patch).unwrap();
        })
    });
    for _ in 0..2 {
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }
    for start in starts {
        start.send(()).unwrap();
    }
    for job in jobs {
        job.join().unwrap();
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["autoCheck"], false);
    assert_eq!(value["language"], "zh-Hant");
    crate::test_support::cleanup_tree(&root);
}

#[cfg(unix)]
#[test]
fn ui_preferences_refuse_links_and_bound_the_read() {
    let (root, _) = crate::test_support::roots("locale-links");
    std::fs::create_dir_all(&root).unwrap();
    let target = root.join("original.json");
    std::fs::write(&target, r#"{"language":"en"}"#).unwrap();
    let link = root.join("preferences.json");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(load_preference(&link).is_err());
    assert!(save_preference(&link, LanguagePreference::System).is_err());
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        r#"{"language":"en"}"#
    );
    std::fs::write(&target, "x".repeat(65537)).unwrap();
    assert!(load_preference(&target).is_err());
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn command_presentations_keep_raw_facts_and_client_languages_separate() {
    let message = UiMessage::new("command.memory_added").arg("id", "{other}<script>");
    let text: PresentedText = message.into();
    assert_eq!(text.raw, "Memory {other}<script> added at revision 1.");
    assert!(
        text.render(LocaleContext::new(Locale::Traditional))
            .contains("已新增記憶 {other}<script>")
    );
    assert!(text.render(LocaleContext::default()).starts_with("Memory"));
}

#[test]
fn quantity_forms_and_runtime_fallback_are_client_local() {
    let english = LocaleContext::new(Locale::English);
    let chinese = LocaleContext::new(Locale::Traditional);
    let message = |count| UiMessage::new("web.images_ready").arg("count", count);
    assert!(english.render(&message(1)).contains("1 image"));
    assert!(!english.render(&message(1)).contains("images"));
    assert!(english.render(&message(2)).contains("2 images"));
    assert!(chinese.render(&message(2)).contains("2"));
    let missing = UiMessage {
        key: "extension.future.message".into(),
        args: BTreeMap::new(),
        arg_presentations: BTreeMap::new(),
        fallback: "English fallback {path}".into(),
    }
    .arg("path", "<script>{other}");
    assert_eq!(chinese.render(&missing), "English fallback <script>{other}");
    assert_eq!(
        english.render(&message(1)),
        LocaleContext::default().render(&message(1))
    );
}

#[test]
fn described_arguments_switch_languages_but_raw_parameters_stay_opaque() {
    let message = UiMessage::new("common.error").arg_message(
        "detail",
        UiMessage::new("common.language_saved").arg("language", "<script>{path}"),
    );
    let raw = message.english();
    let chinese = LocaleContext::new(Locale::Traditional).render(&message);
    assert_ne!(raw, chinese);
    assert!(chinese.contains("<script>{path}"));
    let raw_object =
        serde_json::json!({"key":"common.language_saved","args":{},"fallback":"arbitrary"});
    let ordinary = UiMessage::new("common.error").arg("detail", raw_object.clone());
    assert!(
        LocaleContext::new(Locale::Traditional)
            .render(&ordinary)
            .contains(&raw_object.to_string())
    );
    let wire = serde_json::to_value(&message).unwrap();
    assert_eq!(
        wire["args"]["detail"],
        message.arg_presentations["detail"].english()
    );
    let restored: UiMessage = serde_json::from_value(wire).unwrap();
    assert_eq!(restored, message);
}

#[test]
fn every_builtin_preset_keeps_its_english_description_and_has_both_translations() {
    for preset in crate::presets::MODEL_PRESETS {
        let message = preset_presentation(preset.id, preset.description);
        assert_eq!(message.english(), preset.description);
        for locale in [Locale::Simplified, Locale::Traditional] {
            let text = LocaleContext::new(locale).render(&message);
            assert!(!text.contains("[missing translation]"));
            assert_ne!(text, preset.description);
        }
    }
}

#[test]
fn background_result_presentations_keep_english_facts_and_diagnostics() {
    let report = crate::application::CompactReport {
        shadowed_count: 7,
        degraded: Some("raw provider diagnostic {path}".into()),
    };
    let message = report.status_presentation();
    assert_eq!(
        message.english(),
        "compacted: shadowed 7 events (degraded: raw provider diagnostic {path})"
    );
    for locale in [Locale::Simplified, Locale::Traditional] {
        let text = LocaleContext::new(locale).render(&message);
        assert!(text.contains("raw provider diagnostic {path}"));
        assert_ne!(text, message.english());
    }
}
