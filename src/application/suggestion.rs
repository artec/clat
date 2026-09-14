//! Manual, non-durable prompt suggestions over the shared utility service.
use super::*;
use crate::plugins::services::{UtilityModel, UtilityTask};

pub struct PreparedPromptSuggestion {
    utility: Arc<dyn UtilityModel>,
    config: crate::ModelConfig,
    credentials: crate::ProviderCredentials,
    session_id: SessionId,
    context: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptSuggestion {
    pub session_id: SessionId,
    pub text: String,
}

impl PreparedPromptSuggestion {
    pub fn generate(self, cancel: &crate::CancelToken) -> Option<PromptSuggestion> {
        let output = self.utility.generate(
            UtilityTask::PromptSuggestion,
            &self.config,
            &self.credentials,
            &self.context,
            cancel,
        )?;
        let text: String = output.text.trim().chars().take(1000).collect();
        (!text.is_empty()).then_some(PromptSuggestion {
            session_id: self.session_id,
            text,
        })
    }
}

impl TrustedProjectApplication {
    pub fn prepare_prompt_suggestion(
        &mut self,
    ) -> Result<PreparedPromptSuggestion, ApplicationError> {
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
        if !self.utility.enabled(UtilityTask::PromptSuggestion) {
            return Err(ApplicationError::new("prompt suggestions are disabled"));
        }
        let session_id = self
            .current_session_id()
            .ok_or_else(|| ApplicationError::new("no active conversation"))?;
        let (config, credentials) = self.model_state()?;
        let attempt = self
            .sessions
            .prepare_suggestion_attempt(&session_id)
            .map_err(|error| ApplicationError::new(error.to_string()))?
            .ok_or_else(|| {
                ApplicationError::new("prompt suggestion budget exhausted or conversation empty")
            })?;
        Ok(PreparedPromptSuggestion {
            utility: Arc::clone(&self.utility),
            config,
            credentials,
            session_id,
            context: attempt.context,
        })
    }
}
