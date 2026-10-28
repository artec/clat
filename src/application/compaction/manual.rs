//! Manual compaction admission, cancellation and completion have one core owner.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

impl TrustedProjectApplication {
    /// 手动 `/compact`：异步 worker 内执行（含网络摘要），立即返回可取
    /// 消的 handle（INV-C11）；与活动 Run 互斥。完成经
    /// `ApplicationEvent::CompactionUpdated` 报告。
    pub fn compact_session(&mut self) -> Result<CompactHandle, ApplicationError> {
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("another run is already active"));
        }
        self.prepare_compaction_slot()?;
        let work = ManualCompactionWork::prepare(self)?;
        let handle = work.spawn(Arc::clone(&self.subscribers))?;
        self.active_compaction = Some(handle.clone());
        Ok(handle)
    }

    fn prepare_compaction_slot(&mut self) -> Result<(), ApplicationError> {
        if self.compaction_started_at().is_some() {
            return Err(ApplicationError::new("a compaction is already active"));
        }
        if let Some(previous) = self.active_compaction.take() {
            previous.join()?;
        }
        Ok(())
    }

    /// State comes from the worker, never from frontend delivery of notices.
    pub fn compaction_started_at(&self) -> Option<i64> {
        self.active_compaction
            .as_ref()
            .filter(|handle| !handle.is_finished())
            .map(|handle| handle.started_ms)
    }

    /// Cancellation is a request; the slot stays busy until the worker exits.
    pub fn cancel_compaction(&self) -> bool {
        match self.active_compaction.as_ref() {
            Some(handle) if !handle.is_finished() => {
                handle.cancel();
                true
            }
            _ => false,
        }
    }
}

impl ManualCompactionWork {
    fn prepare(app: &TrustedProjectApplication) -> Result<Self, ApplicationError> {
        app.sessions
            .active_id()
            .ok_or_else(|| ApplicationError::new("no conversation to compact"))?;
        let turn = app.sessions.active_turns().map_err(session_error)?;
        let (config, credentials) = app.model_state()?;
        if !config.is_configured() {
            return Err(ApplicationError::new(
                "model is not configured; configure a model and endpoint first",
            ));
        }
        let compactor = app
            .compactor
            .clone()
            .ok_or_else(|| ApplicationError::new("compaction service is not available"))?;
        Ok(Self {
            sessions: Arc::clone(&app.sessions),
            journal: app.sessions.journal().map_err(session_error)?,
            compactor,
            config,
            credentials,
            cancel: CancelToken::new(),
            turn,
        })
    }

    fn spawn(
        self,
        subscribers: Arc<Mutex<Vec<std::sync::mpsc::Sender<ApplicationEvent>>>>,
    ) -> Result<CompactHandle, ApplicationError> {
        let busy = Arc::new(AtomicBool::new(true));
        let join_slot = Arc::new(Mutex::new(None));
        let report_slot: Arc<Mutex<Option<Result<CompactReport, PresentedText>>>> =
            Arc::new(Mutex::new(None));
        let handle = CompactHandle {
            started_ms: crate::session::event::now_ms(),
            cancel: self.cancel.clone(),
            busy: Arc::clone(&busy),
            join: Arc::clone(&join_slot),
            report: Arc::clone(&report_slot),
        };
        let worker = std::thread::Builder::new()
            .name("clat-compact".into())
            .spawn(move || {
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::CompactionUpdated(CompactionStatus::Started),
                );
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.execute()))
                        .unwrap_or_else(|payload| {
                            Err(UiMessage::new("compaction.worker_failed")
                                .arg("error", crate::plugin::panic_message(payload))
                                .into())
                        });
                // CB1-11：结构化结果存入 handle，供 join_report 消费。
                if let Ok(mut slot) = report_slot.lock() {
                    *slot = Some(result.clone());
                }
                busy.store(false, Ordering::Release);
                broadcast_to(
                    &subscribers,
                    ApplicationEvent::CompactionUpdated(compact_result_status(&result)),
                );
            })
            .map_err(|error| ApplicationError::new(format!("spawn compaction worker: {error}")))?;
        *join_slot
            .lock()
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))? = Some(worker);
        Ok(handle)
    }
}

