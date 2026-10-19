//! Domain-owned conversation state and contracts.
use super::UtilityFailure;
use crate::model::{ModelConfig, ProviderCredentials};

pub(crate) struct GeneratedTitle {
    pub(crate) title: String,
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// 会话标题生成（能力批次 1 / F）。失败返回结构化安全类别（R1-F1：
/// `UtilityFailure::safe_text()` 是唯一文本出口，不透传 provider 正文）；
/// 标题必须已经过清洗（首个非空行、去引号/Markdown/控制序列、宽度契约
/// ≤48 显示列，超限仅在词内 run 之外截断——不可断的超限输出清洗为空由
/// 调用方保留旧标题；TITLE-CUT-1/R1，2026-10-18）。
/// `cancel` 与调用方生命周期联动（run worker 旁路线程/close 可取消）。
pub(crate) trait SessionTitler: Send + Sync {
    fn enabled(&self) -> bool {
        true
    }

    fn generate_title(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
        conversation: &str,
        cancel: &crate::CancelToken,
    ) -> Result<GeneratedTitle, UtilityFailure>;
}
