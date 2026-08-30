//! Permission model: the three interactive modes (`PermissionMode`), the
//! `SafeByDefault` headless delegate, the approver port, and the write
//! path fence (`WriteScope`).

use crate::project::Project;
use crate::tool::{ToolCall, ToolDefinition, ToolEffect};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Ask {
        reason: String,
    },
    Deny {
        reason: String,
    },
    /// Fail-closed because no approver exists to answer (non-interactive
    /// stdin, headless pipe). Run semantics are identical to `Deny`; the
    /// distinction exists so the journal can record the DSH outcome
    /// `unavailable` instead of `rejected` (event catalog §2.4).
    Unavailable {
        reason: String,
    },
}

/// A side-effecting tool call a policy needs a human to approve or deny.
///
/// `Run` itself never constructs this; it is the contract between an
/// [`InteractivePermissionPolicy`] and whatever front end (TUI, IDE, headless
/// prompt) can answer it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionRequest {
    pub tool: String,
    pub effect: ToolEffect,
    pub reason: String,
    pub arguments: Value,
    /// The model-issued tool-call id; lets journaling approvals reference
    /// the exact pending call (`tool/call`, `approval/asked` payloads).
    pub call_id: String,
}

impl PermissionRequest {
    /// D-2：DSH 宿主审批帧（`approval/requested`——只带 tool 名/reason/
    /// call id，无 effect/arguments 语义）的前端投影构造。effect 归
    /// `Write`（审批语义即副作用放行），arguments 空（前端滚动审阅对
    /// 缺席参数自然退化）。
    pub fn from_host_approval(tool: String, reason: String, call_id: String) -> Self {
        Self {
            tool,
            effect: ToolEffect::Write,
            reason,
            arguments: Value::Null,
            call_id,
        }
    }
}

/// UI-independent port implemented by TUI, desktop, headless clients, or
/// tests. It answers requests; permission classification remains in core.
/// 审批闭包的装箱形态（A1 起 decide 携带取消令牌）。
pub type BoxedAskFn =
    Box<dyn Fn(PermissionRequest, &crate::model::CancelToken) -> PermissionDecision + Send + Sync>;

pub trait PermissionApprover: Send + Sync {
    /// W1-17/A1：`cancel` 是本次 run 的取消令牌——实现必须能在等待人
    /// 答复期间响应它（对齐 `UserAsker::ask` 的形态）：run 取消后，滞留
    /// 的审批等待有界返回（语义 = Deny），不许挂到人答。
    fn decide(
        &self,
        request: PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> PermissionDecision;
}

impl<F> PermissionApprover for F
where
    F: Fn(PermissionRequest, &crate::model::CancelToken) -> PermissionDecision + Send + Sync,
{
    fn decide(
        &self,
        request: PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        self(request, cancel)
    }
}

pub trait PermissionPolicy: Send + Sync {
    fn check(
        &self,
        project: &Project,
        tool: &ToolDefinition,
        call: &ToolCall,
    ) -> PermissionDecision;
}

/// Wraps a base policy and resolves `Ask` decisions through an injected
/// interactive approver instead of failing the run.
///
/// The approver is a plain closure, so any client can implement it: the TUI
/// shows a dialog and blocks on the user, while a test or future headless
/// mode can answer programmatically. `Allow` and `Deny` from the base policy
/// pass through untouched; only `Ask` invokes the approver.
pub struct InteractivePermissionPolicy {
    delegate: Box<dyn PermissionPolicy>,
    approver: Arc<dyn PermissionApprover>,
    /// 本次 run 的取消令牌（W1-17/A1）：随审批请求传给 approver——
    /// run 取消后审批等待必须能被解开。
    cancel: crate::model::CancelToken,
}

impl InteractivePermissionPolicy {
    pub fn new(
        delegate: impl PermissionPolicy + 'static,
        cancel: crate::model::CancelToken,
        ask: BoxedAskFn,
    ) -> Self {
        Self::with_approver(delegate, cancel, Arc::new(BoxedApprover(ask)))
    }

