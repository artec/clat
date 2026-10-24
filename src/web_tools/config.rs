use super::*;
use crate::{ModelProtocol, ProviderCredentials, control_storage::ControlStorage};
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchProvider {
    #[default]
    Http,
    Deepseek,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    fetch_provider: FetchProvider,
    use_model_key: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebSettingsView {
    pub fetch_provider: FetchProvider,
    pub use_model_key: bool,
    pub credential_set: bool,
    pub search_available: bool,
    pub fetch_available: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSettingsEdit {
    pub fetch_provider: FetchProvider,
    pub use_model_key: bool,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_key: bool,
}
pub(crate) struct WebService {
    control: Arc<ControlStorage>,
    transport: Arc<dyn WebTransport>,
}
#[derive(Clone)]
pub(super) struct Snapshot {
    pub provider: FetchProvider,
    pub key: Option<String>,
}
impl WebService {
    pub(crate) fn new(control: Arc<ControlStorage>) -> Self {
        Self {
            control,
            transport: Arc::new(NativeTransport),
        }
    }
    fn settings(&self) -> Result<Settings, WebError> {
        self.control
            .web_settings()
            .map_err(|e| WebError::new("settings_failed").detail(e))
            .and_then(|v| serde_json::from_value(v).map_err(|_| WebError::new("settings_failed")))
    }
    fn key(&self, settings: &Settings) -> Result<Option<String>, WebError> {
        let vendor = if settings.use_model_key {
            "deepseek"
        } else {
            "web:deepseek"
        };
        self.control
            .load_vendor_key(vendor, ModelProtocol::OpenAiCompatible)
            .map(|c| {
                c.and_then(|c| {
                    c.value(0)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                })
            })
            .map_err(|_| WebError::new("settings_failed"))
    }
    pub(crate) fn view(&self) -> Result<WebSettingsView, WebError> {
        let _guard = self.control.web_update_lock();
        let s = self.settings()?;
        let set = self.key(&s)?.is_some();
        Ok(WebSettingsView {
            fetch_provider: s.fetch_provider,
            use_model_key: s.use_model_key,
            credential_set: set,
            search_available: set,
            fetch_available: s.fetch_provider == FetchProvider::Http || set,
        })
    }
    pub(crate) fn save(&self, edit: WebSettingsEdit) -> Result<(), WebError> {
        let _guard = self.control.web_update_lock();
        if edit.clear_key && edit.api_key.is_some() {
            return Err(WebError::new("invalid_settings"));
        }
        if let Some(key) = &edit.api_key
            && (key.trim().is_empty() || key.len() > 4096 || key.chars().any(char::is_control))
        {
            return Err(WebError::new("invalid_settings"));
        }
        if edit.clear_key || edit.api_key.is_some() {
            let mut key = ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible);
            key.set_value(0, edit.api_key.unwrap_or_default());
            self.control
                .upsert_vendor_key("web:deepseek", &key)
                .map_err(|_| WebError::new("settings_failed"))?;
        }
        let settings = Settings {
            fetch_provider: edit.fetch_provider,
            use_model_key: edit.use_model_key,
        };
        self.control
            .save_web_settings(
                &serde_json::to_value(settings).map_err(|_| WebError::new("invalid_settings"))?,
            )
            .map_err(|e| WebError::new("settings_failed").detail(e))
    }
    pub(super) fn snapshot(&self) -> Result<Snapshot, WebError> {
        let _guard = self.control.web_update_lock();
        let s = self.settings()?;
        Ok(Snapshot {
            provider: s.fetch_provider,
            key: self.key(&s)?,
        })
    }
    pub(super) fn transport(&self) -> &dyn WebTransport {
        self.transport.as_ref()
    }
    #[cfg(test)]
    pub(super) fn with_transport(
        control: Arc<ControlStorage>,
        transport: Arc<dyn WebTransport>,
    ) -> Self {
        Self { control, transport }
    }
}
