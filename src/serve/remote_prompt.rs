//! Remote single-chat admission, run coordination and recovery.
use super::remote::RemoteApprover;
use super::remote_steering::RemoteSteering;
use super::state::ServeShared;
use crate::ApplicationRunRequest;
use crate::application::{RemoteChatReadiness, RemoteChatTicket, RemotePromptStartOutcome};
use crate::i18n::UiMessage;
use crate::message::PendingMessage;
use crate::remote::{ApprovalRegistry, RemoteConversations, RemoteRoute, RemoteRunSink};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicBool, mpsc};

pub(super) struct RemotePrompts<'a> {
    pub(super) shared: &'a Arc<ServeShared>,
    pub(super) route: &'a RemoteRoute,
    pub(super) conversations: &'a RemoteConversations,
    pub(super) approvals: &'a Arc<ApprovalRegistry>,
    pub(super) shutdown: &'a AtomicBool,
    pub(super) notify: Arc<dyn Fn(UiMessage) + Send + Sync>,
    pub(super) text: Arc<dyn Fn(String) + Send + Sync>,
    pub(super) typing: Arc<dyn Fn(bool) + Send + Sync>,
}

/// A staged image can only be constructed from bytes admitted by the host.
pub(super) struct RemoteAttachment {
    opaque: PathBuf,
    staged: PathBuf,
    drafts: crate::application::DraftImages,
}

impl Drop for RemoteAttachment {
    fn drop(&mut self) {
        let _ = self.drafts.release_clipboard_path(&self.staged);
    }
}

enum StartResult {
    Accepted,
    Duplicate,
    Revoked,
    Unmapped,
    Conflict,
    MappingPending { error: String, claim_active: bool },
    Failed { error: String, retry_delivery: bool },
}