    pub fn with_approver(
        delegate: impl PermissionPolicy + 'static,
        cancel: crate::model::CancelToken,
        approver: Arc<dyn PermissionApprover>,
    ) -> Self {
        Self {
            delegate: Box::new(delegate),
            approver,
            cancel,
        }
    }
}

struct BoxedApprover(BoxedAskFn);

impl PermissionApprover for BoxedApprover {
    fn decide(
        &self,
        request: PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        (self.0)(request, cancel)
    }
}

impl PermissionPolicy for InteractivePermissionPolicy {
    fn check(
        &self,
        project: &Project,
        tool: &ToolDefinition,
        call: &ToolCall,
    ) -> PermissionDecision {
        match self.delegate.check(project, tool, call) {
            PermissionDecision::Ask { reason } => {
                let request = PermissionRequest {
                    tool: tool.name.clone(),
                    effect: tool.effect,
                    reason,
                    arguments: call.arguments.clone(),
                    call_id: call.id.clone(),
                };
                match self.approver.decide(request, &self.cancel) {
                    // The approver answers with a final decision only.
                    PermissionDecision::Ask { .. } => PermissionDecision::Deny {
                        reason: "approver returned an unresolved decision".into(),
                    },
                    decision => decision,
                }
            }
            decision => decision,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SafeByDefault;

impl PermissionPolicy for SafeByDefault {
    fn check(
        &self,
        _project: &Project,
        tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        match tool.effect {
            ToolEffect::Pure | ToolEffect::Read | ToolEffect::SessionWrite => {
                PermissionDecision::Allow
            }
            ToolEffect::Write
            | ToolEffect::Execute
            | ToolEffect::Network
            | ToolEffect::ExternalRead
            | ToolEffect::Destructive => PermissionDecision::Ask {
                reason: format!("tool `{}` can cause side effects", tool.name),
            },
        }
    }
}

/// 用户可切换的权限档位（DSH sandbox/mode 的 CLAT 形态）。
///
/// 档位只移动 [`PermissionDecision::Ask`] 的边界：`Pure` / `Read` /
/// `SessionWrite` 永远放行，`FullAccess` 全放行，差别在副作用类工具
/// 是否需要逐次审批（见 [`mode_decision`] 决策表）。没有任何档位在
/// 表格层产生 `Deny`——拒绝始终来自人（approver），不是档位语义。
///
/// 档位是**会话属性**：以 `sandbox/mode` journal 事件（DSH 词汇，
/// latest-wins）随会话持久化，resume/重启随日志恢复；进程内的共享
/// cell（内部 `ModeSource::Shared`）只是活跃会话档位的镜像——切换在下
/// 一次权限检查生效，会话边界（mount 恢复 / resume / /new）按目标
/// 会话自己的 fold 重新对齐。从未记录过档位的遗留会话回落编译期默认
/// `ProjectWrite`（恰同 DSH shipped 默认 workspace-write）。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PermissionMode {
    /// 一切副作用工具逐次审批（决策等价于 [`SafeByDefault`] 列）。
    ReadOnly,
    /// 项目内文件写、任意路径读与网络/外部读工具自动放行（写工具本就
    /// 受 cap-std 项目根约束，读与网络对齐 DSH 的「不门控」面）；
    /// 命令执行与破坏性操作仍逐次审批。默认档。
    #[default]
    ProjectWrite,
    /// 全放行（DSH approval=never 对应物）；不再弹任何权限框。
    FullAccess,
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            PermissionMode::ReadOnly => "Read Only",
            PermissionMode::ProjectWrite => "Project Write",
            PermissionMode::FullAccess => "Full Access",
        })
    }
}

impl PermissionMode {
    /// journal 事件 `sandbox/mode` 的 payload 值——**DSH 词汇**
    /// （`read-only` / `workspace-write` / `danger-full-access`，
    /// DSH `sandbox/src/index.ts:29`），与 Display（用户可读、带空格）
    /// 分离：CLAT 与 DSH 的会话日志按此互读，不随 CLAT UI 文案演进。
    pub fn journal_value(&self) -> &'static str {
        match self {
            PermissionMode::ReadOnly => "read-only",
            PermissionMode::ProjectWrite => "workspace-write",
            PermissionMode::FullAccess => "danger-full-access",
        }
    }

    /// 解析 journal 值；未知值（未来 DSH 词汇/手改）返回 None，调用方
    /// 自行决定回落（fold 层保持上一已知档，装载层回落默认档）。
    pub fn from_journal_value(value: &str) -> Option<Self> {
        match value.trim() {
            "read-only" => Some(PermissionMode::ReadOnly),
            "workspace-write" => Some(PermissionMode::ProjectWrite),
            "danger-full-access" => Some(PermissionMode::FullAccess),
            _ => None,
        }
    }
}

