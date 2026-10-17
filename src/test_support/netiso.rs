//! NETISO-1（AG-DOGFOOD-7）测试隔离暴露面：武装出站防线、经生产工厂
//! 路径构建真实 HTTP 适配器、复算 utility companion 的生效路由。
//!
//! 只在测试构建 / `test-support` feature 下编译；生产二进制不包含。
//! 武装是进程级的——武装测试必须住在独立测试二进制（根包
//! `tests/netiso_outbound_guard.rs`），不得混入共享 lib 测试进程。

use crate::model::{Model, ModelConfig, ModelError, ProviderCredentials};
use crate::providers::outbound_guard::GuardArm;

pub use crate::providers::outbound_guard::arm_outbound_guard;

/// 与生产 `CompatibleFactory::build` 相同的真实适配器构造路径
/// （`src/plugins/providers.rs`），供隔离测试直接驱动出站点。
pub fn openai_compatible(
    config: &ModelConfig,
    credentials: &ProviderCredentials,
) -> Result<Box<dyn Model>, ModelError> {
    Ok(Box::new(
        crate::providers::OpenAiCompatibleModel::from_runtime_fields(
            credentials.values().to_vec(),
            config,
        )?,
    ))
}

/// 与生产 `ResponsesFactory::build` 相同的真实适配器构造路径。
pub fn openai_responses(
    config: &ModelConfig,
    credentials: &ProviderCredentials,
) -> Result<Box<dyn Model>, ModelError> {
    Ok(Box::new(
        crate::providers::OpenAiModel::from_runtime_fields(
            credentials.values().to_vec(),
            config.model.trim(),
            config.endpoint.trim(),
        )?,
    ))
}

/// 复算 production utility 路由（无显式 utility profile 时的默认分支，
/// 含预设 companion 解析）。与 `DefaultUtilityModel::route` 无 store 分支
/// 同一函数——不是测试侧重写的字符串断言。
pub fn effective_utility_route(primary: &ModelConfig) -> ModelConfig {
    crate::plugins::conversation::utility::resolve_default(primary)
}

/// 与生产适配器 `send()` 相同的出站 URL 拼接规则。
pub fn outbound_request_url(config: &ModelConfig) -> String {
    crate::providers::openai_compatible::join_endpoint(&config.endpoint, &config.request_path)
}

/// 保持 `GuardArm` 对外可见（调用方持有令牌直到测试结束）。
pub type OutboundGuardArm = GuardArm;