struct ManualCompactionWork {
    sessions: Arc<SessionService>,
    journal: Arc<dyn RunJournal>,
    compactor: Arc<dyn HistoryCompactor>,
    config: ModelConfig,
    credentials: ProviderCredentials,
    cancel: CancelToken,
    turn: u64,
}
impl ManualCompactionWork {
    fn execute(self) -> Result<CompactReport, PresentedText> {
        let nodes = self
            .sessions
            .surface_nodes()
            .map_err(|error| error.to_string())?;
        let compaction_nodes: Vec<CompactionNode> = nodes
            .iter()
            .map(|(seq, item)| CompactionNode {
                seq: *seq,
                item: item.clone(),
            })
            .collect();
        let outcome = compact_history(
            self.compactor.as_ref(),
            &self.config,
            &self.credentials,
            &compaction_nodes,
            true,
            &self.cancel,
        );
        if self.cancel.is_cancelled() {
            return Err(UiMessage::new("compaction.cancelled").into());
        }
        let summary = outcome.summary.as_deref().ok_or_else(|| {
            outcome
                .degraded
                .clone()
                .unwrap_or_else(|| "nothing to compact".into())
        })?;

        let shadowed = &compaction_nodes[..outcome.shadowed_count];
        // SV 最小适配行（S6 例外，docs/todo/session-v3-execution.md）：
        // V3 受保护 system 头不得被压缩 shadow——头之前的
        // 前缀节点（至多首条用户消息）从遮蔽前缀剔除。
        let shadowed = match self.sessions.last_system_head() {
            Some((head, _)) => {
                let skip = shadowed.iter().take_while(|node| node.seq <= head).count();
                &shadowed[skip..]
            }
            None => shadowed,
        };
        write_compaction_events(
            self.journal.as_ref(),
            shadowed,
            summary,
            &outcome,
            &self.config,
            self.turn,
        )?;
        let _ = self.sessions.sync_active();
        Ok(CompactReport {
            shadowed_count: outcome.shadowed_count,
            degraded: outcome.degraded,
        })
    }
}

fn compact_result_status(result: &Result<CompactReport, PresentedText>) -> CompactionStatus {
    let note = match result {
        Ok(report) => report.status_presentation().into(),
        Err(error) => UiMessage::new("command.compaction_failed")
            .arg_message("error", error.message())
            .into(),
    };
    CompactionStatus::Finished {
        note,
        succeeded: result.is_ok(),
    }
}

/// `/compact` 的可取消句柄：取消令牌 + 幂等 join + 业务结果
/// （CB1-11：headless 调用方经 `join_report` 拿到结构化 CompactReport）。
#[derive(Clone)]
pub struct CompactHandle {
    started_ms: i64,
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    report: Arc<Mutex<Option<Result<CompactReport, PresentedText>>>>,
}

impl CompactHandle {
    pub fn started_at(&self) -> i64 {
        self.started_ms
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_finished(&self) -> bool {
        !self.busy.load(Ordering::Acquire)
    }

    pub fn join(&self) -> Result<(), ApplicationError> {
        let mut slot = self
            .join
            .lock()
            .map_err(|_| ApplicationError::new("compaction join lock poisoned"))?;
        if let Some(handle) = slot.take() {
            handle
                .join()
                .map_err(|_| ApplicationError::new("compaction worker panicked"))?;
        }
        Ok(())
    }

    /// join 后返回压缩业务结果（Ok=事件族已耐久落盘；Err=失败原因）。
    /// ApplicationEvent 只是展示通道，这里才是结构化结果。
    pub fn join_report(&self) -> Result<Result<CompactReport, PresentedText>, ApplicationError> {
        self.join()?;
        self.report
            .lock()
            .map_err(|_| ApplicationError::new("compaction report lock poisoned"))
            .map(|slot| slot.clone().unwrap_or(Err("no compaction result".into())))
    }
}