/// 档位 × effect 决策表（不变量 P1）。
pub fn mode_decision(mode: PermissionMode, tool: &ToolDefinition) -> PermissionDecision {
    if mode_allows(mode, tool.effect) {
        return PermissionDecision::Allow;
    }
    let reason = match mode {
        PermissionMode::ReadOnly if tool.effect == ToolEffect::Write => format!(
            "tool `{}` writes files — file edits are gated under Read Only mode",
            tool.name
        ),
        PermissionMode::ReadOnly => format!(
            "tool `{}` ({}) is gated under Read Only mode",
            tool.name, tool.effect
        ),
        PermissionMode::ProjectWrite => format!(
            "tool `{}` ({}) is gated under Project Write mode — commands and destructive tools still need approval",
            tool.name, tool.effect
        ),
        PermissionMode::FullAccess => {
            unreachable!("full access allows every effect (see mode_allows)")
        }
    };
    PermissionDecision::Ask { reason }
}

/// 决策表的 Allow 格：唯一允许语义来源（`mode_decision` 与
/// `escalation_targets` 共享，防两处表漂移）。
pub fn mode_allows(mode: PermissionMode, effect: ToolEffect) -> bool {
    match mode {
        PermissionMode::FullAccess => true,
        // PW = 「文件与读自由；命令与破坏性操作逐次审」：Network /
        // ExternalRead（DSH 词汇表外/其 MCP 完全不设防）随读面一起
        // 放行；Execute / Destructive 是 CLAT 无内核沙箱时仅剩的两类
        // 无法 containment 的操作，保留逐次审（对齐 DSH 的承重偏差）。
        PermissionMode::ProjectWrite => matches!(
            effect,
            ToolEffect::Pure
                | ToolEffect::Read
                | ToolEffect::SessionWrite
                | ToolEffect::Write
                | ToolEffect::Network
                | ToolEffect::ExternalRead
        ),
        PermissionMode::ReadOnly => matches!(
            effect,
            ToolEffect::Pure | ToolEffect::Read | ToolEffect::SessionWrite
        ),
    }
}

/// 权限弹框的升级选项（不变量 P5）：列出比 `mode` 更宽、且能让本次
/// `effect` 直接放行的档位（宽度升序）。切过去仍要询问的档位不出现
/// ——那只是一次无效跳转（Execute@Read Only 不 offered Project
/// Write）。已放行的组合没有升级可言，返回空。
pub fn escalation_targets(mode: PermissionMode, effect: ToolEffect) -> Vec<PermissionMode> {
    if mode_allows(mode, effect) {
        return Vec::new();
    }
    let mut targets = Vec::new();
    if mode == PermissionMode::ReadOnly && mode_allows(PermissionMode::ProjectWrite, effect) {
        targets.push(PermissionMode::ProjectWrite);
    }
    targets.push(PermissionMode::FullAccess);
    targets
}

/// 注入模型系统指令的档位说明（DSH renderPolicyContext 对应物）：
/// 让模型在尝试前知道当前审批边界，而不是撞上拒绝/弹窗后才推断。
pub fn mode_guidance(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => {
            "every side-effecting tool call (file writes, commands, network) requires user approval before it runs"
        }
        PermissionMode::ProjectWrite => {
            "file edits, file reads (anywhere on disk), and network/search tools run without approval; commands and destructive tools require user approval"
        }
        PermissionMode::FullAccess => "all tools run without approval prompts",
    }
}

/// 写入路径围栏的档（不变量 SR2）：路径层与权限层读取同一 cell 的
/// 时刻快照——权限检查 Allow 不等于路径围栏开放。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteScope {
    /// 仅项目根相对路径（RO/PW 的围栏；exec 恒为此档）。
    ProjectRoot,
    /// 任意绝对路径（Full Access 的围栏开放，DSH danger-full-access
    /// 的「不设防」对应物；原子写纪律不随围栏放开）。
    Unrestricted,
}