impl RemotePrompts<'_> {
    pub(super) fn stage_image(
        &self,
        delivery: &str,
        index: usize,
        bytes: &[u8],
        extension: &str,
    ) -> Result<RemoteAttachment, String> {
        self.shared
            .app
            .lock()
            .expect("application lock")
            .ensure_image_input_available()
            .map_err(|error| error.to_string())?;
        let staged = self.shared.drafts.stage_remote_image(bytes, extension)?;
        let digest = Sha256::digest(bytes);
        let delivery = self.route.channel.delivery_key(delivery);
        let opaque = PathBuf::from(format!(
            "{}-{delivery}-{index}-{digest:x}.{extension}",
            self.route.channel.run_prefix()
        ));
        Ok(RemoteAttachment {
            opaque,
            staged,
            drafts: self.shared.drafts.clone(),
        })
    }

    pub(super) fn prepare(&self, delivery: &str) -> Result<Option<RemoteChatTicket>, String> {
        let (armed, fallback) = self.conversations.snapshot(self.route)?;
        let readiness = self
            .shared
            .app
            .lock()
            .expect("application lock")
            .prepare_remote_chat(
                self.route.channel,
                &self.route.user,
                &self.route.chat,
                delivery,
                fallback,
                armed,
            );
        match readiness {
            RemoteChatReadiness::Ready(ticket) => Ok(Some(ticket)),
            RemoteChatReadiness::Unmapped => {
                (self.notify)(UiMessage::new("remote.chat_unmapped"));
                Ok(None)
            }
            RemoteChatReadiness::Revoked => Ok(None),
        }
    }

    pub(super) fn submit(
        &self,
        ticket: RemoteChatTicket,
        delivery: &str,
        text: String,
        staged: Vec<RemoteAttachment>,
    ) -> Result<(), String> {
        if text.trim().is_empty() && staged.is_empty() {
            (self.notify)(UiMessage::new("remote.empty_input"));
            return Ok(());
        }
        let opaque = staged.iter().map(|image| image.opaque.clone()).collect();
        let paths = staged.iter().map(|image| image.staged.clone()).collect();
        let mut pending = PendingMessage::from_front_end(text, Some(delivery.to_owned()), opaque);
        pending.resolve_staged_attachments(paths);
        if !ticket.accepts_digest(&pending.request_digest()) {
            (self.notify)(UiMessage::new("remote.delivery_conflict"));
            return Ok(());
        }
        self.submit_ready(ticket, pending, staged, delivery)
    }

    fn submit_ready(
        &self,
        ticket: RemoteChatTicket,
        pending: PendingMessage,
        staged: Vec<RemoteAttachment>,
        delivery: &str,
    ) -> Result<(), String> {
        let recovering = ticket.recovery_waits_for_idle();
        if !self.shared.active_run_info().is_null() {
            if recovering {
                return Err(
                    "durable chat-mapping recovery waits for the active run to finish".into(),
                );
            }
            return RemoteSteering {
                shared: self.shared,
                route: self.route,
                conversations: self.conversations,
                shutdown: self.shutdown,
                notify: &*self.notify,
            }
            .submit(&ticket, pending, staged, delivery);
        }
        let rpc_id = format!(
            "{}-{}",
            self.route.channel.run_prefix(),
            uuid::Uuid::new_v4()
        );
        if !self.shared.try_claim_run(&rpc_id, super::state::now_ms()) {
            if recovering {
                return Err(
                    "durable chat-mapping recovery waits for the active run to finish".into(),
                );
            }
            (self.notify)(UiMessage::new("remote.busy_retry"));
            return Err("remote run slot busy; delivery remains retryable".into());
        }
        let result = self.start(ticket, pending, delivery, &rpc_id);
        drop(staged);
        self.finish_start(result)
    }

    fn start(
        &self,
        ticket: RemoteChatTicket,
        message: PendingMessage,
        delivery: &str,
        run_id: &str,
    ) -> Result<StartResult, String> {
        let (completion, received) = mpsc::channel();
        let approver = Arc::new(RemoteApprover {
            shared: Arc::clone(self.shared),
            route: self.route.clone(),
            approvals: Arc::clone(self.approvals),
            notify: Arc::clone(&self.notify),
            run_id: run_id.to_owned(),
        });
        let started = {
            let mut app = self.shared.app.lock().expect("application lock");
            let started = app.start_remote_prompt(
                ticket,
                ApplicationRunRequest {
                    message,
                    approver,
                    asker: None,
                    events: Box::new(self.run_sink()),
                    completion,
                },
            );
            if let RemotePromptStartOutcome::Started { handle, .. } = &started.outcome {
                self.shared.publish_run_boundary(handle);
            }
            if started.selection_changed {
                self.shared.advance_selection_generation();
            }
            started
        };
        self.started(delivery, received, started.outcome)
    }

    fn run_sink(&self) -> RemoteRunSink {
        let shared = Arc::clone(self.shared);
        let text = Arc::clone(&self.text);
        let typing = Arc::clone(&self.typing);
        let notice = Arc::clone(&self.notify);
        RemoteRunSink {
            fanout: Box::new(move |event| shared.fanout_run_event(event)),
            text: Box::new(move |value| text(value)),
            notice: Box::new(move |value| notice(value)),
            typing: Box::new(move |active| typing(active)),
        }
    }

    fn started(
        &self,
        delivery: &str,
        received: mpsc::Receiver<crate::ApplicationRunResult>,
        outcome: RemotePromptStartOutcome,
    ) -> Result<StartResult, String> {
        match outcome {
            RemotePromptStartOutcome::Started {
                handle,
                binding,
                mapping_error,
            } => {
                (self.typing)(true);
                self.shared.spawn_settler(
                    format!(
                        "{}:{}",
                        self.route.channel.run_prefix(),
                        self.route.channel.delivery_key(delivery)
                    ),
                    received,
                    *handle,
                );
                if let Some(error) = mapping_error {
                    let error = match self
                        .conversations
                        .retain_fallback(self.route.clone(), binding)
                    {
                        Ok(()) => error,
                        Err(fallback_error) => {
                            format!("{error}; could not retain temporary mapping: {fallback_error}")
                        }
                    };
                    return Ok(StartResult::MappingPending {
                        error: format!("could not persist chat/session mapping: {error}"),
                        claim_active: true,
                    });
                }
                if let Err(error) = self.conversations.clear(self.route) {
                    return Ok(StartResult::MappingPending {
                        error,
                        claim_active: true,
                    });
                }
                Ok(StartResult::Accepted)
            }
            RemotePromptStartOutcome::Duplicate => {
                self.shared.release_run_claim();
                self.conversations.clear_fallback(self.route)?;
                (self.notify)(UiMessage::new("remote.duplicate"));
                Ok(StartResult::Duplicate)
            }
            RemotePromptStartOutcome::Revoked => Ok(StartResult::Revoked),
            RemotePromptStartOutcome::Unmapped => Ok(StartResult::Unmapped),
            RemotePromptStartOutcome::Conflict => Ok(StartResult::Conflict),
            RemotePromptStartOutcome::MappingPending { error } => Ok(StartResult::MappingPending {
                error,
                claim_active: false,
            }),
            RemotePromptStartOutcome::Failed {
                error,
                retry_delivery,
            } => Ok(StartResult::Failed {
                error,
                retry_delivery,
            }),
        }
    }

    fn finish_start(&self, result: Result<StartResult, String>) -> Result<(), String> {
        match result {
            Ok(StartResult::Accepted | StartResult::Duplicate) => Ok(()),
            Ok(StartResult::Revoked) => {
                self.shared.release_run_claim();
                Ok(())
            }
            Ok(StartResult::Unmapped) => {
                self.shared.release_run_claim();
                (self.notify)(UiMessage::new("remote.chat_unmapped"));
                Ok(())
            }
            Ok(StartResult::Conflict) => {
                self.shared.release_run_claim();
                (self.notify)(UiMessage::new("remote.delivery_conflict"));
                Ok(())
            }
            Ok(StartResult::MappingPending {
                error,
                claim_active,
            }) => {
                if !claim_active {
                    self.shared.release_run_claim();
                }
                (self.notify)(UiMessage::new("remote.mapping_pending"));
                Err(error)
            }
            Ok(StartResult::Failed {
                error,
                retry_delivery,
            }) => {
                self.shared.release_run_claim();
                (self.notify)(
                    UiMessage::new("remote.not_started").arg("detail", bounded_summary(&error)),
                );
                if retry_delivery { Err(error) } else { Ok(()) }
            }
            Err(error) => {
                self.shared.release_run_claim();
                Err(error)
            }
        }
    }
}

