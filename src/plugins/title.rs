//! 会话标题生成（能力批次 1 / F）。
//!
//! `core.session_title` 服务：用已配置模型把首条用户消息提炼为简短标
//! 题，替换 storage 落库时的"首行截断"默认值。单次小请求（无工具、
//! 短 deadline、重试一次）；失败返回 None，调用方静默保留既有标题
//! （INV-F1）。标题清洗：取首个非空行、剥包裹引号/Markdown 标记、按
//! char 边界截断到 16 字符（INV-F2）。

use super::services::{
    PROVIDER_SERVICE, PROVIDER_SERVICE_ID, ProviderRegistry, SESSION_TITLE_SERVICE,
    SESSION_TITLE_SERVICE_ID, SessionTitler,
};
use crate::model::{
    CancelToken, FinishReason, ModelConfig, ModelOptions, ModelRequest, ProviderCredentials,
};
use crate::plugin::{Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind};
use crate::providers::{ModelBuildFn, RetryPolicy, retry_model_with};
use std::sync::Arc;
use std::time::Duration;

const ID: PluginId = PluginId::new("builtin.session_title");
const REQUIRES: &[crate::plugin::ServiceId] = &[PROVIDER_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[SESSION_TITLE_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

const TITLE_INSTRUCTIONS: &str = "Generate a concise title (at most 8 words) for a coding \
assistant conversation that starts with the message below. Use the same language as the \
message. Output only the title text: no quotes, no trailing punctuation beyond what is \
natural, no explanation.";

const TITLE_OUTPUT_LIMIT: u32 = 32;
const TITLE_DEADLINE: Duration = Duration::from_secs(15);
const TITLE_MAX_CHARS: usize = 16;

pub(crate) struct SessionTitlePlugin;

impl Plugin for SessionTitlePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let providers = context
            .require(PROVIDER_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        context
            .provide(
                SESSION_TITLE_SERVICE,
                Arc::new(DefaultSessionTitler { providers }) as Arc<dyn SessionTitler>,
            )
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct DefaultSessionTitler {
    providers: Arc<ProviderRegistry>,
}

impl SessionTitler for DefaultSessionTitler {
    fn generate_title(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
        first_user_message: &str,
        cancel: &CancelToken,
    ) -> Option<String> {
        let build: ModelBuildFn = {
            let providers = Arc::clone(&self.providers);
            let config = config.clone();
            let credentials = credentials.clone();
            Box::new(move || providers.build(&config, &credentials))
        };
        let mut model = retry_model_with(
            config.protocol.to_string(),
            config.model.clone(),
            build,
            RetryPolicy {
                max_attempts: 2,
                backoff: vec![Duration::from_secs(1)],
                total_deadline: Some(TITLE_DEADLINE),
                total_attempt_cap: Some(2),
                ..RetryPolicy::default()
            },
        );
        let items = [crate::model::ModelItem::user_text(
            first_user_message.to_owned(),
        )];
        let tools: [crate::tool::ToolDefinition; 0] = [];
        let options = ModelOptions {
            output_limit: Some(TITLE_OUTPUT_LIMIT),
            ..ModelOptions::default()
        };
        // 请求令牌派生自 worker 的取消令牌并带上 TITLE_DEADLINE：
        // retry 的 total_deadline 只约束尝试间隔，管不住单次 send 的
        // connect（30s）/等响应头（60s）阶段——那些阶段不轮询取消标
        // 志，只有 `remaining()` 的请求级 timeout 能有界化（2026-08-19
        // 退出延迟诊断）。父取消仍即时生效（Esc/退出）。
        let request_cancel = cancel.child_with_deadline(std::time::Instant::now() + TITLE_DEADLINE);
        let request = ModelRequest {
            instructions: Some(TITLE_INSTRUCTIONS),
            items: &items,
            tools: &tools,
            options: &options,
            cancel: &request_cancel,
        };
        let mut sink = Vec::new();
        let response = model.stream(request, &mut sink).ok()?;
        if response.finish_reason == FinishReason::Cancelled {
            return None;
        }
        Some(sanitize_title(&response.text))
    }
}

/// 取首个非空行、剥包裹引号/Markdown、截断到 16 chars。
pub(crate) fn sanitize_title(raw: &str) -> String {
    let mut text = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .replace(['\r', '\t'], " ");
    let mut inner = text.as_str();
    while let Some(stripped) = strip_wrapping_markup(inner) {
        inner = stripped;
    }
    text = inner.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(TITLE_MAX_CHARS).collect()
}

fn strip_wrapping_markup(text: &str) -> Option<&str> {
    const PAIRS: [(&str, &str); 7] = [
        ("\"", "\""),
        ("「", "」"),
        ("『", "』"),
        ("**", "**"),
        ("__", "__"),
        ("`", "`"),
        ("~~", "~~"),
    ];
    for (open, close) in PAIRS {
        if let Some(body) = text
            .strip_prefix(open)
            .and_then(|body| body.strip_suffix(close))
        {
            return Some(body);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Model, ModelError, ModelEventSink, ModelFactory, ModelProtocol, ModelResponse,
    };
    use crate::plugin::{PluginId, PluginManager, PluginOwner};
    use crate::plugins::services::{PROVIDER_SERVICE, ProviderLease};
    use crate::plugins::{ProviderRegistryPlugin, SessionTitlePlugin};

    #[test]
    fn sanitizer_uses_first_line_strips_markup_and_truncates_on_char_boundary() {
        // 首个非空行 + 剥引号。
        assert_eq!(
            sanitize_title("\n  \"Fix login bug\"  \nignored"),
            "Fix login bug"
        );
        assert_eq!(sanitize_title("「修复登录」"), "修复登录");
        assert_eq!(
            sanitize_title("\n\n**修复登录竞态**\nignored"),
            "修复登录竞态"
        );
        // CJK 截断到 char 边界：80 个'中' → 16 个。
        assert_eq!(sanitize_title(&"中".repeat(80)).chars().count(), 16);
        // 纯引号输出清洗后为空。
        assert_eq!(sanitize_title("   \"\"   "), "");
    }

    struct CannedFactory(&'static str);

    impl ModelFactory for CannedFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for title tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(CannedModel(self.0)))
        }
    }

    struct CannedModel(&'static str);

    impl Model for CannedModel {
        fn provider(&self) -> &str {
            "title-fake"
        }

        fn model_id(&self) -> &str {
            "title-fake"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: self.0.into(),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Completed,
                usage: None,
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            })
        }
    }

    fn titler_with(factory: CannedFactory) -> Arc<dyn SessionTitler> {
        titler_with_factory(factory)
    }

    fn titler_with_factory(factory: impl ModelFactory + 'static) -> Arc<dyn SessionTitler> {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ProviderRegistryPlugin),
                Arc::new(SessionTitlePlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease: ProviderLease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.title")),
                Arc::new(factory),
            )
            .map_err(|error| error.to_string())
            .expect("register");
        manager.require(SESSION_TITLE_SERVICE).expect("titler")
    }

    #[test]
    fn successful_generation_returns_sanitized_title() {
        let titler = titler_with(CannedFactory("  \"Fix the login bug\" "));
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let title = titler
            .generate_title(
                &config,
                &credentials,
                "login fails on Safari",
                &CancelToken::new(),
            )
            .expect("title");
        assert_eq!(title, "Fix the login bu");
    }

    /// 不变量（2026-08-19 退出延迟）：自动标题请求必须携带 deadline
    /// ——`remaining()` 有值时 provider 会把它配置成请求级 HTTP
    /// timeout，connect（30s）与等响应头（60s）阶段才有界；worker 的
    /// 取消令牌本身无 deadline，pre-fix 此处为 None，退出撞上在途标
    /// 题请求可被拖住数十秒。
    #[test]
    fn title_request_carries_a_deadline_so_http_phases_are_bounded() {
        use std::sync::Mutex;
        let seen: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));

        struct DeadlineProbingFactory(Arc<Mutex<Option<Duration>>>);
        impl ModelFactory for DeadlineProbingFactory {
            fn protocol(&self) -> ModelProtocol {
                ModelProtocol::OpenAiCompatible
            }
            fn describe(
                &self,
                _credentials: &ProviderCredentials,
            ) -> crate::model::ProviderDescriptor {
                unimplemented!("not needed for title tests")
            }
            fn build(
                &self,
                _config: &ModelConfig,
                _credentials: &ProviderCredentials,
            ) -> Result<Box<dyn Model>, ModelError> {
                Ok(Box::new(DeadlineProbingModel(Arc::clone(&self.0))))
            }
        }

        struct DeadlineProbingModel(Arc<Mutex<Option<Duration>>>);
        impl Model for DeadlineProbingModel {
            fn provider(&self) -> &str {
                "title-fake"
            }
            fn model_id(&self) -> &str {
                "title-fake"
            }
            fn stream(
                &mut self,
                request: ModelRequest<'_>,
                _events: &mut dyn ModelEventSink,
            ) -> Result<ModelResponse, ModelError> {
                *self.0.lock().expect("seen") = request.cancel.remaining();
                Ok(ModelResponse {
                    text: String::new(),
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Completed,
                    usage: None,
                    provider_response_id: None,
                    provider_state: Vec::new(),
                    reasoning: None,
                })
            }
        }

        let titler = titler_with_factory(DeadlineProbingFactory(Arc::clone(&seen)));
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        let _ = titler.generate_title(&config, &credentials, "hello", &CancelToken::new());
        let remaining = *seen.lock().expect("seen");
        assert!(
            remaining.is_some(),
            "the title request must carry a deadline (pre-fix: the worker token has none)"
        );
        assert!(
            remaining.expect("checked Some") <= Duration::from_secs(15),
            "the deadline must be TITLE_DEADLINE, got {remaining:?}"
        );
    }

    struct FailingFactory;

    impl ModelFactory for FailingFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> crate::model::ProviderDescriptor {
            unimplemented!("not needed for title tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Err(ModelError::transport("title provider down"))
        }
    }

    #[test]
    fn generation_failure_is_silent_none() {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(ProviderRegistryPlugin),
                Arc::new(SessionTitlePlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        let _lease: ProviderLease = providers
            .register(
                PluginOwner::for_test(PluginId::new("test.title")),
                Arc::new(FailingFactory),
            )
            .map_err(|error| error.to_string())
            .expect("register");
        let titler = manager.require(SESSION_TITLE_SERVICE).expect("titler");
        let config = ModelConfig::default();
        let credentials = ProviderCredentials::for_protocol(config.protocol);
        assert!(
            titler
                .generate_title(&config, &credentials, "anything", &CancelToken::new())
                .is_none()
        );
    }
}
