//! Shared remote bridge policies; transport-specific reply routes stay outside.
use super::state::ServeShared;
use crate::PermissionMode;
use crate::i18n::UiMessage;
use crate::model::CancelToken;
use crate::permission::{PermissionApprover, PermissionDecision, PermissionRequest};
use crate::remote::{
    ApprovalAnswer, ApprovalRegistry, ApprovalScope, ApprovalWait, Command, RemoteConversations,
    RemoteDeliveryId, RemotePrincipal, RemoteRoute, approval_prompt, wait_for_approval,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub(super) struct RemoteLocale(Arc<dyn Fn() -> crate::i18n::LocaleContext + Send + Sync>);

impl RemoteLocale {
    pub(super) fn fixed(locale: crate::i18n::LocaleContext) -> Self {
        Self(Arc::new(move || locale))
    }

    pub(super) fn terminal(explicit: Option<String>, fallback: crate::i18n::LocaleContext) -> Self {
        Self::terminal_with_preferences(
            explicit,
            std::env::var("CLAT_LANG").ok(),
            fallback,
            Arc::new(|| {
                crate::i18n::terminal_preferences_path()
                    .and_then(|p| crate::i18n::load_preference(&p))
            }),
        )
    }

    fn terminal_with_preferences(
        explicit: Option<String>,
        environment: Option<String>,
        fallback: crate::i18n::LocaleContext,
        read: Arc<dyn Fn() -> Result<crate::i18n::LanguagePreference, String> + Send + Sync>,
    ) -> Self {
        Self(Arc::new(move || {
            crate::i18n::resolve_terminal_sources(
                explicit.as_deref(),
                environment.as_deref(),
                || read(),
            )
            .map(|language| language.context)
            .unwrap_or(fallback)
        }))
    }

    pub(super) fn current(&self) -> crate::i18n::LocaleContext {
        (self.0)()
    }
}

pub(super) enum DeliveryResult {
    Handled,
    Duplicate,
    Rejected,
    Retry(String),
}

pub(super) struct RemoteCommands<'a> {
    pub(super) shared: &'a ServeShared,
    pub(super) route: &'a RemoteRoute,
    pub(super) conversations: &'a RemoteConversations,
    pub(super) approvals: &'a ApprovalRegistry,
    pub(super) notify: &'a dyn Fn(UiMessage),
}

