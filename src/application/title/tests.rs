use super::*;
use crate::plugins::services::UtilityFailure;
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
    ) -> Result<crate::plugins::services::GeneratedTitle, UtilityFailure> {
        let mut calls = self.0.lock().unwrap();
        calls.push(conversation.into());
        Ok(crate::plugins::services::GeneratedTitle {
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
    let evidence =
        std::env::temp_dir().join(format!("clat-title-evidence-{}", uuid::Uuid::new_v4()));
    completed_turn(&sessions, 1, "original topic");
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers, &evidence);
    assert_eq!(sessions.title_state().0.as_deref(), Some("topic 1"));
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers, &evidence);
    assert_eq!(
        titler.0.lock().unwrap().len(),
        1,
        "duplicate notifications for the same turn must not call the model twice"
    );
    for turn in 2..=6 {
        completed_turn(&sessions, turn, "changed topic");
    }
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers, &evidence);
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
    maybe_autotitle(&titler, &sessions, &job, &cancel, &subscribers, &evidence);
    assert_eq!(
        titler.0.lock().unwrap().len(),
        2,
        "manual title stops model calls, not only writes"
    );
    sessions.quiesce_active().unwrap();
    // 成功路径（含 CAS 正常拒绝）不写失败证据。
    assert!(!evidence.join("utility-log.jsonl").exists());
    drop(evidence);
    crate::test_support::cleanup_tree(&root);
}

/// TITLE-CUT-1（2026-10-18）：真实 SessionTitlePlugin 链（罐头模型 → 清洗 →
/// journal 落库）下，合规中文标题必须全值持久化并在全新 SessionService 的
/// resume 回放中原样读出；超限且无合规前缀（单个 64 字符 token）的输出不得
/// 覆盖既有标题，并留下有界 evidence。旧码 16 chars 截断在本测试两腿皆红。
#[test]
fn generated_titles_persist_whole_and_replay_whole_overflow_keeps_old_title() {
    use crate::model::{
        FinishReason, Model, ModelError, ModelEventSink, ModelFactory, ModelProtocol, ModelRequest,
        ModelResponse,
    };
    use crate::plugin::{PluginId, PluginManager, PluginOwner};
    use crate::plugins::services::{PROVIDER_SERVICE, ProviderLease, SESSION_TITLE_SERVICE};
    use crate::plugins::{CompanionUtilityPlugin, ProviderRegistryPlugin, SessionTitlePlugin};
    use crate::session::key::SessionKey;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SequencedFactory(Arc<AtomicUsize>, Vec<String>);
    impl ModelFactory for SequencedFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }
        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for title persistence tests")
        }
        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(SequencedModel(
                Arc::clone(&self.0),
                self.1.clone(),
            )))
        }
    }
    struct SequencedModel(Arc<AtomicUsize>, Vec<String>);
    impl Model for SequencedModel {
        fn provider(&self) -> &str {
            "title-seq"
        }
        fn model_id(&self) -> &str {
            "title-seq"
        }
        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            let index = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelResponse {
                text: self.1[index].clone(),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            })
        }
    }

    fn plugin_titler(factory: SequencedFactory) -> Arc<dyn SessionTitler> {
        let mut manager = PluginManager::root(crate::plugin::ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ProviderRegistryPlugin),
                Arc::new(CompanionUtilityPlugin),
                Arc::new(SessionTitlePlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease: ProviderLease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.titlecut")),
                Arc::new(factory),
            )
            .map_err(|error| error.to_string())
            .expect("register");
        manager.require(SESSION_TITLE_SERVICE).expect("titler")
    }

    let root = std::env::temp_dir().join(format!("clat-titlecut-{}", uuid::Uuid::new_v4()));
    let evidence = std::env::temp_dir().join(format!("clat-titlecut-ev-{}", uuid::Uuid::new_v4()));
    // evidence 写入不建父目录（R1-F4：无处可写即丢行），测试侧先建目录。
    std::fs::create_dir_all(&evidence).expect("evidence dir");
    let sessions = SessionService::new(root.clone(), JsonlCompression::Zstd).unwrap();
    let project = ProjectKey::from_cwd("/tmp/titlecut");
    let id = sessions.new_session(&project).unwrap().id;
    let config = ModelConfig::default();
    let job = AutotitleJob {
        session_id: id.clone(),
        credentials: ProviderCredentials::for_protocol(config.protocol),
        config,
    };
    let (tx, rx) = mpsc::channel();
    let subscribers = Arc::new(Mutex::new(vec![tx]));
    let cancel = CancelToken::new();

    // 腿 1：合规中文标题（18 汉字 = 36 列 ≤ 48 列上限）全值落库并广播全值。
    let whole = "修复会话标题在十六字符处被截断的问题";
    let titler = plugin_titler(SequencedFactory(
        Arc::new(AtomicUsize::new(0)),
        vec![whole.to_owned(), String::new()],
    ));
    completed_turn(&sessions, 1, "修复会话标题截断问题");
    maybe_autotitle(
        titler.as_ref(),
        &sessions,
        &job,
        &cancel,
        &subscribers,
        &evidence,
    );
    assert_eq!(
        sessions.title_state().0.as_deref(),
        Some(whole),
        "a compliant CJK title must be stored whole, not cut to 16 chars"
    );
    let mut broadcasts = rx.try_iter();
    assert!(matches!(
        broadcasts.next(),
        Some(ApplicationEvent::TitleUpdated { ref title }) if title == whole
    ));
    assert!(
        broadcasts.next().is_none(),
        "one committed update broadcasts once"
    );
    assert!(
        !evidence.join("utility-log.jsonl").exists(),
        "the success path writes no failure evidence"
    );

    // 回放读出：写者退役后用全新 SessionService 重开同一会话。
    sessions.quiesce_active().unwrap();
    let reopened = SessionService::new(root.clone(), JsonlCompression::Zstd).unwrap();
    let view = reopened
        .resume(&SessionKey {
            project: project.clone(),
            id: id.clone(),
        })
        .expect("resume");
    assert_eq!(
        view.title.as_deref(),
        Some(whole),
        "journal replay must reproduce the whole stored title"
    );

    // 腿 2：超限且不可断（单个 64 字符 token）→ 保留旧标题 + evidence。
    let overlong = "a".repeat(64);
    let titler = plugin_titler(SequencedFactory(
        Arc::new(AtomicUsize::new(0)),
        vec![overlong.clone()],
    ));
    for turn in 2..=6 {
        completed_turn(&reopened, turn, "changed topic");
    }
    maybe_autotitle(
        titler.as_ref(),
        &reopened,
        &job,
        &cancel,
        &subscribers,
        &evidence,
    );
    assert_eq!(
        reopened.title_state().0.as_deref(),
        Some(whole),
        "an unbreakable over-limit model output must keep the previous title"
    );
    let log = std::fs::read_to_string(evidence.join("utility-log.jsonl"))
        .expect("rejection leaves a bounded evidence line");
    assert!(log.contains("session-title"), "evidence names the utility");
    assert!(
        log.contains("sanitize"),
        "evidence records the sanitize rejection"
    );
    assert!(
        !log.contains(&overlong),
        "evidence must not echo untrusted model output"
    );

    reopened.quiesce_active().unwrap();
    crate::test_support::cleanup_tree(&root);
    crate::test_support::cleanup_tree(&evidence);
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
        ) -> Result<crate::plugins::services::GeneratedTitle, UtilityFailure> {
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
        &std::env::temp_dir(),
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
