//! Manual, non-durable prompt suggestions over the shared utility service.
use super::*;
#[cfg(test)]
use crate::plugins::services::UtilityModel;
use crate::plugins::services::{UtilityFailure, UtilityTask};

pub struct PreparedPromptSuggestion {
    task: UtilityTask,
    utility: Arc<crate::plugins::UtilityRuntime>,
    config: crate::ModelConfig,
    credentials: crate::ProviderCredentials,
    session_id: SessionId,
    context: String,
    storage_root: std::path::PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestion {
    pub session_id: SessionId,
    pub text: String,
}

impl PreparedPromptSuggestion {
    /// 生成建议：`Err` 是**安全文本**——core 内部先收敛为
    /// `UtilityFailure` 类别码（R1-F1：只含自有词表，不透传 provider
    /// 错误正文），再经 `safe_text()` 导出；并留一行 utility 证据。成功
    /// 只返回预览文本——绝不自动发送、不写 journal、不触碰草稿。
    pub fn generate(self, cancel: &crate::CancelToken) -> Result<PromptSuggestion, String> {
        self.execute(cancel, |result| result)?
    }

    pub(super) fn execute<T>(
        self,
        cancel: &crate::CancelToken,
        finish: impl FnOnce(Result<PromptSuggestion, String>) -> T,
    ) -> Result<T, String> {
        let producer = self
            .utility
            .producer(cancel)
            .map_err(|_| UtilityFailure::Cancelled.safe_text())?;
        let outcome = self.utility.generate(
            self.task,
            &self.config,
            &self.credentials,
            &self.context,
            cancel,
        );
        producer
            .commit(|| finish(self.finish(outcome)))
            .map_err(|_| UtilityFailure::Cancelled.safe_text())
    }

    fn finish(
        &self,
        outcome: Result<crate::plugins::services::UtilityOutput, UtilityFailure>,
    ) -> Result<PromptSuggestion, String> {
        match outcome {
            Ok(output) => {
                let text: String = output.text.trim().chars().take(1000).collect();
                if text.is_empty() {
                    // 空正文已由 utility 层归类；此处防御同语义。
                    let failure = UtilityFailure::EmptyResponse {
                        finish: crate::plugins::services::UtilityFinish::Completed,
                    };
                    self.record_failure(&failure.safe_text());
                    return Err(failure.safe_text());
                }
                Ok(PromptSuggestion {
                    session_id: self.session_id.clone(),
                    text,
                })
            }
            Err(failure) => {
                self.record_failure(&failure.safe_text());
                Err(failure.safe_text())
            }
        }
    }

    fn record_failure(&self, reason: &str) {
        super::title::append_utility_evidence(
            &self.storage_root,
            "prompt-suggestion",
            Some(self.session_id.as_str()),
            "generate",
            reason,
        );
    }
}

impl TrustedProjectApplication {
    #[cfg(test)]
    pub(crate) fn set_utility_for_test(&mut self, utility: Arc<dyn UtilityModel>) {
        self.utility = Some(Arc::new(
            self.utility_service().unwrap().with_model(utility),
        ));
    }

    pub fn prepare_prompt_suggestion(
        &mut self,
    ) -> Result<PreparedPromptSuggestion, ApplicationError> {
        self.prepare_suggestion(UtilityTask::PromptSuggestion)
    }

    pub(super) fn prepare_suggestion(
        &mut self,
        task: UtilityTask,
    ) -> Result<PreparedPromptSuggestion, ApplicationError> {
        let utility = self.utility_service()?.clone();
        let producer = utility
            .producer(&crate::CancelToken::new())
            .map_err(ApplicationError::new)?;
        if self
            .active_run
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("a run is already active"));
        }
        if self
            .active_compaction
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Err(ApplicationError::new("compaction is active"));
        }
        if !utility.enabled(task) {
            return Err(ApplicationError::new("prompt suggestions are disabled"));
        }
        let session_id = self
            .current_session_id()
            .ok_or_else(|| ApplicationError::new("no active conversation"))?;
        let (config, credentials) = self.model_state()?;
        let attempt = producer
            .commit(|| self.sessions.prepare_suggestion_attempt(&session_id))
            .map_err(ApplicationError::new)?
            .map_err(|error| ApplicationError::new(error.to_string()))?
            .ok_or_else(|| {
                ApplicationError::new("conversation has no text for a prompt suggestion")
            })?;
        Ok(PreparedPromptSuggestion {
            task,
            utility,
            config,
            credentials,
            session_id,
            context: attempt.context,
            storage_root: self.host_storage.root().to_path_buf(),
        })
    }
}