/// 档位 → 写入围栏（SR2）：FA 开放绝对写；RO/PW 保持项目根。RO 下
/// 人工放行的单次写仍限项目根——对齐 DSH read-only 的升级阶梯
/// （只升到 workspace-write，不因一次审批放开全盘）。
pub fn mode_write_scope(mode: PermissionMode) -> WriteScope {
    match mode {
        PermissionMode::FullAccess => WriteScope::Unrestricted,
        PermissionMode::ReadOnly | PermissionMode::ProjectWrite => WriteScope::ProjectRoot,
    }
}

/// 写工具的围栏来源（与 [`ModeSource`] 对称）：TUI 传共享 cell（与
/// 权限检查读同一时刻的档位）；exec 传固定 [`WriteScope::ProjectRoot`]。
#[derive(Clone, Default)]
pub(crate) enum WriteScopeSource {
    #[default]
    ProjectRoot,
    Shared(Arc<std::sync::RwLock<PermissionMode>>),
}

impl WriteScopeSource {
    pub(crate) fn resolve(&self) -> WriteScope {
        match self {
            WriteScopeSource::ProjectRoot => WriteScope::ProjectRoot,
            WriteScopeSource::Shared(cell) => {
                mode_write_scope(*cell.read().expect("permission mode lock"))
            }
        }
    }
}

/// 权限策略工厂的档位来源（不变量 P7/P8）：
///
/// - [`ModeSource::Classic`]：委托 [`SafeByDefault`]，逐次询问——
///   headless `clat exec` 的既有行为，决策与理由文本零变化。
/// - [`ModeSource::Shared`]：委托 [`ModePolicy`]，读共享档位 cell——
///   交互前端的档位系统。
pub(crate) enum ModeSource {
    Classic,
    Shared(Arc<std::sync::RwLock<PermissionMode>>),
}

impl Clone for ModeSource {
    fn clone(&self) -> Self {
        match self {
            ModeSource::Classic => ModeSource::Classic,
            ModeSource::Shared(cell) => ModeSource::Shared(Arc::clone(cell)),
        }
    }
}

/// 共享档位的策略委托：每次 `check` 读 cell，切换即生效（P3）。
pub struct ModePolicy {
    mode: Arc<std::sync::RwLock<PermissionMode>>,
}

impl ModePolicy {
    pub fn new(mode: Arc<std::sync::RwLock<PermissionMode>>) -> Self {
        Self { mode }
    }

    fn mode(&self) -> PermissionMode {
        *self.mode.read().expect("permission mode lock")
    }
}