impl RemoteCommands<'_> {
    pub(super) fn handle(&self, action: Command<'_>) -> Result<(), String> {
        match action {
            Command::Approval(answer, id) => self.approval(id, answer),
            Command::Help => {
                (self.notify)(UiMessage::new("remote.help"));
                Ok(())
            }
            Command::Unknown => {
                (self.notify)(UiMessage::new("remote.unknown_command"));
                Ok(())
            }
            Command::Status => self.status(),
            Command::New => self.new_chat(),
            Command::Stop => self.stop(),
        }
    }

    fn approval(&self, id: &str, answer: ApprovalAnswer) -> Result<(), String> {
        let Some(scope) = approval_scope(self.shared, self.route) else {
            return Ok(());
        };
        if self
            .approvals
            .take(&scope, id)
            .is_none_or(|entry| entry.decision_tx.send(answer).is_err())
        {
            (self.notify)(UiMessage::new("remote.approval_missing"));
        }
        Ok(())
    }

    fn status(&self) -> Result<(), String> {
        let active = !self.shared.active_run_info().is_null();
        let fallback = self.conversations.snapshot(self.route)?.1;
        let status = self
            .shared
            .app
            .lock()
            .expect("application lock")
            .remote_chat_status(
                self.route.channel,
                &self.route.user,
                &self.route.chat,
                fallback.as_ref(),
            );
        let state = match status {
            crate::application::RemoteChatStatus::MappedCurrent { title } => {
                UiMessage::new("remote.status_current").arg(
                    "title",
                    title
                        .map(|value| {
                            let value = crate::redact::redact_secrets(&value);
                            format!(": {}", value.chars().take(300).collect::<String>())
                        })
                        .unwrap_or_default(),
                )
            }
            crate::application::RemoteChatStatus::MappedInactive => {
                UiMessage::new("remote.status_inactive")
            }
            crate::application::RemoteChatStatus::Unmapped => {
                UiMessage::new("remote.status_unmapped")
            }
            crate::application::RemoteChatStatus::Revoked => return Ok(()),
        };
        let run = UiMessage::new(if active {
            "remote.status_active"
        } else {
            "remote.status_idle"
        });
        (self.notify)(
            UiMessage::new("remote.status")
                .arg_message("state", state)
                .arg_message("run", run),
        );
        Ok(())
    }

    fn new_chat(&self) -> Result<(), String> {
        let claim_id = format!("remote-new-{}", uuid::Uuid::new_v4());
        if !self.shared.try_claim_run(&claim_id, super::state::now_ms()) {
            (self.notify)(UiMessage::new("remote.busy_new"));
            return Err("remote run slot busy; delivery remains retryable".into());
        }
        let outcome = {
            let mut app = self.shared.app.lock().expect("application lock");
            let outcome =
                app.begin_new_remote_chat(self.route.channel, &self.route.user, &self.route.chat);
            if outcome
                .as_ref()
                .is_ok_and(|value| matches!(value, crate::application::RemoteNewChatOutcome::Ready))
                || outcome
                    .as_ref()
                    .is_err_and(crate::ApplicationError::selection_changed)
            {
                self.shared.advance_selection_generation();
            }
            outcome
        };
        self.shared.release_run_claim();
        let outcome = outcome.map_err(|error| error.to_string())?;
        if matches!(outcome, crate::application::RemoteNewChatOutcome::Ready) {
            self.conversations.arm(self.route.clone())?;
            (self.notify)(UiMessage::new("remote.new_ready"));
        }
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        let fallback = self.conversations.snapshot(self.route)?.1;
        let stopped = {
            let app = self.shared.app.lock().expect("application lock");
            if app.remote_chat_owns_current_session(
                self.route.channel,
                &self.route.user,
                &self.route.chat,
                fallback.as_ref(),
            ) {
                app.cancel_active_run();
                true
            } else {
                false
            }
        };
        (self.notify)(UiMessage::new(if stopped {
            "remote.stop_requested"
        } else {
            "remote.stop_no_run"
        }));
        Ok(())
    }
}

/// Classify and acknowledge one normalized delivery around the shared action.
/// The adapter owns transport acknowledgement and only advances after a
/// terminal result; retry preserves the original delivery for redelivery.
pub(super) fn dispatch_delivery<'a>(
    shared: &ServeShared,
    route: &RemoteRoute,
    delivery_id: &str,
    action: Option<Command<'a>>,
    handle: impl FnOnce(Option<Command<'a>>) -> Result<(), String>,
) -> DeliveryResult {
    let delivery_id = RemoteDeliveryId::from(delivery_id);
    let disposition = shared
        .app
        .lock()
        .expect("application lock")
        .classify_remote_delivery(route.channel, &route.user, &delivery_id);
    match disposition {
        crate::application::RemoteDeliveryDisposition::Unauthorized => {
            return DeliveryResult::Rejected;
        }
        crate::application::RemoteDeliveryDisposition::Duplicate => {
            return DeliveryResult::Duplicate;
        }
        crate::application::RemoteDeliveryDisposition::Accept => {}
    }
    if let Err(error) = handle(action) {
        return DeliveryResult::Retry(error);
    }
    match shared
        .app
        .lock()
        .expect("application lock")
        .commit_remote_delivery(route.channel, &delivery_id)
    {
        Ok(()) => DeliveryResult::Handled,
        Err(error) => DeliveryResult::Retry(error.to_string()),
    }
}

pub(super) fn approval_scope(shared: &ServeShared, route: &RemoteRoute) -> Option<ApprovalScope> {
    let app = shared.app.lock().ok()?;
    if !app.remote_user_is_authorized(route.channel, &route.user)
        || !app.remote_run_owned_by(route.channel, &route.user, &route.chat)
    {
        return None;
    }
    let session = app.current_session_id()?.to_string();
    let active = shared.active_run_info();
    let run = active.get("prompt_rpc_id")?.as_str()?.to_owned();
    Some(ApprovalScope {
        principal: RemotePrincipal {
            channel: route.channel,
            user: route.user.clone(),
        },
        chat: route.chat.clone(),
        session,
        run,
    })
}

