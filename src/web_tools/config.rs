use super::*;
use crate::{ModelProtocol, ProviderCredentials, control_storage::ControlStorage};
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchProvider {
    #[default]
    Http,
    Deepseek,
}
/// WEB-D1（2026-11-20 负责人裁定）：web_search 的三态路由。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider {
    /// 智能（默认）：web key → 计费 Search API；当前模型为 DeepSeek
    /// 厂商且无 web key → Messages 原生免费搜索；其余无 key → 共享
    /// Ceramic 代理。
    #[default]
    Smart,
    /// 强制共享 Ceramic 代理（显式选择，即便配置了 web key）。
    Ceramic,
    /// 强制 DeepSeek Messages 原生搜索（显式选择，忽略 web key）。
    DeepseekMessages,
}
/// 快照（prepare 期）冻结的搜索执行路径。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SearchRoute {
    /// 计费 Search API（`/v1/tools/search`，吃 PaidCallBudget）。
    Api,
    /// 共享 Ceramic 代理（项目方付费）。
    Shared,
    /// DeepSeek Messages 原生搜索（免费，成本=一次辅助模型 turn）。
    Messages,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    fetch_provider: FetchProvider,
    use_model_key: bool,
    #[serde(default)]
    search_provider: SearchProvider,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebSettingsView {
    pub fetch_provider: FetchProvider,
    pub use_model_key: bool,
    pub search_provider: SearchProvider,
    pub credential_set: bool,
    pub search_available: bool,
    pub fetch_available: bool,
    /// 当前生效的搜索路径（展示用）：`api` / `messages` / `shared`。
    pub search_route: &'static str,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSettingsEdit {
    pub fetch_provider: FetchProvider,
    pub use_model_key: bool,
    #[serde(default)]
    pub search_provider: SearchProvider,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_key: bool,
}
pub(crate) struct WebService {
    pub(super) control: Arc<ControlStorage>,
    transport: Arc<dyn WebTransport>,
    /// 共享 Ceramic 搜索的口令缓存(内存 only,见 ceramic.rs)。
    pub(super) shared: ceramic::SharedToken,
    /// 私有地址同意通道的问答槽(与 ask_user 同一管线;headless 为空
    /// 槽 → fail-closed,见 consent.rs)。
    pub(super) asker: Arc<crate::interaction::AskUserSlot>,
    /// 同意流分类解析的测试缝:生产恒 None(走系统解析器);测试注入
    /// 固定映射避免依赖真实 DNS。见 consent.rs `resolve`。
    pub(super) resolve_hook: std::sync::Mutex<Option<ResolveHook>>,
}

/// 解析缝签名:主机名 → 候选地址(见 consent.rs)。
pub(super) type ResolveHook = Arc<dyn Fn(&str) -> Option<Vec<std::net::IpAddr>> + Send + Sync>;
#[derive(Clone)]
pub(super) struct Snapshot {
    pub provider: FetchProvider,
    pub key: Option<String>,
    /// prepare 期冻结的搜索路径（INV-WD2：执行期不复核）。
    pub search_route: SearchRoute,
    /// Messages 路径凭据（INV-WD3 顺序：当前 DeepSeek 模型对话 key →
    /// `web:deepseek` vendor key；对话 key 即搜索 key，DSH 同款）。
    pub messages_key: Option<String>,
}
impl WebService {
    pub(crate) fn new(
        control: Arc<ControlStorage>,
        asker: Arc<crate::interaction::AskUserSlot>,
    ) -> Self {
        Self {
            control,
            transport: Arc::new(NativeTransport),
            shared: ceramic::SharedToken::default(),
            asker,
            resolve_hook: std::sync::Mutex::new(None),
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
    /// `web:deepseek` vendor key 槽（Messages 路径的对话 key 缺位回退）。
    fn web_key(&self) -> Result<Option<String>, WebError> {
        self.control
            .load_vendor_key("web:deepseek", ModelProtocol::OpenAiCompatible)
            .map(|c| {
                c.and_then(|c| {
                    c.value(0)
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned)
                })
            })
            .map_err(|_| WebError::new("settings_failed"))
    }
    /// INV-WD2：路由判据与凭据在同一读区段取值并随快照冻结——执行期
    /// 不再复核（`native_web_preparation_freezes_provider_and_credential_
    /// selection` 同纪律）。厂商判定只认直连 DeepSeek 端点
    /// （`endpoint_vendor`：deepseek.com）；网关路由（如 OpenCode Go）
    /// 的凭据打不了 api.deepseek.com，不进 Messages 路径。
    fn routing(
        &self,
        s: &Settings,
    ) -> Result<(SearchRoute, Option<String>, Option<String>), WebError> {
        let key = self.key(s)?;
        let model = self
            .control
            .load_model_state()
            .map_err(|_| WebError::new("settings_failed"))?;
        let deepseek_model = model.as_ref().is_some_and(|(config, _)| {
            crate::model::endpoint_vendor(&config.endpoint) == crate::model::ModelVendor::DeepSeek
        });
        let model_key = model.and_then(|(_, credentials)| {
            credentials
                .value(0)
                .filter(|k| !k.trim().is_empty())
                .map(str::to_owned)
        });
        let messages_key = match model_key {
            some @ Some(_) => some,
            None => self.web_key()?,
        };
        let search_route = match s.search_provider {
            SearchProvider::Ceramic => SearchRoute::Shared,
            SearchProvider::DeepseekMessages => SearchRoute::Messages,
            // INV-WD1：智能模式下 DeepSeek 模型 + 无 web key → Messages，
            // 绝不触达 Ceramic；Ceramic 兜底仅"非 DeepSeek 模型且无 key"。
            SearchProvider::Smart => {
                if key.is_some() {
                    SearchRoute::Api
                } else if deepseek_model {
                    SearchRoute::Messages
                } else {
                    SearchRoute::Shared
                }
            }
        };
        Ok((search_route, key, messages_key))
    }
    pub(crate) fn view(&self) -> Result<WebSettingsView, WebError> {
        let _guard = self.control.web_update_lock();
        let s = self.settings()?;
        let (search_route, key, _) = self.routing(&s)?;
        let set = key.is_some();
        Ok(WebSettingsView {
            fetch_provider: s.fetch_provider,
            use_model_key: s.use_model_key,
            search_provider: s.search_provider,
            credential_set: set,
            search_available: true,
            fetch_available: s.fetch_provider == FetchProvider::Http || set,
            search_route: match search_route {
                SearchRoute::Api => "api",
                SearchRoute::Messages => "messages",
                SearchRoute::Shared => "shared",
            },
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
            search_provider: edit.search_provider,
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
        let (search_route, key, messages_key) = self.routing(&s)?;
        Ok(Snapshot {
            provider: s.fetch_provider,
            key,
            search_route,
            messages_key,
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
        Self {
            control,
            transport,
            shared: ceramic::SharedToken::default(),
            asker: crate::interaction::AskUserSlot::shared(),
            resolve_hook: std::sync::Mutex::new(None),
        }
    }
}
