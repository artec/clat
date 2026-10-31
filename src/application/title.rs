use crate::CancelToken;
use crate::model::{ModelConfig, ProviderCredentials};
use crate::plugins::services::SessionTitler;
use crate::session::id::SessionId;
use crate::session::use_cases::{SessionService, SetTitleExpectation};
use std::path::{Path, PathBuf};
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
    join: Mutex<Vec<JoinHandle<()>>>,
}

impl TitleWorker {
    pub(super) fn spawn(
        titler: Arc<dyn SessionTitler>,
        sessions: Arc<SessionService>,
        subscribers: Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
        storage_root: PathBuf,
        utility: Arc<crate::plugins::UtilityRuntime>,
    ) -> Result<Self, ApplicationError> {
        #[cfg(test)]
        crate::test_support::execution_observation::record("utility.title_worker");
        let (sender, receiver) = mpsc::sync_channel::<AutotitleJob>(1);
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let join = std::thread::Builder::new()
            .name("clat-title".into())
            .spawn(move || {
                while !worker_cancel.is_cancelled() {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => {
                            let Ok(producer) = utility.producer(&worker_cancel) else {
                                break;
                            };
                            maybe_autotitle(
                                titler.as_ref(),
                                sessions.as_ref(),
                                &job,
                                &worker_cancel,
                                &subscribers,
                                &storage_root,
                                &producer,
                            )
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| ApplicationError::new(format!("spawn title worker: {error}")))?;
        Ok(Self {
            sender,
            cancel,
            join: Mutex::new(vec![join]),
        })
    }

    pub(super) fn shutdown(&mut self) -> Result<(), ApplicationError> {
        self.cancel.cancel();
        crate::plugin::lifecycle::join(&self.join).map_err(ApplicationError::new)?;
        Ok(())
    }
}

/// 请求期间的手工改名会让迟到的模型标题失败（CB1-04）。CAS/属主拒绝是
/// 正常语义（手工标题优先），不留证据；生成失败（含超时/空回复）写一行
/// 有界脱敏证据。落盘成功后广播 `TitleUpdated`（N2）——前端据此刷新。
fn maybe_autotitle(
    titler: &dyn SessionTitler,
    sessions: &SessionService,
    job: &AutotitleJob,
    cancel: &CancelToken,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    storage_root: &Path,
    producer: &crate::plugin::lifecycle::Producer,
) {
    let AutotitleJob {
        session_id,
        config,
        credentials,
    } = job;
    if cancel.is_cancelled() || !titler.enabled() {
        return;
    }
    let attempt = match producer.commit(|| sessions.prepare_title_attempt(session_id)) {
        Ok(Ok(Some(attempt))) => attempt,
        Ok(Ok(None)) | Err(_) => return,
        Ok(Err(error)) => {
            let _ = producer.commit(|| {
                append_utility_evidence(
                    storage_root,
                    "session-title",
                    Some(session_id.as_str()),
                    "prepare",
                    &super::bounded_reason(&error.to_string()),
                )
            });
            return;
        }
    };
    let generated = match titler.generate_title(config, credentials, &attempt.context, cancel) {
        Ok(generated) => generated,
        Err(failure) => {
            let _ = producer.commit(|| {
                append_utility_evidence(
                    storage_root,
                    "session-title",
                    Some(session_id.as_str()),
                    "generate",
                    &failure.safe_text(),
                )
            });
            return;
        }
    };
    let _ = producer.commit(|| {
        commit_generated_title(
            sessions,
            subscribers,
            storage_root,
            session_id,
            &attempt.expectation,
            &attempt.message_seqs,
            generated,
        )
    });
}

/// 生成的标题落盘（清洗为空 → 证据行；CAS/属主拒绝是正常语义不留痕）并
/// 在成功后广播 `TitleUpdated`。
fn commit_generated_title(
    sessions: &SessionService,
    subscribers: &Arc<Mutex<Vec<mpsc::Sender<ApplicationEvent>>>>,
    storage_root: &Path,
    session_id: &SessionId,
    expectation: &crate::session::use_cases::SetTitleExpectation,
    message_seqs: &[u64],
    generated: crate::plugins::services::GeneratedTitle,
) {
    if generated.title.is_empty() {
        append_utility_evidence(
            storage_root,
            "session-title",
            Some(session_id.as_str()),
            "sanitize",
            "generated title sanitized to empty",
        );
        return;
    }
    // provider 派生标题的 source 引用生成它的 provider/model
    // （catalog §2.2，审计 P1-14）。
    let applied = sessions.set_title(
        session_id,
        expectation.clone(),
        &generated.title,
        crate::session::use_cases::TitleSource::Provider {
            provider: &generated.provider,
            model: &generated.model,
            message_seqs: Some(message_seqs),
        },
    );
    match applied {
        Ok(true) => {
            broadcast_to(
                subscribers,
                ApplicationEvent::TitleUpdated {
                    title: generated.title,
                },
            );
        }
        Ok(false) => {}
        Err(error) => append_utility_evidence(
            storage_root,
            "session-title",
            Some(session_id.as_str()),
            "commit",
            &super::bounded_reason(&error.to_string()),
        ),
    }
}

/// 有界证据：一行 JSON 一次 utility 失败，追加写
/// `<storage-root>/utility-log.jsonl`（与 vision-probe-log 同级同纪律）。
/// `reason` 只含 CLAT 自有类别词（R1-F1：`UtilityFailure::safe_text()`）或
/// 自有 SessionError 摘要——provider 错误正文/对话上下文/凭据一概不进
/// 日志。文件超过 256 KiB 时轮转为 `.1`（覆盖旧轮转）；**轮转失败时丢
/// 弃本行**（R1-F4：有界契约优先于单条证据），主链路不受影响。
pub(super) fn append_utility_evidence(
    storage_root: &Path,
    task: &str,
    session: Option<&str>,
    stage: &str,
    reason: &str,
) {
    const CAP: u64 = 256 * 1024;
    // 写者不止一个：title worker 线程与任意建议调用线程会并发追加同一
    // 文件（实测无锁时两行 JSON 交错撕裂）。进程内静态锁串行化全部
    // 证据写——微秒级临界区，跨 storage root 互斥无正确性影响。
    static EVIDENCE_WRITE: Mutex<()> = Mutex::new(());
    let _guard = EVIDENCE_WRITE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let path = storage_root.join("utility-log.jsonl");
    if let Ok(metadata) = std::fs::metadata(&path)
        && metadata.len() > CAP
    {
        // R1-F4：目标被目录占用/不可替换时丢本行，绝不向超限文件续写。
        if std::fs::rename(&path, storage_root.join("utility-log.jsonl.1")).is_err() {
            return;
        }
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let line = serde_json::json!({
        "ts": timestamp,
        "task": task,
        "session": session,
        "stage": stage,
        "reason": reason,
    });
    // 单次 write（先组串再写），配合上面的互斥，保证一行一行完整落盘。
    let record = format!("{line}\n");
    let result = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .and_then(|mut file| {
            use std::io::Write as _;
            file.write_all(record.as_bytes())
        });
    if let Err(error) = result {
        let _ = error; // 无处可写时只丢证据行。
    }
}

#[cfg(test)]
mod tests;

impl TrustedProjectApplication {
    /// 用户改名（`/rename`）：`SetTitleExpectation::Force` +
    /// `TitleSource::User`（catalog §2.2——用户改名不引用 provider）。
    ///
    /// 门槛（N4，2026-08-19 放宽）：有活动会话即可改——不再要求 LLM
    /// 已起名。原门槛把"首轮一次性自动命名失败/早于该功能的旧会话"
    /// 永久挡在门外（用户实测几百轮的会话被拒）；CAS 本就保证改名
    /// 压制迟到的自动命名（N5），门槛没有额外保护价值。清洗后为空
    /// 拒绝（`Invalid`）。成功后广播 `TitleUpdated`。
    pub fn rename_session(&mut self, raw: &str) -> Result<RenameOutcome, ApplicationError> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(RenameOutcome::NoSession);
        };
        self.rename_selected_session(&session_id, raw)
    }

    /// Bind a delayed frontend intent to its original conversation. Selection
    /// validation and the durable write share this mutable application borrow.
    pub fn rename_selected_session(
        &mut self,
        session_id: &SessionId,
        raw: &str,
    ) -> Result<RenameOutcome, ApplicationError> {
        if self.current_session_id().as_ref() != Some(session_id) {
            return Ok(RenameOutcome::NoSession);
        }
        let title = crate::session::projection::sanitize_user_title(raw);
        if title.is_empty() {
            return Ok(RenameOutcome::Invalid);
        }
        let applied = self
            .sessions
            .set_title(
                session_id,
                SetTitleExpectation::Force,
                &title,
                crate::session::use_cases::TitleSource::User,
            )
            .map_err(session_error)?;
        if applied {
            broadcast_to(
                &self.subscribers,
                ApplicationEvent::TitleUpdated {
                    title: title.clone(),
                },
            );
            Ok(RenameOutcome::Renamed { title })
        } else {
            // set_title 的会话守卫（活动 id 漂移）——与 NoSession 同义。
            Ok(RenameOutcome::NoSession)
        }
    }
}