impl PermissionPolicy for ModePolicy {
    fn check(
        &self,
        _project: &Project,
        tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        mode_decision(self.mode(), tool)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl PermissionPolicy for AllowAll {
    fn check(
        &self,
        _project: &Project,
        _tool: &ToolDefinition,
        _call: &ToolCall,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread;

    fn definition(effect: ToolEffect) -> ToolDefinition {
        ToolDefinition {
            name: "test".into(),
            description: "test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
            effect,
            strict: true,
        }
    }

    fn call() -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "test".into(),
            arguments: serde_json::json!({"path": "notes.txt"}),
        }
    }

    #[test]
    fn safe_policy_allows_read_only_tools() {
        let policy = SafeByDefault;
        let project = Project::new(".");

        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Read), &call()),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn safe_policy_allows_session_write_but_not_other_side_effects() {
        // INV-T2：SessionWrite 由准确 effect 分类获得免审；Pure 语义
        // 不变，其余副作用仍需询问。
        let policy = SafeByDefault;
        let project = Project::new(".");
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::SessionWrite), &call()),
            PermissionDecision::Allow
        );
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Pure), &call()),
            PermissionDecision::Allow
        );
        assert!(matches!(
            policy.check(&project, &definition(ToolEffect::Write), &call()),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn safe_policy_asks_before_side_effects() {
        let policy = SafeByDefault;
        let project = Project::new(".");

        assert!(matches!(
            policy.check(&project, &definition(ToolEffect::Write), &call()),
            PermissionDecision::Ask { .. }
        ));
    }

    #[test]
    fn interactive_policy_passes_through_decisions_without_asking() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest, _cancel: &crate::model::CancelToken| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(
            SafeByDefault,
            crate::model::CancelToken::new(),
            Box::new(ask),
        );
        let project = Project::new(".");

        // Read tools are auto-allowed by the delegate and never reach the approver.
        assert_eq!(
            policy.check(&project, &definition(ToolEffect::Read), &call()),
            PermissionDecision::Allow
        );
        assert!(request_rx.try_recv().is_err());
        drop(decision_tx);
    }

    #[test]
    fn interactive_policy_asks_and_applies_user_decision() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest, _cancel: &crate::model::CancelToken| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(
            SafeByDefault,
            crate::model::CancelToken::new(),
            Box::new(ask),
        );
        let project = Project::new(".");
        let definition = definition(ToolEffect::Write);
        let call = call();

        let handle = thread::spawn(move || policy.check(&project, &definition, &call));
        let request = request_rx.recv().expect("request");
        assert_eq!(request.tool, "test");
        assert_eq!(request.effect, ToolEffect::Write);
        assert_eq!(request.arguments, serde_json::json!({"path": "notes.txt"}));

        decision_tx
            .send(PermissionDecision::Allow)
            .expect("decision");
        assert_eq!(handle.join().expect("join"), PermissionDecision::Allow);
    }

    #[test]
    fn interactive_policy_turns_an_unresolved_answer_into_deny() {
        let (request_tx, request_rx) = mpsc::channel();
        let (decision_tx, decision_rx) = mpsc::channel();
        let decision_rx = Mutex::new(decision_rx);
        let ask = move |request: PermissionRequest, _cancel: &crate::model::CancelToken| {
            request_tx.send(request).expect("request");
            decision_rx.lock().expect("lock").recv().expect("decision")
        };
        let policy = InteractivePermissionPolicy::new(
            SafeByDefault,
            crate::model::CancelToken::new(),
            Box::new(ask),
        );
        let project = Project::new(".");
        let definition = definition(ToolEffect::Execute);
        let call = call();

        let handle = thread::spawn(move || policy.check(&project, &definition, &call));
        let _request = request_rx.recv().expect("request");
        decision_tx
            .send(PermissionDecision::Ask {
                reason: "still unsure".into(),
            })
            .expect("decision");
        assert!(matches!(
            handle.join().expect("join"),
            PermissionDecision::Deny { .. }
        ));
    }

    /// 不变量 P1：模式 × effect 决策表。期望值从设计表逐格抄写，不从
    /// 实现推导——表错了这里必须红。
    #[test]
    fn mode_decision_table_matches_the_spec() {
        let expect = |mode: PermissionMode, effect: ToolEffect, allowed: bool| {
            let decision = mode_decision(mode, &definition(effect));
            if allowed {
                assert_eq!(
                    decision,
                    PermissionDecision::Allow,
                    "expected Allow for {effect} under {mode}"
                );
            } else {
                match decision {
                    PermissionDecision::Ask { reason } => {
                        assert!(
                            reason.contains(&mode.to_string()),
                            "ask reason names the mode: {reason}"
                        );
                    }
                    other => panic!("expected Ask for {effect} under {mode}, got {other:?}"),
                }
            }
        };
        use ToolEffect::{
            Destructive, Execute, ExternalRead, Network, Pure, Read, SessionWrite, Write,
        };
        // 无副作用类：所有档位放行。
        for effect in [Pure, Read, SessionWrite] {
            for mode in [
                PermissionMode::ReadOnly,
                PermissionMode::ProjectWrite,
                PermissionMode::FullAccess,
            ] {
                expect(mode, effect, true);
            }
        }
        // 文件写：RO 问，PW/FA 放行。
        expect(PermissionMode::ReadOnly, Write, false);
        expect(PermissionMode::ProjectWrite, Write, true);
        expect(PermissionMode::FullAccess, Write, true);
        // 网络与外部读（DSH 词汇表外/MCP 不设防）：RO 问，PW/FA 放行。
        for effect in [Network, ExternalRead] {
            expect(PermissionMode::ReadOnly, effect, false);
            expect(PermissionMode::ProjectWrite, effect, true);
            expect(PermissionMode::FullAccess, effect, true);
        }
        // 命令与破坏性操作（无内核沙箱时的逐次审保留面）：RO/PW 问，
        // FA 放行。
        for effect in [Execute, Destructive] {
            expect(PermissionMode::ReadOnly, effect, false);
            expect(PermissionMode::ProjectWrite, effect, false);
            expect(PermissionMode::FullAccess, effect, true);
        }
    }

    /// 不变量 P5：升级选项 = 能让当前 effect 直接放行的更宽档位集合。
    #[test]
    fn escalation_targets_offer_only_modes_that_allow_this_call() {
        use ToolEffect::{Execute, Network, Read, Write};
        // Write@RO：PW 已足够，FA 也行——两者都出现（宽度升序）。
        assert_eq!(
            escalation_targets(PermissionMode::ReadOnly, Write),
            vec![PermissionMode::ProjectWrite, PermissionMode::FullAccess]
        );
        // Execute@RO：切 PW 仍要问，只有 FA 值得出现。
        assert_eq!(
            escalation_targets(PermissionMode::ReadOnly, Execute),
            vec![PermissionMode::FullAccess]
        );
        // Network@RO：PW 即可放行（DSH 网络不门控），两档都出现。
        assert_eq!(
            escalation_targets(PermissionMode::ReadOnly, Network),
            vec![PermissionMode::ProjectWrite, PermissionMode::FullAccess]
        );
        // 门控类@PW：只剩 FA。
        assert_eq!(
            escalation_targets(PermissionMode::ProjectWrite, Execute),
            vec![PermissionMode::FullAccess]
        );
        // 已放行：无升级可言。
        assert!(escalation_targets(PermissionMode::FullAccess, Execute).is_empty());
        assert!(escalation_targets(PermissionMode::ReadOnly, Read).is_empty());
        assert!(escalation_targets(PermissionMode::ProjectWrite, Write).is_empty());
    }

    /// 不变量 P3：共享 cell 的切换在下一次 check 生效；档位语义由
    /// cell 驱动而非构造时快照。
    #[test]
    fn mode_policy_reads_the_shared_cell_on_every_check() {
        let cell = Arc::new(std::sync::RwLock::new(PermissionMode::ReadOnly));
        let policy = ModePolicy::new(Arc::clone(&cell));
        let project = Project::new(".");
        let write = definition(ToolEffect::Write);

        assert!(matches!(
            policy.check(&project, &write, &call()),
            PermissionDecision::Ask { .. }
        ));
        *cell.write().expect("mode lock") = PermissionMode::ProjectWrite;
        assert_eq!(
            policy.check(&project, &write, &call()),
            PermissionDecision::Allow
        );
        // 降级同样即时：下一次 check 回到 Ask。
        *cell.write().expect("mode lock") = PermissionMode::FullAccess;
        assert_eq!(
            policy.check(&project, &write, &call()),
            PermissionDecision::Allow
        );
        *cell.write().expect("mode lock") = PermissionMode::ReadOnly;
        assert!(matches!(
            policy.check(&project, &write, &call()),
            PermissionDecision::Ask { .. }
        ));
    }

    /// 档位说明供系统指令注入：三档都有非空、互异的文案。
    #[test]
    fn mode_guidance_covers_every_mode() {
        let texts = [
            mode_guidance(PermissionMode::ReadOnly),
            mode_guidance(PermissionMode::ProjectWrite),
            mode_guidance(PermissionMode::FullAccess),
        ];
        for text in texts {
            assert!(!text.is_empty());
        }
        assert!(texts[0] != texts[1] && texts[1] != texts[2] && texts[0] != texts[2]);
    }

    /// journal 值（`sandbox/mode` payload，DSH 词汇）：三档往返；未知值
    /// 返回 None（未来 DSH 词汇/手改不 panic，fold 保持上一已知档）。
    #[test]
    fn journal_values_round_trip_and_reject_unknown_values() {
        use PermissionMode::{FullAccess, ProjectWrite, ReadOnly};
        assert_eq!(ReadOnly.journal_value(), "read-only");
        assert_eq!(ProjectWrite.journal_value(), "workspace-write");
        assert_eq!(FullAccess.journal_value(), "danger-full-access");
        for mode in [ReadOnly, ProjectWrite, FullAccess] {
            assert_eq!(
                PermissionMode::from_journal_value(mode.journal_value()),
                Some(mode)
            );
        }
        assert_eq!(PermissionMode::from_journal_value("full-access"), None);
        assert_eq!(PermissionMode::from_journal_value("yolo"), None);
        assert_eq!(PermissionMode::from_journal_value(""), None);
    }
}