pub(super) struct RemoteApprover {
    pub(super) shared: Arc<ServeShared>,
    pub(super) route: RemoteRoute,
    pub(super) approvals: Arc<ApprovalRegistry>,
    pub(super) notify: Arc<dyn Fn(UiMessage) + Send + Sync>,
    pub(super) run_id: String,
}

impl RemoteApprover {
    fn allow_always(&self, scope: &ApprovalScope, cancel: &CancelToken) -> PermissionDecision {
        let persisted = {
            let app = self.shared.app.lock().expect("application lock");
            let active = self.shared.active_run_info();
            let current = app.current_session_id();
            let valid = !cancel.is_cancelled()
                && app.remote_user_is_authorized(self.route.channel, &self.route.user)
                && app.remote_run_owned_by(self.route.channel, &self.route.user, &self.route.chat)
                && current
                    .as_ref()
                    .is_some_and(|id| id.as_str() == scope.session)
                && active.get("prompt_rpc_id").and_then(|id| id.as_str())
                    == Some(scope.run.as_str());
            if !valid {
                return PermissionDecision::Deny {
                    reason: "remote approval run is no longer active".into(),
                };
            }
            app.set_permission_mode(PermissionMode::FullAccess)
        };
        match persisted {
            Ok(()) => {
                (self.notify)(UiMessage::new("remote.always_allowed"));
                PermissionDecision::Allow
            }
            Err(error) => {
                let key = if error.to_string().contains("outcome unknown") {
                    "remote.always_uncertain"
                } else {
                    "remote.always_failed"
                };
                (self.notify)(UiMessage::new(key));
                PermissionDecision::Deny {
                    reason: "Full Access escalation did not commit".into(),
                }
            }
        }
    }
}

