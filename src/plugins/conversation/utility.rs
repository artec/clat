//! Shared, static-plugin utility model. Titles and suggestions use one route.
use crate::model::{
    CancelToken, FinishReason, ModelConfig, ModelOptions, ModelRequest, ProviderCredentials,
};
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::plugins::services::{
    CONFIG_SERVICE, CONFIG_SERVICE_ID, ConfigStore, PROVIDER_SERVICE, PROVIDER_SERVICE_ID,
    ProviderRegistry, UTILITY_MODEL_SERVICE, UTILITY_MODEL_SERVICE_ID, UtilityFailure,
    UtilityFinish, UtilityModel, UtilityOutput, UtilityTask,
};
use crate::presets::preset_by_id;
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use std::sync::Arc;
use std::time::Duration;

const ID: PluginId = PluginId::new("builtin.utility_model");
const REQUIRES: &[crate::plugin::ServiceId] = &[PROVIDER_SERVICE_ID];
const OPTIONAL: &[crate::plugin::ServiceId] = &[CONFIG_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[UTILITY_MODEL_SERVICE_ID],
    requires: REQUIRES,
    optional: OPTIONAL,
};
const DEADLINE: Duration = Duration::from_secs(15);

pub(crate) struct CompanionUtilityPlugin;

impl Plugin for CompanionUtilityPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let providers = context
            .require(PROVIDER_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let config = context
            .try_require(CONFIG_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context
            .provide(
                UTILITY_MODEL_SERVICE,
                Arc::new(DefaultUtilityModel { providers, config }) as Arc<dyn UtilityModel>,
            )
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultUtilityModel {
    providers: Arc<ProviderRegistry>,
    config: Option<Arc<dyn ConfigStore>>,
}

impl UtilityModel for DefaultUtilityModel {
    fn enabled(&self, task: UtilityTask) -> bool {
        let Some(store) = &self.config else {
            return task == UtilityTask::SessionTitle;
        };
        let Ok(settings) = store.load_utility_settings() else {
            return false;
        };
        match task {
            UtilityTask::SessionTitle => settings.naming_enabled,
            UtilityTask::PromptSuggestion => settings.suggestions_enabled,
        }
    }

    fn generate(
        &self,
        task: UtilityTask,
        primary: &ModelConfig,
        primary_credentials: &ProviderCredentials,
        conversation: &str,
        cancel: &CancelToken,
    ) -> Result<UtilityOutput, UtilityFailure> {
        if !self.enabled(task) {
            return Err(UtilityFailure::Disabled);
        }
        let (config, credentials) = self
            .route(primary, primary_credentials)
            .ok_or(UtilityFailure::RouteUnavailable)?;
        // AG-DOGFOOD-6（TITLE-1/SUG-1 根因）：utility 请求是一次性小请求，
        // 不复用主 agent 循环的思考档位——内置预设全部是思考不可关闭的
        // 推理模型（DeepSeek/GLM 官方 "Disabling reasoning is no longer
        // supported"），带着 `thinking: enabled` + `effort: high` 和
        // 32/96 的 max_tokens 去请求，思考 token 先耗尽预算，流以
        // length 终止且无正文，标题/建议静默为空。整形为任务级形态：
        // 不发 thinking 对象（交由服务端默认）、effort 收敛到 low、预算
        // 放宽到思考后仍有正文余量。
        let config = shape_utility_request(&config);
        let response = request(
            &self.providers,
            task,
            &config,
            &credentials,
            conversation,
            cancel,
        )?;
        Ok(UtilityOutput {
            text: response,
            provider: config.protocol.to_string(),
            model: config.model,
        })
    }
}

/// 任务级请求整形：适用于任何路由来源（预设 companion 或自定义端点），
/// 规则与厂商无关——不发 `thinking` 对象（GLM 官方拒绝 `disabled`，
/// 服务端默认本就开启）；`reasoning_effort` 已存在时收敛为 low；其余
/// extra_body 字段（stream_options 等）原样保留。
pub(crate) fn shape_utility_request(config: &ModelConfig) -> ModelConfig {
    let mut shaped = config.clone();
    if let Some(body) = shaped.extra_body.as_object_mut() {
        body.remove("thinking");
        if body.contains_key("reasoning_effort") {
            body.insert("reasoning_effort".into(), serde_json::json!("low"));
        }
    }
    shaped
}

impl DefaultUtilityModel {
    fn route(
        &self,
        primary: &ModelConfig,
        primary_credentials: &ProviderCredentials,
    ) -> Option<(ModelConfig, ProviderCredentials)> {
        let Some(store) = &self.config else {
            return Some((resolve_default(primary), primary_credentials.clone()));
        };
        let settings = store.load_utility_settings().ok()?;
        if let Some(profile) = settings.profile {
            return store.load_profile(&profile).ok().flatten();
        }
        Some((resolve_default(primary), primary_credentials.clone()))
    }
}

fn request(
    providers: &Arc<ProviderRegistry>,
    task: UtilityTask,
    config: &ModelConfig,
    credentials: &ProviderCredentials,
    conversation: &str,
    cancel: &CancelToken,
) -> Result<String, UtilityFailure> {
    let build: ModelBuildFn = {
        let providers = Arc::clone(providers);
        let config = config.clone();
        let credentials = credentials.clone();
        Box::new(move || providers.build(&config, &credentials))
    };
    let mut model = retry_model_with(
        config.protocol.to_string(),
        config.model.clone(),
        build,
        RetryPolicy {
            max_attempts: 1,
            backoff: Vec::new(),
            total_deadline: Some(DEADLINE),
            total_attempt_cap: Some(1),
            ..RetryPolicy::default()
        },
    );
    let items = [crate::model::ModelItem::user_text(conversation.to_owned())];
    let tools: [crate::tool::ToolDefinition; 0] = [];
    let options = ModelOptions {
        output_limit: Some(output_limit(task)),
        ..ModelOptions::default()
    };
    let request_cancel = cancel.child_with_deadline(std::time::Instant::now() + DEADLINE);
    // R1-F1：错误正文（不可信）不离开本层——只保留自有 kind 类别码。
    let response = model
        .stream(
            ModelRequest {
                instructions: Some(instructions(task)),
                items: &items,
                tools: &tools,
                options: &options,
                cancel: &request_cancel,
            },
            &mut Vec::new(),
        )
        .map_err(|error| UtilityFailure::RequestFailed(error.kind()))?;
    if response.finish_reason == FinishReason::Cancelled {
        return Err(UtilityFailure::Cancelled);
    }
    if response.text.trim().is_empty() {
        return Err(UtilityFailure::EmptyResponse {
            finish: UtilityFinish::from(response.finish_reason),
        });
    }
    Ok(response.text)
}

/// 自有 finish 词表：未知 provider 值归一为 `Unknown`，不携带原串。
impl From<FinishReason> for UtilityFinish {
    fn from(reason: FinishReason) -> Self {
        match reason {
            FinishReason::Completed => Self::Completed,
            FinishReason::ToolCalls => Self::ToolCalls,
            FinishReason::MaxTokens => Self::MaxTokens,
            FinishReason::Refusal => Self::Refusal,
            FinishReason::Cancelled => Self::Cancelled,
            FinishReason::Incomplete => Self::Incomplete,
            FinishReason::Error => Self::Error,
            FinishReason::Unknown(_) => Self::Unknown,
        }
    }
}

fn instructions(task: UtilityTask) -> &'static str {
    match task {
        UtilityTask::SessionTitle => {
            "Generate a concise title (at most 8 words) for the coding assistant conversation \
             below, focusing on its recent topic. Use the same language as the conversation. \
             Output only the title text: no quotes, no trailing punctuation beyond what is \
             natural, no explanation."
        }
        UtilityTask::PromptSuggestion => {
            "Predict one useful next user message for the coding assistant conversation below. \
             Use the same language as the conversation. Output only the proposed user message, \
             with no quotes or explanation."
        }
    }
}

fn output_limit(task: UtilityTask) -> u32 {
    match task {
        // 预算必须覆盖"低档思考 + 一行正文"：思考不可关闭的模型上，
        // 32/96 会被思考 token 耗尽（TITLE-1/SUG-1 根因）。
        UtilityTask::SessionTitle => 512,
        UtilityTask::PromptSuggestion => 768,
    }
}

pub(crate) fn resolve_default(primary: &ModelConfig) -> ModelConfig {
    let Some(preset) = primary.preset.as_deref().and_then(preset_by_id) else {
        return primary.clone();
    };
    if primary.protocol != preset.protocol
        || primary.model != preset.model
        || primary.endpoint != preset.endpoint
        || primary.request_path != preset.request_path
    {
        return primary.clone();
    }
    let Some(companion_id) = preset.utility_model else {
        return primary.clone();
    };
    if companion_id == preset.id {
        return primary.clone();
    }
    let Some(companion) = preset_by_id(companion_id) else {
        return primary.clone();
    };
    if companion.vendor != preset.vendor
        || companion.protocol != primary.protocol
        || companion.endpoint != primary.endpoint
        || companion.request_path != primary.request_path
    {
        return primary.clone();
    }
    let mut config = ModelConfig::default();
    companion.apply(&mut config);
    config.auth_header = primary.auth_header.clone();
    config.auth_prefix = primary.auth_prefix.clone();
    config.extra_headers = primary.extra_headers.clone();
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utility_companions_keep_credentials_on_the_exact_primary_endpoint() {
        for (main, expected) in [
            ("deepseek-v4-pro", "deepseek-flash"),
            ("glm-5.3", "glm-5.3-flash"),
            ("qwen3.8-max", "qwen3.8-flash"),
            ("kimi-k3", "kimi-for-coding"),
            ("hy4-preview", "hy3"),
        ] {
            let mut config = ModelConfig::default();
            preset_by_id(main).unwrap().apply(&mut config);
            config.auth_header = "X-Private-Key".into();
            config.auth_prefix = String::new();
            let utility = resolve_default(&config);
            assert_eq!(utility.preset.as_deref(), Some(expected));
            assert_eq!(utility.endpoint, config.endpoint);
            assert_eq!(utility.protocol, config.protocol);
            assert_eq!(utility.auth_header, "X-Private-Key");
            assert!(utility.auth_prefix.is_empty());
            for field in 0..5 {
                let mut custom = config.clone();
                match field {
                    0 => custom.endpoint = "https://private.example/v1".into(),
                    1 => custom.model = "custom-model".into(),
                    2 => custom.request_path = "/private/chat".into(),
                    3 => custom.protocol = crate::model::ModelProtocol::OpenAiResponses,
                    _ => custom.preset = None,
                }
                assert_eq!(
                    resolve_default(&custom),
                    custom,
                    "custom route must not be guessed"
                );
            }
        }
    }
}
