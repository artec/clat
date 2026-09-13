//! Secret-free model selection. Credentials remain inside the host.
use super::HostClient;
use crate::application::{ModelRouteView, ModelSettingsView};
use serde_json::json;

pub struct HostModelChoices {
    pub settings: ModelSettingsView,
    pub profiles: Vec<(String, ModelRouteView)>,
}

impl HostClient {
    pub fn model_choices(&self) -> Result<HostModelChoices, String> {
        let settings: ModelSettingsView =
            serde_json::from_value(self.call("model.settings.get", &json!({}))?)
                .map_err(|_| "invalid host model settings")?;
        let mut profiles = Vec::new();
        for name in &settings.profiles {
            let route: Option<ModelRouteView> =
                serde_json::from_value(self.call("model.profile.get", &json!({"name": name}))?)
                    .map_err(|_| "invalid host model profile")?;
            profiles.push((
                name.clone(),
                route.ok_or("model profiles changed; reopen /model")?,
            ));
        }
        Ok(HostModelChoices { settings, profiles })
    }
}
