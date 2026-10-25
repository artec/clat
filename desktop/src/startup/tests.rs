use super::*;
use std::cell::Cell;

fn saved() -> Result<Option<PathBuf>, PresentedText> {
    Ok(Some(std::env::temp_dir().join("remembered-project")))
}

#[test]
fn loading_lasts_until_navigation_or_failure_is_published_not_just_claimed() {
    let state = Startup::default();
    assert!(state.loading());
    state
        .resolve(
            saved,
            || Ok(None),
            |_| {
                assert!(state.loading());
                Ok(())
            },
        )
        .unwrap();
    assert!(state.loading());
    state.finish();
    assert!(!state.loading());
    let stopped = Startup::default();
    stopped.cancel();
    assert!(!stopped.loading());
}

#[test]
fn startup_resume_is_once_and_never_runs_after_explicit_stop_or_manual_open() {
    let startup = Startup::default();
    let calls = Cell::new(0);
    assert_eq!(
        startup
            .resolve(
                saved,
                || Ok(None),
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(7)
                }
            )
            .unwrap(),
        Some(7)
    );
    assert_eq!(
        startup
            .resolve(saved, || panic!("no discovery retry"), |_| Ok(8))
            .unwrap(),
        None
    );
    assert_eq!(calls.get(), 1);
    let cancelled = Startup::default();
    cancelled.cancel();
    assert_eq!(
        cancelled
            .resolve(saved, || panic!("explicit stop wins"), |_| Ok(8))
            .unwrap(),
        None
    );
}

#[test]
fn startup_first_launch_uses_existing_host_or_welcome_without_opening_a_project() {
    for existing in [None, Some(42)] {
        let state = Startup::default();
        assert_eq!(
            state
                .resolve(
                    || Ok(None),
                    || Ok(existing),
                    |_| panic!("no remembered project")
                )
                .unwrap(),
            existing
        );
    }
    let state = Startup::default();
    assert_eq!(
        state.resolve(saved, || Ok(Some(1)), |_| Ok(2)).unwrap(),
        Some(2)
    );
}

#[test]
fn startup_failures_block_background_fallback_and_do_not_retry_or_hide_the_reason() {
    for key in [
        "host.project_unavailable",
        "desktop.restore_needs_trust",
        "host.same_version_different_build",
    ] {
        let state = Startup::default();
        let error = state
            .resolve(saved, || Ok(Some(1)), |_| Err(UiMessage::new(key).into()))
            .unwrap_err();
        assert_eq!(error.message().key, key);
        assert!(state.blocked());
        assert_eq!(
            state
                .resolve(saved, || panic!("failed startup must not retry"), |_| Ok(2))
                .unwrap(),
            None
        );
        state.cancel();
        assert!(!state.blocked());
    }
    let state = Startup::default();
    assert!(
        state
            .resolve(saved, || Err("incompatible Host".into()), |_| Ok(2))
            .is_err()
    );
    assert!(state.blocked());
}

#[test]
fn startup_preference_errors_do_not_even_attempt_discovery_or_open() {
    let state = Startup::default();
    let result = state.resolve::<u8>(
        || Err("bad UI preference".into()),
        || panic!("no discovery"),
        |_| panic!("no start"),
    );
    assert_eq!(result.unwrap_err().raw, "bad UI preference");
    assert!(state.blocked());
}

#[test]
fn startup_preferences_survive_language_updates_and_reopen_and_reject_invalid_records() {
    let root = std::env::temp_dir().join(format!(
        "clat-desktop-resume-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("preferences.json");
    assert_eq!(load(&path).unwrap(), None);
    save_ui_preferences(&path, &json!({"lastProject":root})).unwrap();
    save_ui_preferences(&path, &json!({"language":"zh-Hant","autoCheck":false})).unwrap();
    assert_eq!(load(&path).unwrap().as_deref(), Some(root.as_path()));
    assert_eq!(load_ui_preferences(&path).unwrap()["autoCheck"], false);
    for bad in [json!(false), json!("relative"), json!("")] {
        save_ui_preferences(&path, &json!({"lastProject":bad})).unwrap();
        assert_eq!(
            load(&path).unwrap_err().message().key,
            "desktop.restore_invalid"
        );
    }
    std::fs::write(&path, "{bad json").unwrap();
    assert!(load(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{bad json");
    std::fs::remove_dir_all(root).unwrap();
}
