use crate::CancelToken;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::SessionTitler;
use crate::session::id::SessionId;
use crate::session::use_cases::{SessionService, SetTitleExpectation};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use super::*;

/// 自动命名任务（仅排给仍无显式标题的会话；CAS 防覆盖并发手工改名）。
/// 绑定产生它的会话：期望值与会话不可分（F-A）。
pub(super) struct AutotitleJob {
    pub(super) session_id: SessionId,
    pub(super) config: ModelConfig,
    pub(super) credentials: ProviderCredentials,
    pub(super) expectation: SetTitleExpectation,
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
        expectation,
    } = job;
    // F-A：会话已切换 → 生成与写入都针对错误会话，直接放弃（连模型
    // 调用也省下）。set_title 侧的会话守卫是第二道门。
    if sessions.active_id().as_ref() != Some(session_id) {
        return;
    }
    // 双发竞争（两次 run 各排一任务，任务 1 已落盘）：排队到执行之间
    // 标题可能已存在——早退省下一次注定被 CAS 拒绝的 LLM 调用（对抗
    // 审计 2026-08-19）。
    if sessions.title_state().1.is_some() {
        return;
    }
    let Some(first_user) = sessions.first_user_text() else {
        return;
    };
    let derived = crate::session::projection::fallback_title(&first_user);
    if derived.is_empty() {
        return;
    }
    let Some(title) = titler.generate_title(config, credentials, &first_user, cancel) else {
        return;
    };
    if !title.is_empty() && title != derived {
        // provider 派生标题的 source 引用生成它的 provider/model
        // （catalog §2.2，审计 P1-14）。
        let applied = sessions.set_title(
            session_id,
            expectation.clone(),
            &title,
            crate::session::use_cases::TitleSource::Provider {
                provider: &config.protocol.to_string(),
                model: &config.model,
            },
        );
        if matches!(applied, Ok(true)) {
            broadcast_to(subscribers, ApplicationEvent::TitleUpdated { title });
        }
    }
}
