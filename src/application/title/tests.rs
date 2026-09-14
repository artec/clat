use super::*;
use crate::session::event::payloads;
use crate::session::key::ProjectKey;
use crate::session::persistence::JsonlCompression;
use crate::session::run_journal::NewSessionEvent;
use crate::session::use_cases::{SetTitleExpectation, TitleSource};

struct RecordingTitler(Mutex<Vec<String>>);

impl SessionTitler for RecordingTitler {
    fn generate_title(
        &self,
        _: &ModelConfig,
        _: &ProviderCredentials,
        conversation: &str,
        _: &CancelToken,
    ) -> Option<crate::plugins::services::GeneratedTitle> {
        let mut calls = self.0.lock().unwrap();
        calls.push(conversation.into());
        Some(crate::plugins::services::GeneratedTitle {
            title: format!("topic {}", calls.len()),
            provider: "test".into(),
            model: "small".into(),
        })
    }
}

fn completed_turn(sessions: &SessionService, turn: u64, text: &str) {
    let journal = sessions.journal().unwrap();
    journal
        .append_atomic(&[
            NewSessionEvent::new("turn/start", payloads::turn_start(turn)),
            NewSessionEvent::new("user/message", payloads::user_message(text)).append(Vec::new()),
            NewSessionEvent::new(
                "turn/end",
                payloads::turn_end(turn, &crate::session::event::TurnEndReason::Completed),
            ),
        ])
        .unwrap();
    journal.flush().unwrap();
}

#[test]
fn utility_worker_updates_provider_titles_but_manual_rename_ends_model_calls() {
    let root = std::env::temp_dir().join(format!("clat-utility-worker-{}", uuid::Uuid::new_v4()));
    let sessions = SessionService::new(root.clone(), JsonlCompression::Zstd).unwrap();
    let project = ProjectKey::from_cwd("/tmp/utility-worker");
    let id = sessions.new_session(&project).unwrap().id;
    let config = ModelConfig::default();
    let job = AutotitleJob {
        session_id: id.clone(),
        credentials: ProviderCredentials::for_protocol(config.protocol),
        config,
    };
    let titler = RecordingTitler(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let subscribers = Arc::new(Mutex::new(vec![tx]));
    let cancel = CancelToken::new();
    completed_turn(&sessions, 1, "original topic");
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers);
    assert_eq!(sessions.title_state().0.as_deref(), Some("topic 1"));
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers);
    assert_eq!(
        titler.0.lock().unwrap().len(),
        1,
        "queued work cannot bypass the interval"
    );
    for turn in 2..=6 {
        completed_turn(&sessions, turn, "changed topic");
    }
    // Deterministic five-minute advance without changing process clocks.
    let budget = root
        .join(&project.bucket)
        .join(crate::session::path_layout::encode_segment(id.as_str()))
        .join("clat-utility-budget.json");
    std::fs::write(
        &budget,
        r#"{"version":1,"title_attempts":1,"last_title_turn":1,"last_title_ms":0}"#,
    )
    .unwrap();
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers);
    assert_eq!(
        sessions.title_state().0.as_deref(),
        Some("topic 2"),
        "provider title must not stop continuous naming"
    );
    assert!(titler.0.lock().unwrap()[1].contains("changed topic"));
    assert_eq!(
        rx.try_iter().count(),
        2,
        "each committed update broadcasts once"
    );
    sessions
        .set_title(
            &id,
            SetTitleExpectation::Force,
            "my title",
            TitleSource::User,
        )
        .unwrap();
    for turn in 7..=11 {
        completed_turn(&sessions, turn, "another topic");
    }
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers);
    assert_eq!(
        titler.0.lock().unwrap().len(),
        2,
        "manual title stops model calls, not only writes"
    );
    sessions.quiesce_active().unwrap();
    crate::test_support::cleanup_tree(&root);
}

#[test]
fn disabled_naming_never_reserves_budget_or_calls_the_provider() {
    struct DisabledTitler;
    impl SessionTitler for DisabledTitler {
        fn enabled(&self) -> bool {
            false
        }

        fn generate_title(
            &self,
            _: &ModelConfig,
            _: &ProviderCredentials,
            _: &str,
            _: &CancelToken,
        ) -> Option<crate::plugins::services::GeneratedTitle> {
            panic!("disabled naming must stop before provider I/O")
        }
    }

    let root = std::env::temp_dir().join(format!("clat-utility-disabled-{}", uuid::Uuid::new_v4()));
    let sessions = SessionService::new(root.clone(), JsonlCompression::Zstd).unwrap();
    let project = ProjectKey::from_cwd("/tmp/utility-disabled");
    let id = sessions.new_session(&project).unwrap().id;
    completed_turn(&sessions, 1, "no automatic naming");
    let config = ModelConfig::default();
    let job = AutotitleJob {
        session_id: id.clone(),
        credentials: ProviderCredentials::for_protocol(config.protocol),
        config,
    };
    maybe_autotitle(
        &DisabledTitler,
        &sessions,
        &job,
        &CancelToken::new(),
        &Arc::new(Mutex::new(Vec::new())),
    );
    let budget = root
        .join(project.bucket)
        .join(crate::session::path_layout::encode_segment(id.as_str()))
        .join("clat-utility-budget.json");
    assert!(
        !budget.exists(),
        "disabled naming must not consume an attempt"
    );
    sessions.quiesce_active().unwrap();
    crate::test_support::cleanup_tree(&root);
}
