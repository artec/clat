//! Host-wide setup: configuration is authoritative; validation is transient.
//! No session, project context, tools, or durable events enter this service.
//! All writes share model_updates with the old settings APIs. Profile + route
//! activation is a single settings commit; failure keeps the previous state.
use super::*;
use crate::CancelToken;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod request;
#[cfg(test)]
mod tests;
mod worker;

pub(crate) struct SetupService {
    epoch: String,
    operations: Mutex<BTreeMap<String, Arc<Operation>>>,
}
impl Default for SetupService {
    fn default() -> Self {
        Self {
            epoch: uuid::Uuid::new_v4().to_string(),
            operations: Mutex::new(BTreeMap::new()),
        }
    }
}
impl Drop for SetupService {
    fn drop(&mut self) {
        for op in self
            .operations
            .get_mut()
            .expect("setup operations")
            .values()
        {
            op.cancel.cancel();
        }
    }
}
struct Operation {
    signature: String,
    status: Mutex<ModelSetupOperation>,
    cancel: CancelToken,
}
#[derive(Clone, Serialize)]
pub struct ModelSetupOperation {
    pub operation_id: String,
    pub revision: String,
    pub model: String,
    pub saved: bool,
    pub state: String,
    pub failure: Option<String>,
}
#[derive(Serialize)]
pub struct ModelSetupView {
    pub scope: String,
    pub revision: String,
    pub state: &'static str,
    pub settings: ModelSettingsView,
    pub vendors: Vec<request::VendorHelp>,
}
fn setup_error(code: &str) -> ApplicationError {
    ApplicationError::presented(crate::i18n::UiMessage::new(&format!("setup.{code}")))
}
fn configuration_state(config: &ModelConfig, credentials: &ProviderCredentials) -> &'static str {
    if !config.is_configured() {
        return "needs_model";
    }
    if config.preset.is_some() && credentials.value(0).is_none_or(|key| key.trim().is_empty()) {
        return "needs_credentials";
    }
    "configured"
}
impl TrustedProjectApplication {
    fn setup_revision(&self) -> String {
        format!(
            "{}:{}",
            self.host_storage.model_setup.epoch,
            self.host_storage.model_revision()
        )
    }
    pub fn model_setup_view(&self) -> Result<ModelSetupView, ApplicationError> {
        // model_settings_view takes the same lock itself. Retry on a revision
        // change so the settings and readiness always describe one snapshot.
        loop {
            let revision = self.setup_revision();
            let settings = self.model_settings_view()?;
            let _guard = self
                .host_storage
                .model_updates
                .lock()
                .expect("model updates");
            if revision != self.setup_revision() {
                continue;
            }
            let (config, credentials) = self.model_state()?;
            return Ok(ModelSetupView {
                scope: format!(
                    "{:x}",
                    Sha256::digest(self.host_storage.root().to_string_lossy().as_bytes())
                ),
                revision,
                state: configuration_state(&config, &credentials),
                settings,
                vendors: request::vendor_help(),
            });
        }
    }
    pub fn model_setup_status(&self, id: &str) -> Result<ModelSetupOperation, ApplicationError> {
        let op = self.setup_operation(id)?;
        let mut view = op.status.lock().expect("setup status").clone();
        if view.saved && view.revision != self.setup_revision() {
            view.state = "stale".into();
        }
        Ok(view)
    }
    pub fn model_setup_cancel(&self, id: &str) -> Result<ModelSetupOperation, ApplicationError> {
        let op = self.setup_operation(id)?;
        let mut view = op.status.lock().expect("setup status");
        if view.state == "checking" {
            op.cancel.cancel();
            view.state = "cancelled".into();
        }
        drop(view);
        self.model_setup_status(id)
    }
    fn setup_operation(&self, id: &str) -> Result<Arc<Operation>, ApplicationError> {
        self.host_storage
            .model_setup
            .operations
            .lock()
            .expect("setup operations")
            .get(id)
            .cloned()
            .ok_or_else(|| setup_error("operation_missing"))
    }
}