fn bounded_summary(text: &str) -> String {
    let mut output = text.chars().take(300).collect::<String>();
    if text.chars().count() > 300 {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::RemoteChannel;
    use crate::serve::remote::{DeliveryResult, dispatch_delivery};
    use crate::test_support::{
        SteerGate, TestBehavior, TestProviderPlugin, configure_test_model, roots,
    };
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[test]
    fn second_channel_recovers_committed_delivery_after_restart() {
        let (storage, project_root) = roots("remote-test-recovery");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = crate::Project::new(&project_root);
        let mount = || {
            crate::BootstrapApplication::open(project.clone(), storage.clone())
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Success,
                }))
                .unwrap()
        };
        let app = mount();
        configure_test_model(&app);
        app.authorize_test_remote("user");
        app.inject_next_wechat_chat_mapping_save_failure();
        let app = Arc::new(Mutex::new(app));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let route = RemoteRoute {
            channel: RemoteChannel::Test,
            user: "user".into(),
            chat: "chat".into(),
        };
        let conversations = RemoteConversations::default();
        conversations.arm(route.clone()).unwrap();
        let approvals = Arc::new(ApprovalRegistry::default());
        let shutdown = AtomicBool::new(false);
        let bridge = RemotePrompts {
            shared: &shared,
            route: &route,
            conversations: &conversations,
            approvals: &approvals,
            shutdown: &shutdown,
            notify: Arc::new(|_| {}),
            text: Arc::new(|_| {}),
            typing: Arc::new(|_| {}),
        };
        assert!(matches!(
            dispatch_delivery(&shared, &route, "same", None, |_| {
                let ticket = bridge.prepare("same")?.unwrap();
                bridge.submit(ticket, "same", "persist once".into(), Vec::new())
            }),
            DeliveryResult::Retry(_)
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        let session = app.lock().unwrap().current_session_id().unwrap();
        assert!(
            app.lock()
                .unwrap()
                .inspect_test_remote_binding("chat")
                .is_none()
        );
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&route.channel.delivery_key("same"))
                .is_some()
        );
        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        drop(approvals);
        drop(shared);
        Arc::try_unwrap(app)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap()
            .close()
            .unwrap();

        let mut reopened = mount();
        configure_test_model(&reopened);
        reopened.new_session().unwrap();
        let app = Arc::new(Mutex::new(reopened));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let conversations = RemoteConversations::default();
        let approvals = Arc::new(ApprovalRegistry::default());
        let bridge = RemotePrompts {
            shared: &shared,
            route: &route,
            conversations: &conversations,
            approvals: &approvals,
            shutdown: &shutdown,
            notify: Arc::new(|_| {}),
            text: Arc::new(|_| {}),
            typing: Arc::new(|_| {}),
        };
        assert!(matches!(
            dispatch_delivery(&shared, &route, "same", None, |_| {
                let ticket = bridge
                    .prepare("same")?
                    .expect("pending intent survives restart");
                bridge.submit(ticket, "same", "persist once".into(), Vec::new())
            }),
            DeliveryResult::Handled
        ));
        assert_eq!(
            app.lock()
                .unwrap()
                .inspect_test_remote_binding("chat")
                .unwrap()
                .session_id,
            session
        );
        assert_eq!(app.lock().unwrap().current_session_id(), Some(session));
        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        drop(approvals);
        drop(shared);
        Arc::try_unwrap(app)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap()
            .close()
            .unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }

    #[test]
    fn second_channel_steering_waits_for_its_scoped_durable_receipt() {
        let (storage, project_root) = roots("remote-test-steering");
        std::fs::create_dir_all(&project_root).unwrap();
        let gate = Arc::new(SteerGate::default());
        let app =
            crate::BootstrapApplication::open(crate::Project::new(&project_root), storage.clone())
                .unwrap()
                .with_permission_modes()
                .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Steer(Arc::clone(&gate)),
                }))
                .unwrap();
        configure_test_model(&app);
        app.authorize_test_remote("user");
        let app = Arc::new(Mutex::new(app));
        let shared = Arc::new(ServeShared::new(Arc::clone(&app), "token".into(), 0));
        let route = RemoteRoute {
            channel: RemoteChannel::Test,
            user: "user".into(),
            chat: "chat".into(),
        };
        let conversations = RemoteConversations::default();
        conversations.arm(route.clone()).unwrap();
        let approvals = Arc::new(ApprovalRegistry::default());
        let shutdown = AtomicBool::new(false);
        let bridge = RemotePrompts {
            shared: &shared,
            route: &route,
            conversations: &conversations,
            approvals: &approvals,
            shutdown: &shutdown,
            notify: Arc::new(|_| {}),
            text: Arc::new(|_| {}),
            typing: Arc::new(|_| {}),
        };
        let deliver = |id: &str, text: &str| {
            dispatch_delivery(&shared, &route, id, None, |_| {
                let ticket = bridge.prepare(id)?.expect("mapped or armed");
                bridge.submit(ticket, id, text.into(), Vec::new())
            })
        };
        let image = bridge.stage_image("first", 0, &crate::test_support::png_bytes(2, 2, [42, 21, 9]), "png").unwrap();
        let temporary_path = image.staged.clone();
        assert!(temporary_path.exists());
        assert!(matches!(dispatch_delivery(&shared, &route, "first", None, |_| {
            let ticket = bridge.prepare("first")?.expect("armed chat");
            bridge.submit(ticket, "first", "first".into(), vec![image])
        }), DeliveryResult::Handled));
        assert!(!temporary_path.exists(), "staged image is released after admission");
        let admission = app.lock().unwrap().committed_admission(&RemoteChannel::Test.delivery_key("first")).unwrap();
        assert_eq!(admission.receipt.attachment_ids.len(), 1);
        gate.wait_entered();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| deliver("second", "also run the tests"));
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                !worker.is_finished(),
                "delivery must wait for durable steering"
            );
            gate.release();
            assert!(matches!(worker.join().unwrap(), DeliveryResult::Handled));
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !shared.active_run_info().is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.active_run_info().is_null());
        assert!(
            app.lock()
                .unwrap()
                .committed_admission(&RemoteChannel::Test.delivery_key("second"))
                .is_some()
        );
        assert!(app.lock().unwrap().committed_admission("second").is_none());
        shared.mark_shutting_down();
        shared.drain_workers();
        drop(bridge);
        drop(approvals);
        drop(shared);
        let app = Arc::try_unwrap(app).ok().unwrap().into_inner().unwrap();
        app.close().unwrap();
        crate::test_support::cleanup_tree(&storage);
        crate::test_support::cleanup_tree(&project_root);
    }
}