impl PermissionApprover for RemoteApprover {
    fn decide(&self, request: PermissionRequest, cancel: &CancelToken) -> PermissionDecision {
        let Some(scope) =
            approval_scope(&self.shared, &self.route).filter(|scope| scope.run == self.run_id)
        else {
            return PermissionDecision::Deny {
                reason: "remote approval run is no longer active".into(),
            };
        };
        let (decision_tx, decision_rx) = std::sync::mpsc::channel();
        let request_id = self
            .approvals
            .insert(scope.clone(), decision_tx, APPROVAL_TIMEOUT);
        (self.notify)(approval_prompt(&request, &request_id));
        let answer = wait_for_approval(&decision_rx, cancel, Instant::now() + APPROVAL_TIMEOUT);
        self.approvals.remove(&request_id);
        if matches!(answer, ApprovalWait::Answer(_))
            && (cancel.is_cancelled()
                || approval_scope(&self.shared, &self.route).as_ref() != Some(&scope))
        {
            return PermissionDecision::Deny {
                reason: "remote approval run is no longer active".into(),
            };
        }
        match answer {
            ApprovalWait::Answer(ApprovalAnswer::Allow) => {
                (self.notify)(UiMessage::new("remote.approval_submitted"));
                PermissionDecision::Allow
            }
            ApprovalWait::Answer(ApprovalAnswer::Always) => self.allow_always(&scope, cancel),
            ApprovalWait::Answer(ApprovalAnswer::Deny) => PermissionDecision::Deny {
                reason: "denied from the remote frontend".into(),
            },
            ApprovalWait::Timeout => {
                (self.notify)(UiMessage::new("remote.approval_timeout").arg("id", request_id));
                PermissionDecision::Deny {
                    reason: "approval timeout".into(),
                }
            }
            ApprovalWait::Cancelled => PermissionDecision::Deny {
                reason: "run cancelled".into(),
            },
            ApprovalWait::Disconnected => PermissionDecision::Deny {
                reason: "no permission decision available".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::RemoteChannel;
    use crate::test_support::{TestBehavior, TestProviderPlugin, configure_test_model, roots};
    use std::sync::{Mutex, mpsc};

    #[test]
    fn locale_provider_resolves_when_message_is_rendered() {
        use crate::i18n::{LanguagePreference, Locale, LocaleContext};
        let (storage, _unused_project) = roots("remote-locale-live");
        let path = storage.join("ui/terminal.json");
        let read_path = path.clone();
        let read = Arc::new(move || crate::i18n::load_preference(&read_path));
        crate::i18n::save_preference(&path, LanguagePreference::Fixed(Locale::English)).unwrap();
        let locale = RemoteLocale::terminal_with_preferences(
            None,
            None,
            LocaleContext::default(),
            read.clone(),
        );
        let notice = UiMessage::new("remote.empty_input");
        let before = locale.current().render(&notice);
        crate::i18n::save_preference(&path, LanguagePreference::Fixed(Locale::Simplified)).unwrap();
        let after = locale.current().render(&notice);
        assert_ne!(
            before, after,
            "queued notices use the current Host language"
        );
        let explicit = RemoteLocale::terminal_with_preferences(
            Some("en".into()),
            None,
            LocaleContext::default(),
            read,
        );
        assert_eq!(explicit.current().locale, Locale::English);
        crate::test_support::cleanup_tree(&storage);
    }

    #[test]
    fn test_channel_uses_shared_approval_and_rejects_another_channel() {
        let (storage, project_root) = roots("remote-approval-second-channel");
        std::fs::create_dir_all(&project_root).unwrap();
        let app =
            crate::BootstrapApplication::open(crate::Project::new(&project_root), storage.clone())
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::RunCommandNetwork,
                }))
                .unwrap();
        configure_test_model(&app);
        app.authorize_test_remote("user");
        let app = Arc::new(Mutex::new(app));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let approvals = Arc::new(ApprovalRegistry::default());
        let route = RemoteRoute {
            channel: RemoteChannel::Test,
            user: "user".into(),
            chat: "chat".into(),
        };
        let conversations = RemoteConversations::default();
        let notices = Mutex::new(Vec::new());
        let notify = |notice| notices.lock().unwrap().push(notice);
        let commands = RemoteCommands {
            shared: &shared,
            route: &route,
            conversations: &conversations,
            approvals: &approvals,
            notify: &notify,
        };
        assert!(matches!(
            dispatch_delivery(&shared, &route, "retry", None, |_| Err("busy".into())),
            DeliveryResult::Retry(_)
        ));
        assert!(matches!(
            dispatch_delivery(&shared, &route, "retry", Some(Command::Status), |action| {
                commands.handle(action.unwrap())
            }),
            DeliveryResult::Handled
        ));
        assert_eq!(notices.lock().unwrap().last().unwrap().key, "remote.status");
        assert!(matches!(
            dispatch_delivery(&shared, &route, "new", Some(Command::New), |action| {
                commands.handle(action.unwrap())
            }),
            DeliveryResult::Handled
        ));
        assert!(conversations.snapshot(&route).unwrap().0);
        assert!(matches!(
            dispatch_delivery(&shared, &route, "retry", None, |_| panic!(
                "duplicate was executed"
            )),
            DeliveryResult::Duplicate
        ));
        let foreign_route = RemoteRoute {
            channel: RemoteChannel::Wechat,
            ..route.clone()
        };
        assert!(matches!(
            dispatch_delivery(&shared, &foreign_route, "retry", None, |_| panic!(
                "unauthorized action was executed"
            )),
            DeliveryResult::Rejected
        ));
        let (notice_tx, notice_rx) = mpsc::channel::<UiMessage>();
        let shutdown = std::sync::atomic::AtomicBool::new(false);
        let prompts = super::super::remote_prompt::RemotePrompts {
            shared: &shared,
            route: &route,
            conversations: &conversations,
            approvals: &approvals,
            shutdown: &shutdown,
            notify: Arc::new(move |notice| {
                let _ = notice_tx.send(notice);
            }),
            text: Arc::new(|_| {}),
            typing: Arc::new(|_| {}),
        };
        assert!(matches!(
            dispatch_delivery(&shared, &route, "message", None, |_| {
                let ticket = prompts.prepare("message")?.expect("authorized and armed");
                prompts.submit(ticket, "message", "run task".into(), Vec::new())
            }),
            DeliveryResult::Handled
        ));
        let notice = notice_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let id = notice
            .args
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap();
        assert!(matches!(
            dispatch_delivery(&shared, &route, "busy-new", Some(Command::New), |action| {
                commands.handle(action.unwrap())
            }),
            DeliveryResult::Retry(_)
        ));
        let scope = approval_scope(&shared, &route).expect("active owned approval");
        let mut foreign = scope.clone();
        foreign.principal.channel = RemoteChannel::Wechat;
        assert!(approvals.take(&foreign, id).is_none());
        assert!(matches!(
            dispatch_delivery(
                &shared,
                &route,
                "answer",
                Some(Command::Approval(ApprovalAnswer::Deny, id)),
                |action| { commands.handle(action.unwrap()) }
            ),
            DeliveryResult::Handled
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null(), "run must settle");
        assert!(matches!(
            dispatch_delivery(&shared, &route, "busy-new", Some(Command::New), |action| {
                commands.handle(action.unwrap())
            }),
            DeliveryResult::Handled
        ));
        assert!(matches!(
            dispatch_delivery(&shared, &route, "message-2", None, |_| {
                let ticket = prompts.prepare("message-2")?.expect("new chat is armed");
                prompts.submit(ticket, "message-2", "run again".into(), Vec::new())
            }),
            DeliveryResult::Handled
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let notice = notice_rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if notice.key == "remote.approval_request" {
                break;
            }
        }
        assert!(matches!(
            dispatch_delivery(&shared, &route, "stop", Some(Command::Stop), |action| {
                commands.handle(action.unwrap())
            }),
            DeliveryResult::Handled
        ));
        assert_eq!(
            notices.lock().unwrap().last().unwrap().key,
            "remote.stop_requested"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            shared.active_run_info().is_null(),
            "stopped run must settle"
        );
        for (index, answer) in [ApprovalAnswer::Allow, ApprovalAnswer::Always]
            .into_iter()
            .enumerate()
        {
            let new_id = format!("next-new-{index}");
            assert!(matches!(
                dispatch_delivery(&shared, &route, &new_id, Some(Command::New), |action| {
                    commands.handle(action.unwrap())
                }),
                DeliveryResult::Handled
            ));
            let message_id = format!("next-message-{index}");
            assert!(matches!(
                dispatch_delivery(&shared, &route, &message_id, None, |_| {
                    let ticket = prompts.prepare(&message_id)?.expect("new chat is armed");
                    prompts.submit(ticket, &message_id, "run task".into(), Vec::new())
                }),
                DeliveryResult::Handled
            ));
            let deadline = Instant::now() + Duration::from_secs(10);
            let approval = loop {
                let notice = notice_rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                if notice.key == "remote.approval_request" {
                    break notice;
                }
            };
            let request_id = approval
                .args
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap();
            let answer_id = format!("next-answer-{index}");
            assert!(matches!(
                dispatch_delivery(
                    &shared,
                    &route,
                    &answer_id,
                    Some(Command::Approval(answer, request_id)),
                    |action| commands.handle(action.unwrap())
                ),
                DeliveryResult::Handled
            ));
            while !shared.active_run_info().is_null() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(shared.active_run_info().is_null());
            let expected = if answer == ApprovalAnswer::Always {
                "remote.always_allowed"
            } else {
                "remote.approval_submitted"
            };
            assert!(notice_rx.try_iter().any(|notice| notice.key == expected));
            if answer == ApprovalAnswer::Always {
                assert_eq!(
                    app.lock().unwrap().permission_mode(),
                    PermissionMode::FullAccess
                );
            }
        }
        shared.mark_shutting_down();
        shared.drain_workers();
        drop(approvals);
        drop(shared);
        let application = Arc::try_unwrap(app).ok().unwrap().into_inner().unwrap();
        application.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }
}
