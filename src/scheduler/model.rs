use super::*;
use crate::{
    model::{ModelConfig, ProviderCredentials},
    plugins::services::ConfigStore,
};
pub(crate) fn load(
    store: &dyn ConfigStore,
    profile: Option<&str>,
) -> Result<(ModelConfig, ProviderCredentials), ScheduleError> {
    let state = match profile {
        Some(name) => store.load_profile(name),
        None => store.load_model_state(),
    }
    .map_err(|e| ScheduleError::new("model_unavailable").detail(e))?
    .ok_or_else(|| ScheduleError::new("model_unavailable"))?;
    let (config, credentials) = state;
    let config = config.normalized();
    if !config.is_configured() {
        return Err(ScheduleError::new("model_unavailable"));
    }
    Ok((config, credentials))
}
pub(crate) fn approved(config: &ModelConfig, profile: Option<String>) -> ApprovedModel {
    ApprovedModel {
        fingerprint: digest(config),
        protocol: config.protocol.to_string(),
        model: config.model.clone(),
        endpoint: safe_endpoint(&config.endpoint),
        profile,
    }
}
fn safe_endpoint(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "[invalid endpoint]".into(),
    }
}
