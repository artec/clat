use crate::CancelToken;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::SessionTitler;
use crate::session::id::SessionId;
use crate::session::use_cases::SessionService;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use super::*;

/// Successful-run notification. The worker coalesces turns and captures CAS
/// and bounded conversation together for this session, immediately before I/O.
pub(super) struct AutotitleJob {
    pub(super) session_id: SessionId,
    pub(super) config: ModelConfig,
    pub(super) credentials: ProviderCredentials,
}

pub(super) struct TitleWorker {
    pub(super) sender: mpsc::SyncSender<AutotitleJob>,
    cancel: CancelToken,
    join: Option<JoinHandle<()>>,
}

impl TitleWorker {
    pub(super) fn spawn(
        titler: Arc<dyn SessionTitler>,
        sessions: Arc<SessionService>,
        subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    ) -> Result<Self, ApplicationError> {
        let (sender, receiver) = mpsc::sync_channel::<AutotitleJob>(1);
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let join = std::thread::Builder::new()
            .name("clat-title".into())
            .spawn(move || {
                while !worker_cancel.is_cancelled() {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => maybe_autotitle(
                            titler.as_ref(),
                            sessions.as_ref(),
                            &job,
                            &worker_cancel,
                            &subscribers,
                        ),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| ApplicationError::new(format!("spawn title worker: {error}")))?;
        Ok(Self {
            sender,
            cancel,
            join: Some(join),
        })
    }

    pub(super) fn shutdown(&mut self) -> Result<(), ApplicationError> {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            // 进程退出语义（2026-08-19，对照 DSH 的 AbortSignal 处置）：
            // 取消后至多等 EXIT_JOIN_GRACE。在途标题请求的 HTTP 阻塞
            // 阶段不被合作式取消打断（`CancelAwareReader` 只在 read
            // 返回之间检查标志），无界 join 会把退出拖到请求超时——
            // 实测可达数十秒（"exit 有时很慢"的根因之一）。放弃等价
            // 于一次失败的自动命名（INV-F 静默语义），线程随进程退出
            // 回收；运行期路径不经过这里，"无 detached 任务"的运行期
            // 不变量不变。
            join_with_grace(join, EXIT_JOIN_GRACE, "title worker")
                .map_err(ApplicationError::new)?;
        }
        Ok(())
    }
}

/// 请求期间的手工改名会让迟到的模型标题失败（CB1-04）。任何失败静默。
/// 落盘成功后广播 `TitleUpdated`（N2）——前端据此刷新标题显示。
fn maybe_autotitle(
    titler: &dyn SessionTitler,
    sessions: &SessionService,
    job: &AutotitleJob,
    cancel: &CancelToken,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
) {
    let AutotitleJob {
        session_id,
        config,
        credentials,
    } = job;
    if cancel.is_cancelled() || !titler.enabled() {
        return;
    }
    let Ok(Some(attempt)) = sessions.prepare_title_attempt(session_id) else {
        return;
    };
    let Some(generated) = titler.generate_title(config, credentials, &attempt.context, cancel)
    else {
        return;
    };
    if !generated.title.is_empty() {
        // provider 派生标题的 source 引用生成它的 provider/model
        // （catalog §2.2，审计 P1-14）。
        let applied = sessions.set_title(
            session_id,
            attempt.expectation,
            &generated.title,
            crate::session::use_cases::TitleSource::Provider {
                provider: &generated.provider,
                model: &generated.model,
                message_seqs: Some(&attempt.message_seqs),
            },
        );
        if matches!(applied, Ok(true)) {
            broadcast_to(
                subscribers,
                ApplicationEvent::TitleUpdated {
                    title: generated.title,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests;
