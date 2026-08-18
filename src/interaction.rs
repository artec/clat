//! 人机交互端口（ask-user）：模型经 `ask_user` 工具向用户提问，core
//! 拥有契约，前端提供实现并经 `ApplicationRunRequest::asker` 安装。
//!
//! 阻塞模型沿用权限审批验证过的管线（worker 线程同步等待前端一次性
//! 应答）；journal 侧即普通 `tool/call` + `tool/result`（问题在参数里、
//! 答案在结果里），事件目录零扩展。等待中的崩溃由既有 RecoveryTracker
//! 合成 `TOOL_OUTCOME_UNKNOWN` 结果。

use crate::model::CancelToken;
use std::sync::{Arc, RwLock};

/// 一个可选项：标签是回传给模型的答案，描述仅供人看。
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单问单答的问题（v1：一次调用一问；多问由模型多次调用）。
pub struct AskQuestion {
    pub question: String,
    pub options: Vec<AskOption>,
    /// 是否提供自由输入。无选项时前端直接进入输入模式。
    pub allow_custom: bool,
}

/// 前端应答。`Declined`（拒绝/取消/断连）以 isError 工具结果回给模型，
/// run 继续。
pub enum AskAnswer {
    Selected(String),
    Custom(String),
    Declined,
}

/// 前端实现的问答端口。`ask` 阻塞直到用户应答或取消令牌生效——实现
/// 必须在等待中轮询 `cancel` 并以 `Declined` 返回。
pub trait UserAsker: Send + Sync {
    fn ask(&self, question: AskQuestion, cancel: &CancelToken) -> AskAnswer;
}

/// 可安装的前端实现槽：`ask_user` 工具在 bootstrap 时携带它注册，
/// `Application` 在每次 run 启动时按请求装入（前端 `Some`、headless
/// `None`），工具调用时按需读取。单活动 run 约束使单一槽位安全。
#[derive(Default)]
pub struct AskUserSlot {
    asker: RwLock<Option<Arc<dyn UserAsker>>>,
}

impl AskUserSlot {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 装入本次 run 的前端实现；`None` 清除（headless 降级）。
    pub fn install(&self, asker: Option<Arc<dyn UserAsker>>) {
        if let Ok(mut slot) = self.asker.write() {
            *slot = asker;
        }
    }

    pub fn asker(&self) -> Option<Arc<dyn UserAsker>> {
        self.asker.read().ok().and_then(|slot| slot.clone())
    }
}
