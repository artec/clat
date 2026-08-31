//! dsh 模式的事件归约与后端编排接线（D-2 §1.3/§2/§3）——与本地
//! `run_events.rs` 对称的 App 线程侧。`DshState` 是 App 上的 dsh 会话
//! 状态（local 模式恒 None）；三根工作线程只发 `DshEvent`，本文件把
//! 事件归约进 conversation/弹框/phase/usage（§1.3 分工铁律：
//! conversation 与 DshState 只在 App 线程被碰）。
//!
//! 帧拦截次序（§3 表）：`session/title` → App.session_title（不进
//! 转录）；`turn/start|end` → running/phase；`sandbox/mode` → preset
//! 投影（latest-wins，不做落定文本匹配——宿主现行文本与设计档案引用
//! 不一致，事件类型才是稳定源）；`request/context` → contextWindow
//! 投影（models 应答无此字段）；`assistant/message` usage → DshUsageAcc
//! （DSH 三计数不相交，显式换算）；词汇违规 → 计数 + flash；其余进
//! `DshTranscript`。

use super::*;
use crate::dsh::backend::{self, DshEvent, DshTask, TaskReply};
use crate::dsh::client::DshClient;
use crate::dsh::frames::{DshFrame, event_vocabulary_violation};
use crate::dsh::transcript::DshTranscript;
use crate::interaction::AskAnswer;
use crate::session::event::SessionEvent;
use crate::tui::conversation::ConversationModel;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

/// 断线自动重连间隔（§0-2 负责人拍板：断线即自动重连，刷新循环驱
/// 动重试，状态栏同步显示重连中）。
const DSH_RECONNECT_INTERVAL: Duration = Duration::from_secs(2);

/// DSH usage 口径（§0-5）：`inputTokens` 仅未缓存部分、缓存在
/// `cacheReadTokens`（llm/types.ts:130 三计数不相交）——在适配层
/// 显式换算，不伪造 CLAT `Usage` 字段（本地口径 input 含缓存，公式
/// 不可移植）。
pub(crate) struct DshUsageAcc {
    input: u64,
    cache_read: Option<u64>,
}

impl DshUsageAcc {
    fn new() -> Self {
        Self {
            input: 0,
            cache_read: None,
        }
    }

    fn observe(&mut self, usage: &Value) {
        if let Some(input) = usage.get("inputTokens").and_then(Value::as_u64) {
            self.input = input;
        }
        // 字段缺席 = 宿主未上报缓存计数：保留旧值（不清零——与本地
        // "未上报不覆盖"同纪律）。
        if let Some(cache) = usage.get("cacheReadTokens").and_then(Value::as_u64) {
            self.cache_read = Some(cache);
        }
    }

    /// Cache 命中率 = cacheRead / (input + cacheRead)，两位小数同本地
    /// 格式；无缓存计数或分母 0 → None（段隐藏）。
    fn cache_percent(&self) -> Option<String> {
        let cached = self.cache_read?;
        let total = self.input.saturating_add(cached);
        if total == 0 {
            return None;
        }
        Some(format!("{:.2}", cached as f64 / total as f64 * 100.0))
    }

    /// Context 分子 = input + cacheRead（下一次请求的近似起点）。
    fn context_current(&self) -> u64 {
        self.input.saturating_add(self.cache_read.unwrap_or(0))
    }
}

/// 宿主 preset journal 值 → 输入框右上角显示词汇（§2.6：与 web 端
/// transform 后的产品标签一致；未知/自定义值原样显示 name）。
pub(crate) fn dsh_preset_label(preset: &str) -> String {
    match preset {
        "read-only" => "Read Only".into(),
        "workspace-write" => "Workspace Write".into(),
        "danger-full-access" => "Full Access".into(),
        other => other.to_owned(),
    }
}

/// 在途审批：rpc 关联 + 决定通道（弹框键位路径零改动——决定经通道
/// 由 App 线程排水，组装 Respond 载荷）。
pub(crate) struct DshPendingApproval {
    rpc_id: String,
    session_id: String,
    approval_id: String,
    decision_rx: Receiver<PermissionDecision>,
}

/// 在途问答：多题推进状态机（D-1 逐题推进迁此）+ 当前题应答通道。
pub(crate) struct DshPendingQuestion {
    rpc_id: String,
    session_id: String,
    questions: Vec<Value>,
    index: usize,
    answers: Vec<Value>,
    answer_rx: Receiver<AskAnswer>,
}

/// 模型展示名索引（标题栏标签，2026-08-23 负责人 dogfood 反馈：标签
/// 此前拼裸 id——`deepseek-official · deepseek-v4-flash`；web 端与
/// /model picker 显示的是 `groups[].name` / `models[].name`）。来自
/// `session.models` 应答（启动 prime + /model 刷新共用）；索引未命中
/// 诚实回落裸 id，不编名字。
#[derive(Default)]
struct ModelNameIndex {
    providers: std::collections::BTreeMap<String, String>,
    models: std::collections::BTreeMap<(String, String), String>,
    /// (provider, model) id 对 → 该模型的档位表 [(id, name)]（宿主
    /// adapter 自有词汇，展示序即宿主序；缺席 = 无推理档位）。
    efforts: std::collections::BTreeMap<(String, String), Vec<(String, String)>>,
}

impl ModelNameIndex {
    fn fold(&mut self, value: &Value) {
        let Some(groups) = value.get("groups").and_then(Value::as_array) else {
            return;
        };
        for group in groups {
            let (Some(id), Some(name)) = (
                group.get("id").and_then(Value::as_str),
                group.get("name").and_then(Value::as_str),
            ) else {
                continue;
            };
            self.providers.insert(id.to_owned(), name.to_owned());
            if let Some(models) = group.get("models").and_then(Value::as_array) {
                for model in models {
                    let Some(model_id) = model.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    if let Some(model_name) = model.get("name").and_then(Value::as_str) {
                        self.models
                            .insert((id.to_owned(), model_id.to_owned()), model_name.to_owned());
                    }
                    // 档位表（档位接入 2026-08-23）：reasoning.efforts 的
                    // adapter 自有 id + 展示名；字段缺席 = 无档位模型。
                    if let Some(efforts) = model
                        .get("reasoning")
                        .and_then(|r| r.get("efforts"))
                        .and_then(Value::as_array)
                    {
                        let table = efforts
                            .iter()
                            .filter_map(|effort| {
                                Some((
                                    effort.get("id").and_then(Value::as_str)?.to_owned(),
                                    effort
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_owned(),
                                ))
                            })
                            .collect::<Vec<_>>();
                        if !table.is_empty() {
                            self.efforts
                                .insert((id.to_owned(), model_id.to_owned()), table);
                        }
                    }
                }
            }
        }
    }

    fn provider_name(&self, id: &str) -> Option<&str> {
        self.providers.get(id).map(String::as_str)
    }

    fn model_name(&self, provider: &str, model: &str) -> Option<&str> {
        self.models
            .get(&(provider.to_owned(), model.to_owned()))
            .map(String::as_str)
    }

    /// 该模型的档位表（无档位模型 → None 或空）。
    fn efforts_for(&self, provider: &str, model: &str) -> &[(String, String)] {
        self.efforts
            .get(&(provider.to_owned(), model.to_owned()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// App 上的 dsh 会话状态（§1.1；running 不在此——App.running 是唯一
/// 事实源，键位/输入框标题/标题栏状态字全读它，防双源漂移）。
pub(crate) struct DshState {
    pub(crate) connected: bool,
    client: DshClient,
    port: u16,
    describe: Value,
    pub(crate) model_label: String,
    /// 当前选择的原始 id 对（provider, model）——标签经名字索引解析成
    /// 展示名（describe / request/context / selectModel / models.current
    /// 四路来源统一走 `set_model_ids`）。
    model_ids: (String, String),
    /// 模型目录名字索引（启动 prime 一次 + /model 刷新）。
    model_names: ModelNameIndex,
    /// 名字目录 prime 只发一次（每连接；目录是宿主全局的）。
    names_primed: bool,
    /// 当前生效档位的原始 id（档位接入 2026-08-23）：latest-wins，
    /// 来源 request/header.config.reasoningEffort（历史 fold + 实时）、
    /// models.current.reasoningEffort、selectModel 落定回执；缺席 =
    /// 该选择不带档位（标题栏档位段隐藏）。展示名经 efforts 表解析。
    current_effort: Option<String>,
    transcript: DshTranscript,
    current_session: Option<String>,
    pub(crate) session_tail: String,
    /// 词汇违规计数（INV-D8 呈现：状态栏 flash + 标题栏 ⚠）。
    pub(crate) unknown_events: usize,
    /// 会话区顶部通知条文本（断线/重连中/流错误）。
    pub(crate) banner: Option<String>,
    reconnect_at: Option<Instant>,
    reconnecting: bool,
    ws_open: bool,
    /// preset journal 值（latest-wins fold `sandbox/mode`）。
    pub(crate) preset: Option<String>,
    /// contextWindow（latest-wins fold `request/context`；缺席隐藏段）。
    pub(crate) context_window: Option<u64>,
    usage: DshUsageAcc,
    pending_approval: Option<DshPendingApproval>,
    pending_question: Option<DshPendingQuestion>,
    tasks: Sender<DshTask>,
    events: SyncSender<DshEvent>,
    /// 当前 WS 连接代际（审计 P2-2）：每次开新一对 downlink 自增；
    /// 旧代际泵的 Frame/LinkDown 一概作废（迟到断线不得把健康的新
    /// 连接再标成断线，重复流不得造成文本重复）。
    pub(crate) generation: u64,
    /// 代际的线程共享镜像：App 写、下行泵读（发现退役即静默弃连接）。
    epoch: Arc<AtomicU64>,
    /// 目标会话的整页历史装载在途（审计 P1-1）：live 帧先入
    /// `staged_events` 暂存，回执后统一补放——装载阶段由显式状态表达，
    /// 不再从「视图是否为空」反推。
    history_loading: bool,
    /// 当前会话的 workspace（会话级真来源，2026-08-24 负责人对齐：
    /// clat dsh 是宿主的终端客户端——和 3080 页面同位，显示的是会话
    /// 所属项目目录，与 clat 的本地运行目录无关；本地目录只在本地
    /// 模式有语义）。来源：Restore 回执带回的会话 cwd / 收养 Create
    /// 的目标 workspace。缺席回落 describe.cwd——那是**宿主进程目录**
    ///（host.ts:39），被我们 spawn 时恰为本地运行目录，仅作降级。
    workspace: Option<String>,
    /// 在途收养（/resume）：目标 (sessionId, workspace)——Created 回
    /// 执按 id 匹配后 workspace 跟随；失败（session-conflict 等）停
    /// 留原会话，workspace 不动。
    pending_adoption: Option<(String, String)>,
    staged_events: Vec<SessionEvent>,
    /// 本进程 spawn 的宿主句柄（退出清理拍板 2026-08-23：Drop 时带走
    /// ——探测直连的宿主从不在此）。FIX-3/CA-03：进程组句柄，Drop 经
    /// connect::terminate_dsh_host 树级清理（unix 进程组 / Windows
    /// Job Object）。
    spawned_host: Option<crate::dsh::connect::OwnedDshHost>,
}

impl Drop for DshState {
    fn drop(&mut self) {
        // 退出清理：clat dsh 退出（含 panic unwind）时带走自己 spawn 的
        // 宿主，不留孤儿——整棵进程树（FIX-3/CA-03：TERM 宽限 → 组
        // KILL → 有界收割；超宽限如实上报，不无限阻塞）。DSH journal
        // 逐事件落盘、崩溃安全由宿主自身保证。
        if let Some(child) = self.spawned_host.take()
            && let Err(warning) = child.terminate()
        {
            eprintln!("clat: dsh: {warning}");
        }
    }
}

impl DshState {
    pub(super) fn new(
        port: u16,
        describe: Value,
        tasks: Sender<DshTask>,
        events: SyncSender<DshEvent>,
    ) -> Self {
        let model_ids = (
            describe
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
            describe
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned(),
        );
        let model_label = format!("{} · {}", model_ids.0, model_ids.1);
        Self {
            connected: true,
            client: DshClient::new(port),
            port,
            describe,
            model_label,
            model_ids,
            model_names: ModelNameIndex::default(),
            names_primed: false,
            current_effort: None,
            transcript: DshTranscript::new(),
            current_session: None,
            session_tail: String::new(),
            unknown_events: 0,
            banner: None,
            reconnect_at: None,
            reconnecting: false,
            ws_open: false,
            preset: None,
            context_window: None,
            usage: DshUsageAcc::new(),
            pending_approval: None,
            pending_question: None,
            tasks,
            events,
            generation: 0,
            epoch: Arc::new(AtomicU64::new(0)),
            history_loading: false,
            staged_events: Vec::new(),
            workspace: None,
            pending_adoption: None,
            spawned_host: None,
        }
    }

    /// 收下一次连接/重连的宿主句柄：旧的（本进程 spawn 的）**整树**
    /// 带走，新的入座（None = 探测直连，不持有）。
    fn adopt_spawned_host(&mut self, child: Option<crate::dsh::connect::OwnedDshHost>) {
        if let Some(old) = self.spawned_host.take()
            && let Err(warning) = old.terminate()
        {
            eprintln!("clat: dsh: {warning}");
        }
        self.spawned_host = child;
    }

    /// 设置当前选择（原始 id）并重解析标签：展示名优先（名字索引），
    /// 未命中诚实回落裸 id（describe / request/context / selectModel /
    /// models.current 四路来源统一入口）。
    fn set_model_ids(&mut self, provider: &str, model: &str) {
        self.model_ids = (provider.to_owned(), model.to_owned());
        self.refresh_model_label();
    }

    /// 依当前 id 对 + 名字索引重解析标签。索引更新后调用让已知的裸 id
    /// 升级为展示名。
    fn refresh_model_label(&mut self) {
        let (provider_id, model_id) = self.model_ids.clone();
        let provider = self
            .model_names
            .provider_name(&provider_id)
            .unwrap_or(provider_id.as_str())
            .to_owned();
        let model = self
            .model_names
            .model_name(&provider_id, &model_id)
            .unwrap_or(model_id.as_str())
            .to_owned();
        self.model_label = format!("{provider} · {model}");
    }

    /// 折入 models 目录应答：名字索引 + `current` 校正（会话权威选择，
    /// 兼收无 request/context 的新会话）+ 标签重解析。current 自带
    /// reasoningEffort——档位一并落位（缺席 = 该选择不带档位）。
    fn fold_model_catalog(&mut self, value: &Value) {
        self.model_names.fold(value);
        let current = value.get("current");
        let effort = current
            .and_then(|c| c.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (
            current
                .and_then(|c| c.get("provider"))
                .and_then(Value::as_str),
            current.and_then(|c| c.get("model")).and_then(Value::as_str),
        ) {
            (Some(provider), Some(model)) => {
                self.set_model_ids(provider, model);
                self.current_effort = effort;
            }
            _ => self.refresh_model_label(),
        }
    }

    /// `request/context` 投影（会话级真来源，审计 P2-4 + 名字解析）：
    /// contextWindow 与 provider/model 一并落位——切换到不同模型的旧
    /// 会话后，标题栏不残留前一会话的模型（describe 是宿主级快照，
    /// 不随会话走）。
    fn apply_request_context(&mut self, event: &SessionEvent) {
        if let Some(window) = event.data.get("contextWindow").and_then(Value::as_u64) {
            self.context_window = Some(window);
        }
        if let (Some(provider), Some(model)) = (
            event.data.get("provider").and_then(Value::as_str),
            event.data.get("model").and_then(Value::as_str),
        ) {
            self.set_model_ids(provider, model);
        }
    }

    /// `request/header` 投影（档位接入 2026-08-23）：header.config 是
    /// 请求构建的权威快照——provider/model/reasoningEffort 一并重投影
    ///（历史 fold 与实时共用；切换会话后档位由此恢复，不必等下一个
    /// request/context）。effort 缺席 = 清除（该请求不带档位）。
    fn apply_request_header(&mut self, event: &SessionEvent) {
        let Some(config) = event
            .data
            .get("header")
            .and_then(|header| header.get("config"))
        else {
            return;
        };
        let effort = config
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let (Some(provider), Some(model)) = (
            config.get("provider").and_then(Value::as_str),
            config.get("model").and_then(Value::as_str),
        ) {
            self.set_model_ids(provider, model);
        }
        self.current_effort = effort;
    }

    /// 当前模型的档位表（[(id, name)]，宿主展示序；空 = 无档位模型）。
    fn current_efforts(&self) -> &[(String, String)] {
        let (provider, model) = &self.model_ids;
        self.model_names.efforts_for(provider, model)
    }

    /// 标题栏档位段：当前档位的展示名（efforts 表解析，未命中回落
    /// 裸 id——宿主自定义档位 id 原样可见；无档位 → None 隐藏段）。
    pub(super) fn effort_display(&self) -> Option<String> {
        let id = self.current_effort.as_deref()?;
        let name = self
            .current_efforts()
            .iter()
            .find(|(effort_id, _)| effort_id == id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| id.to_owned());
        Some(name)
    }

    /// 下一个档位 id（循环）：当前档位在表内 → 其后一个（回绕）；
    /// 未设/不在表（宿主自定义 id）→ 首档（宿主展示序）。空表 → None
    ///（无推理档位的模型）。
    fn next_effort(&self) -> Option<String> {
        let efforts = self.current_efforts();
        let first = efforts.first().map(|(id, _)| id.clone())?;
        let next = self
            .current_effort
            .as_deref()
            .and_then(|current| efforts.iter().position(|(id, _)| id == current))
            .map(|index| efforts[(index + 1) % efforts.len()].0.clone())
            .unwrap_or(first);
        Some(next)
    }

    /// 当前显示根：会话 workspace 优先（会话级真来源），缺席回落
    /// describe.cwd（宿主进程目录——诚实降级，标题栏第二行与状态栏
    /// 同源；/new 亦从这里继承新会话的 workspace）。
    pub(crate) fn cwd(&self) -> String {
        self.workspace.clone().unwrap_or_else(|| {
            self.describe
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
    }

    /// 断线重连的下次唤醒点（主循环 repaint deadline 融合；连接在途
    /// 或已连时不产生唤醒需求）。
    pub(crate) fn reconnect_deadline(&self) -> Option<Instant> {
        if !self.connected && !self.reconnecting {
            self.reconnect_at
        } else {
            None
        }
    }

    /// 当前（mux 过滤目标）会话 id。
    pub(crate) fn current_session(&self) -> Option<&str> {
        self.current_session.as_deref()
    }

    /// 快照测试钩子：标记 WS 已开（快照不触网，跳过真实 downlink）。
    #[cfg(test)]
    pub(crate) fn test_mark_ws_open(&mut self) {
        self.ws_open = true;
    }

    /// 测试钩子：模拟「一对新代际 downlink 已打开」（不触网——代际
    /// 过滤的判别腿用：随后旧代际的 Frame/LinkDown 必须被作废）。
    #[cfg(test)]
    pub(crate) fn test_simulate_stream_generation(&mut self) {
        self.generation += 1;
        self.epoch.store(self.generation, Ordering::SeqCst);
        self.ws_open = true;
    }

    /// 测试钩子：把重连排程拨到过去（poll_dsh 立即到期）。
    #[cfg(test)]
    pub(crate) fn test_due_reconnect_now(&mut self) {
        if self.reconnect_at.is_some() {
            self.reconnect_at = Some(Instant::now() - DSH_RECONNECT_INTERVAL);
        }
    }

    pub(crate) fn send_task(&self, task: DshTask) {
        let _ = self.tasks.send(task);
    }
}

impl App {
    /// UiEvent::Dsh 的归口（§1.3：四变体）。
    pub(super) fn handle_dsh_event(&mut self, event: DshEvent) {
        match event {
            DshEvent::Frame { generation, frame } => self.handle_dsh_frame(generation, frame),
            DshEvent::Reply(reply) => self.handle_dsh_reply(reply),
            DshEvent::LinkDown { generation, reason } => {
                self.handle_dsh_link_down(generation, reason);
            }
            DshEvent::Reconnected {
                port,
                describe,
                child,
            } => {
                self.handle_dsh_reconnected(port, describe, child);
            }
        }
    }

    /// 每帧轮询（与 expire_status/poll_loading 同排）：自动重连排程 +
    /// 审批/问答应答排水（App 线程消费通道，弹框键位路径零改动）。
    pub(super) fn poll_dsh(&mut self) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        // 自动重连（§0-2）：到点且无在途任务 → 发 Reconnect。
        if !dsh.connected
            && !dsh.reconnecting
            && dsh.reconnect_at.is_some_and(|at| at <= Instant::now())
        {
            dsh.reconnecting = true;
            dsh.reconnect_at = None;
            dsh.send_task(DshTask::Reconnect);
        }
        let approval = dsh.pending_approval.take();
        let question = dsh.pending_question.take();
        // 审批决定排水。
        if let Some(pending) = approval {
            match pending.decision_rx.try_recv() {
                Ok(decision) => {
                    let outcome = if matches!(decision, PermissionDecision::Allow) {
                        "allowed-once"
                    } else {
                        "rejected"
                    };
                    if let Some(dsh) = self.dsh.as_ref() {
                        dsh.send_task(DshTask::Respond {
                            rpc_id: pending.rpc_id,
                            result: backend::approval_response(
                                &pending.session_id,
                                &pending.approval_id,
                                outcome,
                            ),
                        });
                    }
                    self.pending_permission = None;
                    self.flash_status(format!("approval {outcome}"));
                }
                Err(TryRecvError::Empty) => {
                    if let Some(dsh) = self.dsh.as_mut() {
                        dsh.pending_approval = Some(pending);
                    }
                }
                Err(TryRecvError::Disconnected) => self.pending_permission = None,
            }
        }
        // 问答应答排水（多题推进）。
        if let Some(pending) = question {
            match pending.answer_rx.try_recv() {
                Ok(answer) => self.dsh_advance_question(pending, answer),
                Err(TryRecvError::Empty) => {
                    if let Some(dsh) = self.dsh.as_mut() {
                        dsh.pending_question = Some(pending);
                    }
                }
                Err(TryRecvError::Disconnected) => {}
            }
        }
    }

    /// 一题应答落位：累积 → 下一题弹框 / 末题发全量；Declined → 整个
    /// rpc 取消（Esc 语义，宿主 claimQuestion 走 cancelled 分支）。
    fn dsh_advance_question(&mut self, mut pending: DshPendingQuestion, answer: AskAnswer) {
        let question = pending
            .questions
            .get(pending.index)
            .cloned()
            .unwrap_or(Value::Null);
        let id = question.get("id").cloned().unwrap_or(Value::Null);
        if matches!(answer, AskAnswer::Declined) {
            if let Some(dsh) = self.dsh.as_mut() {
                dsh.send_task(DshTask::Respond {
                    rpc_id: pending.rpc_id,
                    result: backend::question_cancelled_response(),
                });
            }
            self.pending_ask_user = None;
            self.flash_status("question cancelled");
            return;
        }
        let entry = match answer {
            AskAnswer::Selected(label) => backend::question_answer_entry(&id, vec![label], None),
            AskAnswer::Custom(text) => backend::question_answer_entry(&id, Vec::new(), Some(text)),
            AskAnswer::Declined => unreachable!("handled above"),
        };
        pending.answers.push(entry);
        if pending.index + 1 < pending.questions.len() {
            pending.index += 1;
            // 下一题弹框（§2.5：(i/N) 前缀融入题面；单题不加——与 local
            // ask 弹框同构）。
            let next = pending.questions[pending.index].clone();
            let (answer_tx, answer_rx) = mpsc::channel();
            self.pending_ask_user = Some(dsh_ask_dialog(
                next,
                pending.index,
                pending.questions.len(),
                answer_tx,
            ));
            pending.answer_rx = answer_rx;
            if let Some(dsh) = self.dsh.as_mut() {
                dsh.pending_question = Some(pending);
            }
            return;
        }
        let session_id = pending.session_id.clone();
        let rpc_id = pending.rpc_id.clone();
        let answers = std::mem::take(&mut pending.answers);
        if let Some(dsh) = self.dsh.as_mut() {
            dsh.send_task(DshTask::Respond {
                rpc_id,
                result: backend::question_answer_response(&session_id, answers),
            });
        }
        self.pending_ask_user = None;
        self.flash_status("question answered");
    }

    // ---- 连接生命周期 ----

    fn handle_dsh_link_down(&mut self, generation: u64, reason: String) {
        if self.dsh_connect.is_some() {
            // 初始连接失败：报错退出（D-1 语义——启动是单次尝试）。
            self.close_error = Some(format!("clat: dsh: {reason}"));
            self.should_quit = true;
            return;
        }
        // 代际过滤（审计 P2-2）：旧代际泵的迟到断线与当前连接无关，
        // 作废——否则瞬时 WS 故障重连成功后，旧流的死讯会把健康的
        // 新连接再标成断线，引发重连抖动。
        if let Some(dsh) = self.dsh.as_ref()
            && dsh.generation != generation
        {
            return;
        }
        let mut flash = None;
        if let Some(dsh) = self.dsh.as_mut()
            && dsh.connected
        {
            dsh.connected = false;
            dsh.reconnect_at = Some(Instant::now() + DSH_RECONNECT_INTERVAL);
            dsh.banner = Some(format!("disconnected ({reason}) — reconnecting…"));
            flash = Some("disconnected — reconnecting…".to_owned());
        }
        // running 归约以 turn 帧为准；断线期间无帧可依，先落 false。
        self.running = false;
        self.phases.finish();
        if let Some(message) = flash {
            self.flash_status(message);
        }
    }

    fn handle_dsh_reconnected(
        &mut self,
        port: u16,
        describe: Value,
        child: Option<crate::dsh::connect::OwnedDshHost>,
    ) {
        if self.dsh_connect.is_some() {
            // 初始连接落地：消费连接占位，构造 DshState、起 HTTP worker
            //（复用连接期的 DshEvent 通道与 UiEvent 转发线程——由 run()
            // 安装），恢复最近活跃会话（§1.0 启动序列）。
            let (_, events_tx) = self.dsh_connect.take().expect("checked above");
            let (task_tx, task_rx) = mpsc::channel::<DshTask>();
            backend::spawn_worker(DshClient::new(port), port, task_rx, events_tx.clone());
            let mut dsh = DshState::new(port, describe, task_tx, events_tx);
            dsh.adopt_spawned_host(child);
            self.default_status = abbreviate_home(std::path::Path::new(&dsh.cwd()));
            // 拍板 A：优先恢复自己上次打开的会话（记忆缺席/已删回落
            // 宿主列表头）。
            let prefer = crate::dsh::last_session::read_last_session_at(&self.dsh_memory_path);
            dsh.send_task(DshTask::Restore { prefer });
            self.dsh = Some(dsh);
            self.status = self.default_status.clone();
            self.status_until = None;
            self.flash_status("connected — restoring session…");
            return;
        }
        // 重连成功：换 client/port/describe，App 线程重开一对新代际
        // WS（旧泵自此作废；mux 基线自带 pending 审批/问答重放，弹框
        // 自然恢复）。
        if let Some(dsh) = self.dsh.as_mut() {
            dsh.port = port;
            dsh.client = DshClient::new(port);
            dsh.describe = describe;
            // 宿主句柄的归属甄别（审计 P1-4）：只有本次重连真的 respawn
            // 了宿主（child = Some）才换句柄；探测直连（child = None）
            // 时命中端口的很可能正是我们自己 spawn 且仍健在的宿主——
            // 保留原句柄，绝不能把瞬时的 WS 故障升级成「杀掉自家宿主
            // 再复活」。若原宿主确已死、端口被外部宿主接管，保留的
            // 旧句柄在 Drop 时 kill 只是无害的失败操作。
            if child.is_some() {
                dsh.adopt_spawned_host(child);
            }
            // 重连后的 describe 是宿主级快照：先按它落 id（名字索引
            // 跨重连保留，标签随之重解析）。
            let describe_ids = (
                dsh.describe
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                dsh.describe
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            if let (Some(provider), Some(model)) = describe_ids {
                dsh.set_model_ids(&provider, &model);
            }
        }
        let outcome = self.dsh_open_downlink_pair();
        let mut flash = None;
        if let Some(dsh) = self.dsh.as_mut() {
            match outcome {
                Ok(()) => {
                    dsh.ws_open = true;
                    dsh.connected = true;
                    dsh.reconnecting = false;
                    dsh.reconnect_at = None;
                    dsh.banner = None;
                    flash = Some("reconnected".to_owned());
                }
                Err(message) => {
                    dsh.connected = false;
                    dsh.reconnecting = false;
                    dsh.reconnect_at = Some(Instant::now() + DSH_RECONNECT_INTERVAL);
                    dsh.banner = Some(format!("reconnect failed ({message}) — reconnecting…"));
                }
            }
        }
        if let Some(message) = flash {
            self.flash_status(message);
        }
    }

    /// 启动序列/切换共用的开 WS 步骤（设计 §1.0：历史装载后开双 WS，
    /// 等 Subscribed → baseline）。
    fn dsh_open_downlinks(&mut self) {
        if self.dsh.as_ref().is_none_or(|dsh| dsh.ws_open) {
            return;
        }
        match self.dsh_open_downlink_pair() {
            Ok(()) => {
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.ws_open = true;
                }
            }
            Err(message) => {
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.connected = false;
                    dsh.reconnect_at = Some(Instant::now() + DSH_RECONNECT_INTERVAL);
                    dsh.banner = Some(format!("stream failed ({message}) — reconnecting…"));
                }
            }
        }
    }

    /// 开一对新代际 downlink（mux + host）。自增代际并写入 epoch——
    /// 旧代际泵自此退役（其后续帧/断线被 App 按代际作废，泵线程在
    /// 下一条消息后自行退出弃连接）。部分失败（mux 成、host 败）同样
    /// 已完成自增：孤儿的 mux 泵属当前代际，数据仍可用；下一次重连
    /// 再自增后自然作废。
    fn dsh_open_downlink_pair(&mut self) -> Result<(), String> {
        let Some(dsh) = self.dsh.as_mut() else {
            return Ok(());
        };
        dsh.generation += 1;
        dsh.epoch.store(dsh.generation, Ordering::SeqCst);
        let generation = dsh.generation;
        let port = dsh.port;
        backend::open_downlink(port, "/api/events.mux", &dsh.events, generation, &dsh.epoch)
            .and_then(|()| {
                backend::open_downlink(
                    port,
                    "/api/events.host",
                    &dsh.events,
                    generation,
                    &dsh.epoch,
                )
            })
    }

    // ---- HTTP 应答归约 ----

    fn handle_dsh_reply(&mut self, reply: TaskReply) {
        match reply {
            TaskReply::Restored { session, cwd } => match session {
                Some(session) => {
                    // 恢复带回会话自己的 workspace——显示与 /new 继承
                    // 都以它为准（describe.cwd 是宿主进程目录，此前被
                    // 误当会话项目显示）。
                    // FIX-4/CA-04：workspace 是会话级状态，`Restored` 按
                    // 回执**覆盖**——None 也是覆盖（清除 → 回落
                    // describe.cwd）。不清除会让上一会话的 workspace
                    // 遮住回落，并被随后的 /new 继承到错误目录。
                    match cwd {
                        Some(cwd) => self.dsh_set_workspace(cwd),
                        None => self.dsh_clear_workspace(),
                    }
                    self.dsh_switch_session(session);
                }
                None => {
                    self.flash_status("no previous session — /new to start one");
                    self.dsh_open_downlinks();
                }
            },
            TaskReply::History { session, events } => self.dsh_load_history(session, events),
            TaskReply::Created(session) => {
                // 收养在途：按回执 id 匹配，workspace 跟随目标会话；
                // 不匹配（/new 的新 id / 陈旧残留）清空不跟随。
                let adoption = self
                    .dsh
                    .as_mut()
                    .and_then(|dsh| dsh.pending_adoption.take());
                if let Some((pending_id, cwd)) = adoption
                    && pending_id == session
                {
                    self.dsh_set_workspace(cwd);
                }
                self.dsh_switch_session(session);
                self.flash_status("session created");
            }
            TaskReply::Models(value) => {
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.fold_model_catalog(&value);
                }
                match crate::tui::model_editor::dsh_model_data_from(&value) {
                    Some(data) => {
                        self.picker = Some(ModelPicker::new_dsh(data));
                        self.flash_status("select a model");
                    }
                    None => self.flash_status("no models available"),
                }
            }
            TaskReply::ModelNames(value) => {
                // 启动 prime 回执：只折名字索引与 current 校正（标签升
                // 级为展示名），不开 picker、无 flash——装饰性获取。
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.fold_model_catalog(&value);
                }
            }
            TaskReply::Selected {
                provider,
                model,
                effort,
            } => {
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.set_model_ids(&provider, &model);
                    dsh.current_effort = effort;
                }
                self.flash_status("model selected");
            }
            TaskReply::Status(message) => self.flash_status(message),
            TaskReply::Failed(message) => {
                // create 收养失败（如 session-conflict）在此落：停留原会话。
                // 装载在途失败：解除暂存态并补放 live 帧（fail-soft——
                // 宁可缺历史页 + 报错，不让 loading 悬挂，审计 P2-1）。
                self.dsh_abort_history_loading();
                self.flash_status(format!("error: {message}"));
                // 启动链 fail-soft：Restore/History 失败时流尚未打开，
                // 照常开流（HTTP 一次失败不代表 WS 不可用）；开不了则
                // 由其错误路径转入重连机制，不再永久停在 restoring。
                self.dsh_open_downlinks();
            }
            TaskReply::ReconnectFailed(message) => {
                // 重连尝试失败（审计 P1-3）：复位单飞守卫并重新排程——
                // 否则 reconnecting 永真，自动重连一次失败后永久停摆。
                if let Some(dsh) = self.dsh.as_mut() {
                    dsh.reconnecting = false;
                    if !dsh.connected {
                        dsh.reconnect_at = Some(Instant::now() + DSH_RECONNECT_INTERVAL);
                        dsh.banner = Some(format!("reconnect failed ({message}) — retrying…"));
                    }
                }
                self.flash_status(format!("reconnect failed: {message}"));
            }
            TaskReply::Reconnected { .. } => {
                unreachable!("worker reroutes Reconnected to DshEvent")
            }
        }
    }

    /// 历史装载（首次整页重建 + 投影 fold；间隙补齐只追加未见事件）。
    /// 装载阶段由 `history_loading` 显式表达（审计 P1-1）：切换/启动
    /// 的整页回执必走重建支；期间暂存的 live 帧在重建后统一补放
    /// （先到 live 帧后到回执的竞态不再依赖「视图是否为空」猜测）。
    fn dsh_load_history(&mut self, session: String, events: Vec<SessionEvent>) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.current_session.as_deref() != Some(session.as_str()) {
            return;
        }
        let was_open = dsh.ws_open;
        let mut staged = None;
        if dsh.history_loading {
            dsh.history_loading = false;
            staged = Some(std::mem::take(&mut dsh.staged_events));
            dsh.transcript.load_history(&mut self.conversation, &events);
        } else if self.conversation.is_empty() {
            // 空视图的补拉（Subscribed 基线锚定后触发）：无从判隙，
            // 整页重放最稳。
            dsh.transcript.load_history(&mut self.conversation, &events);
        } else {
            let mut unseen = Vec::new();
            for event in &events {
                if dsh.transcript.gap_before(event).is_some() {
                    unseen.push(event.clone());
                }
            }
            for event in &unseen {
                dsh.transcript.apply(&mut self.conversation, event);
            }
        }
        // 投影 fold 幂等（latest-wins）：整页重放与间隙补齐统一走全量。
        self.dsh_fold_session_projections(&events);
        if let Some(staged) = staged {
            // 暂存的 live 帧在整页之上补放（已入页的会被 seq 判重跳过）。
            for event in staged {
                self.dsh_reduce_session_event(&session, event);
            }
        }
        if !was_open {
            self.dsh_open_downlinks();
            self.flash_status("ready");
        }
    }

    /// 装载失败的中止（审计 P2-1）：解除暂存态，已暂存的 live 帧走
    /// 完整归约补放——历史页缺席但近期活动可见，loading 不悬挂。
    fn dsh_abort_history_loading(&mut self) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if !dsh.history_loading {
            return;
        }
        dsh.history_loading = false;
        let staged = std::mem::take(&mut dsh.staged_events);
        if let Some(session) = dsh.current_session.clone() {
            for event in staged {
                self.dsh_reduce_session_event(&session, event);
            }
        }
    }

    /// 会话级投影 fold（latest-wins）：title / preset / contextWindow /
    /// usage——装载与间隙补齐共用（§2.6 步骤 4 的装载侧）。
    fn dsh_fold_session_projections(&mut self, events: &[SessionEvent]) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        for event in events {
            match event.event_type.as_str() {
                "session/title" => {
                    if let Some(title) = event.data.get("title").and_then(Value::as_str) {
                        self.session_title = Some(title.to_owned());
                    }
                }
                "sandbox/mode" => {
                    if let Some(mode) = event.data.get("mode").and_then(Value::as_str) {
                        dsh.preset = Some(mode.to_owned());
                    }
                }
                "request/context" => {
                    dsh.apply_request_context(event);
                }
                "request/header" => {
                    dsh.apply_request_header(event);
                }
                "assistant/message" => {
                    if let Some(usage) = event.data.get("usage") {
                        dsh.usage.observe(usage);
                    }
                }
                _ => {}
            }
        }
    }

    /// 会话切换（§2.6 七步的 ③④⑤⑥⑦ 骨干；② 由调用方先发 Create
    /// 收养任务、Created 回执进入此处；①在途弹框关闭且不 respond）。
    fn dsh_switch_session(&mut self, session: String) {
        // 拍板 A：切换即记住（下次启动优先回它；写失败 fail-soft，
        // 下次回落列表头）。restore/收养//new 全部经此，单点写入。
        let memory_path = self.dsh_memory_path.clone();
        crate::dsh::last_session::remember_last_session_at(&memory_path, &session);
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        dsh.current_session = Some(session.clone());
        dsh.session_tail = session
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        dsh.transcript = DshTranscript::new();
        dsh.unknown_events = 0;
        dsh.preset = None;
        dsh.context_window = None;
        // 档位随目标会话重投影（历史 request/header fold 恢复）。
        dsh.current_effort = None;
        dsh.usage = DshUsageAcc::new();
        dsh.pending_approval = None;
        dsh.pending_question = None;
        self.pending_permission = None;
        self.pending_ask_user = None;
        self.session_title = None;
        self.running = false;
        self.phases.finish();
        // 视图立即接管为空白（审计 P1-1）：旧会话内容绝不显示在新会话
        // 标题下——此前残留内容会把整页装载误判成间隙补齐而全数跳过
        // （fresh transcript 的 gap_before 恒 None），形成跨会话串线。
        self.conversation = ConversationModel::new();
        self.conversation_scroll_from_bottom = 0;
        // 装载阶段显式化：整页回执前 live 帧入暂存区，回执后统一补放。
        dsh.history_loading = true;
        dsh.staged_events.clear();
        let session_for_names = session.clone();
        dsh.send_task(DshTask::History { session });
        // 名字目录 prime（每连接一次）：标签从裸 id 升级为展示名
        //（2026-08-23 负责人 dogfood：`deepseek-official ·
        // deepseek-v4-flash` 应显示 `DeepSeek · <模型 Name>`，与 web 端
        // 同源）。目录是宿主全局的，切换会话不重复拉。
        if !dsh.names_primed {
            dsh.names_primed = true;
            dsh.send_task(DshTask::ModelNames {
                session: session_for_names,
            });
        }
        self.flash_status("loading history…");
    }

    // ---- WS 帧归约 ----

    fn handle_dsh_frame(&mut self, generation: u64, frame: DshFrame) {
        // 代际过滤（审计 P2-2）：只接受当前代际的帧——旧泵的迟到帧
        // 一概作废（重复流不再能造成文本重复/计数扰动）。
        if let Some(dsh) = self.dsh.as_ref()
            && dsh.generation != generation
        {
            return;
        }
        match frame {
            DshFrame::Subscribed {
                session_id,
                last_seq,
            } => {
                if let Some(dsh) = self.dsh.as_mut()
                    && dsh.current_session.as_deref() == Some(session_id.as_str())
                {
                    dsh.transcript.baseline(last_seq);
                }
            }
            DshFrame::SessionEvent { session_id, event } => {
                self.dsh_apply_session_event(session_id, event);
            }
            DshFrame::ApprovalRequested {
                rpc_id,
                session_id,
                approval_id,
                tool_name,
                call_id,
                reason,
            } => {
                // 只认当前会话的审批（审计 P1-2）：/resume 后宿主同时
                // attached 多个会话，后台会话的工具审批不得弹进当前
                // 视图——更不能被当前用户代答。留在宿主侧待其他客户端
                // 或切回该会话时处理。
                if self
                    .dsh
                    .as_ref()
                    .is_some_and(|dsh| dsh.current_session.as_deref() == Some(session_id.as_str()))
                {
                    self.dsh_open_approval(
                        rpc_id,
                        session_id,
                        approval_id,
                        tool_name,
                        call_id,
                        reason,
                    )
                }
            }
            DshFrame::ApprovalResolved {
                session_id,
                approval_id,
                ..
            } => {
                // 精确关联（审计 P1-2）：会话 + approvalId 双匹配才关框；
                // 别的会话/别的审批的落定不许动当前弹框。
                let mine = self.dsh.as_ref().is_some_and(|dsh| {
                    dsh.pending_approval.as_ref().is_some_and(|pending| {
                        pending.session_id == session_id && pending.approval_id == approval_id
                    })
                });
                if mine {
                    if let Some(dsh) = self.dsh.as_mut() {
                        dsh.pending_approval = None;
                    }
                    self.pending_permission = None;
                }
            }
            DshFrame::QuestionRequested {
                rpc_id,
                session_id,
                questions,
            } => {
                // 同审批：只认当前会话的问答。
                if self
                    .dsh
                    .as_ref()
                    .is_some_and(|dsh| dsh.current_session.as_deref() == Some(session_id.as_str()))
                {
                    self.dsh_open_question(rpc_id, session_id, questions)
                }
            }
            DshFrame::QuestionResolved {
                session_id, rpc_id, ..
            } => {
                // 精确关联（审计 P1-2）：会话 + rpcId 双匹配才清问答态。
                let mine = self.dsh.as_ref().is_some_and(|dsh| {
                    dsh.pending_question.as_ref().is_some_and(|pending| {
                        pending.session_id == session_id && pending.rpc_id == rpc_id
                    })
                });
                if mine {
                    if let Some(dsh) = self.dsh.as_mut() {
                        dsh.pending_question = None;
                    }
                    self.pending_ask_user = None;
                }
            }
            DshFrame::Queue { session_id, items } => {
                // §3：steering 计数校正——宿主队列里 placement ==
                // "steering" 的条目数是权威值，本地回显只多不少时修剪。
                let steering_count = items
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter(|item| {
                                item.get("placement").and_then(Value::as_str) == Some("steering")
                            })
                            .count()
                    })
                    .unwrap_or(0);
                let mine = self
                    .dsh
                    .as_ref()
                    .is_some_and(|dsh| dsh.current_session.as_deref() == Some(session_id.as_str()));
                if mine {
                    // 逐条弹出（discard 是全清语义——run 终态专用）；
                    // 文本弃置：宿主已权威收编/丢弃该队列项。
                    while self.conversation.pending_steering_count() > steering_count {
                        self.conversation.recall_pending_steering();
                    }
                }
            }
            DshFrame::SessionStatus {
                session_id,
                running,
            } => {
                // host 帧面（辅助源）：归约冲突以 turn 帧为准，故只在
                // 无冲突风险时同步（直接赋值，turn 帧随到随覆盖）。
                if let Some(dsh) = self.dsh.as_ref()
                    && dsh.current_session.as_deref() == Some(session_id.as_str())
                {
                    self.running = running;
                }
            }
            DshFrame::SessionAdded { .. } | DshFrame::SessionRemoved { .. } => {}
            DshFrame::StreamError { message } => {
                let mut flash = None;
                if let Some(dsh) = self.dsh.as_mut()
                    && dsh.connected
                {
                    dsh.connected = false;
                    dsh.reconnect_at = Some(Instant::now() + DSH_RECONNECT_INTERVAL);
                    dsh.banner = Some(format!("stream error ({message}) — reconnecting…"));
                    flash = Some("stream error — reconnecting…".to_owned());
                }
                if let Some(text) = flash {
                    self.flash_status(text);
                }
            }
            DshFrame::Unknown { .. } => {}
        }
    }

    fn dsh_apply_session_event(&mut self, session_id: String, event: SessionEvent) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.current_session.as_deref() != Some(session_id.as_str()) {
            return;
        }
        if dsh.history_loading {
            // 整页装载在途（审计 P1-1 竞态腿）：live 帧先暂存——与整页
            // 重建交错应用会造成重复/错序（历史页快照可能不含该帧，
            // 重建又会把已应用的冲掉）。回执到达后统一补放。
            dsh.staged_events.push(event);
            return;
        }
        self.dsh_reduce_session_event(&session_id, event);
    }

    /// 单条会话事件的完整归约（session 归属与暂存Redirect由调用方完成）。
    fn dsh_reduce_session_event(&mut self, session_id: &str, event: SessionEvent) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        // turn 结束的铃（见 turn/end 拦截）——推迟到 dsh 借用结束后落。
        let mut ring_bell = false;
        // INV-D8：未知非 ignorable 类型计数 + 可见提示，绝不静默。
        if event_vocabulary_violation(session_id, &event).is_some() {
            dsh.unknown_events += 1;
        }
        // INV-D5：间隙 → 拉历史补齐。
        if dsh.transcript.gap_before(&event).is_some() {
            dsh.send_task(DshTask::History {
                session: session_id.to_owned(),
            });
        }
        // 投影拦截（§3：先拦，title 不进转录）。
        match event.event_type.as_str() {
            "session/title" => {
                if let Some(title) = event.data.get("title").and_then(Value::as_str) {
                    self.session_title = Some(title.to_owned());
                }
            }
            "turn/start" => {
                self.running = true;
                self.phases.model_requested();
            }
            "turn/end" => {
                self.running = false;
                self.phases.finish();
                // 铃（2026-08-24 负责人 dogfood：dsh 模式此前 turn 结束
                // 不响——本地 run 结束响、dsh 只有审批/问答弹框响，后台
                // 等待落空）。aborted = 用户主动取消，同本地语义不响；
                // completed/error/max-tokens/blocked 都响（B-1 的焦点
                // 三态在 notify 内统一裁决）。实际鸣响推迟到函数末尾
                //（notify 需 &self，与 dsh 的借用互斥）。
                let kind = event
                    .data
                    .get("reason")
                    .and_then(|reason| reason.get("kind"))
                    .and_then(Value::as_str);
                if kind != Some("aborted") {
                    ring_bell = true;
                }
            }
            "sandbox/mode" => {
                if let Some(mode) = event.data.get("mode").and_then(Value::as_str) {
                    dsh.preset = Some(mode.to_owned());
                }
            }
            "request/context" => {
                dsh.apply_request_context(&event);
            }
            "request/header" => {
                dsh.apply_request_header(&event);
            }
            _ => {}
        }
        // phase 派生（§2.4：chunk type 词汇以事件目录为准——text-delta
        // / reasoning-delta；未知 type 维持当前 phase）。
        match event.event_type.as_str() {
            "assistant/chunk" => {
                let chunk_type = event
                    .data
                    .get("chunk")
                    .and_then(|chunk| chunk.get("type"))
                    .and_then(Value::as_str);
                match chunk_type {
                    Some("text-delta") => self.phases.advance(Phase::Responding),
                    Some("reasoning-delta") => self.phases.advance(Phase::Thinking),
                    _ => {}
                }
            }
            "tool/call" => self.phases.advance(Phase::ExecutingTools),
            _ => {}
        }
        // usage 投影（assistant/message 的 usage 字段）。
        if event.event_type == "assistant/message"
            && let Some(usage) = event.data.get("usage")
        {
            dsh.usage.observe(usage);
        }
        if event.event_type != "session/title" {
            dsh.transcript.apply(&mut self.conversation, &event);
        }
        if ring_bell {
            self.notify();
        }
    }

    // ---- 审批/问答弹框（既有类型 + 通道桥接，INV-U9） ----

    #[allow(clippy::too_many_arguments)]
    fn dsh_open_approval(
        &mut self,
        rpc_id: String,
        session_id: String,
        approval_id: String,
        tool_name: String,
        call_id: Option<String>,
        reason: Option<String>,
    ) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.pending_approval.is_some() || self.pending_permission.is_some() {
            return;
        }
        // §2.5 字段映射（core 拥有契约：from_host_approval——effect 归
        // Write、arguments 空）；escalations 空 = w/f 升级键不可达。
        let (decision_tx, decision_rx) = mpsc::channel();
        let request = PermissionRequest::from_host_approval(
            tool_name,
            reason.unwrap_or_default(),
            call_id.unwrap_or_default(),
        );
        self.pending_permission = Some(PendingPermission {
            request,
            decision_tx,
            argument_scroll: 0,
            argument_page_size: 0,
            argument_line_count: 0,
            reviewed_through: 0,
            reviewed_to_end: true,
            escalations: Vec::new(),
        });
        dsh.pending_approval = Some(DshPendingApproval {
            rpc_id,
            session_id,
            approval_id,
            decision_rx,
        });
        self.phases.finish();
        self.notify();
        self.flash_status("approval requested");
    }

    fn dsh_open_question(&mut self, rpc_id: String, session_id: String, questions: Value) {
        let questions = questions.as_array().cloned().unwrap_or_default();
        if questions.is_empty() {
            return;
        }
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        if dsh.pending_question.is_some() || self.pending_ask_user.is_some() {
            return;
        }
        let first = questions[0].clone();
        let (answer_tx, answer_rx) = mpsc::channel();
        self.pending_ask_user = Some(dsh_ask_dialog(first, 0, questions.len(), answer_tx));
        dsh.pending_question = Some(DshPendingQuestion {
            rpc_id,
            session_id,
            questions,
            index: 0,
            answers: Vec::new(),
            answer_rx,
        });
        self.phases.finish();
        self.notify();
        self.flash_status("question requested");
    }

    /// /resume 的 dsh 数据源（§2.5 返工终版）：数据面（INV-D6
    /// fail-soft）缺席 → API 兜底（D-1 行为：Restore 切最近活跃）。
    /// 分组与打开定位（当前工作区组；无活跃会话→最近活跃组）由
    /// picker 构造器内部完成。
    fn dsh_open_resume_picker(&mut self, current: Option<String>) {
        let home = crate::dsh::files::dsh_home().unwrap_or_default();
        let rows = crate::dsh::files::read_sessions(&home);
        let Some(dsh) = self.dsh.as_ref() else {
            return;
        };
        match rows {
            Some(rows) if !rows.is_empty() => {
                self.session_picker = Some(SessionPicker::new_dsh(rows, current));
            }
            _ => {
                let prefer = crate::dsh::last_session::read_last_session_at(&self.dsh_memory_path);
                dsh.send_task(DshTask::Restore { prefer });
                self.flash_status("listing sessions…");
            }
        }
    }

    /// §2.6 七步切换的第②步：create 收养式（§0-1 负责人拍板）——
    /// `session.create { sessionId, cwd: 目标 workspace_path }`（web
    /// 客户端切换同款协议；ensureSession 校验 cwd 必须等于目标会话
    /// 记录的 cwd，api-proxy.ts:1587）。③-⑦由 Created 回执
    ///（dsh_switch_session）落地——workspace 经 pending_adoption 按
    /// 回执 id 匹配跟随；失败（session-conflict 等）→ Failed 回执
    /// flash，停留原会话、workspace 不动。
    pub(super) fn dsh_adopt_session(&mut self, row: crate::tui::session_picker::DshResumeRow) {
        let Some(dsh) = self.dsh.as_mut() else {
            return;
        };
        dsh.pending_adoption = Some((row.session_id.clone(), row.workspace_path.clone()));
        dsh.send_task(DshTask::Create {
            session_id: Some(row.session_id),
            cwd: Some(row.workspace_path),
        });
        self.flash_status("switching session…");
    }

    /// 会话 workspace 落位（恢复带回 / 收养跟随）：常驻状态行改为
    /// 会话项目目录——clat dsh 是宿主的终端客户端（2026-08-24 负责
    /// 人对齐），与 clat 的本地运行目录无关；标题栏第二行读 cwd()
    /// 自然跟随。
    fn dsh_set_workspace(&mut self, cwd: String) {
        if let Some(dsh) = self.dsh.as_mut() {
            dsh.workspace = Some(cwd.clone());
        }
        self.default_status = abbreviate_home(std::path::Path::new(&cwd));
    }

    /// FIX-4/CA-04：清除会话级 workspace（`Restored` 无 cwd）——回落
    /// describe.cwd（宿主进程目录，仅作降级显示与 /new 继承）。
    fn dsh_clear_workspace(&mut self) {
        let fallback = self.dsh.as_mut().map(|dsh| {
            dsh.workspace = None;
            dsh.cwd()
        });
        if let Some(fallback) = fallback {
            self.default_status = abbreviate_home(std::path::Path::new(&fallback));
        }
    }

    // ---- 输入提交（§2.3 分流落点：actions.rs 首行判 dsh 后转此） ----

    pub(super) fn submit_dsh(&mut self, text: String) {
        if text.starts_with('/') {
            self.dispatch_dsh_command(&text);
            return;
        }
        let Some(dsh) = self.dsh.as_ref() else {
            self.flash_status("connecting to dsh…");
            return;
        };
        let Some(session) = dsh.current_session.clone() else {
            self.flash_status("no session — start one with /new");
            return;
        };
        let steer = self.running;
        dsh.send_task(DshTask::Prompt {
            session,
            steer,
            text: text.clone(),
        });
        if steer {
            // §2.3：running 态 Enter = steer，本地回显（session/queue 帧
            // 到达时按 steering 数修剪——advisory 语义同 local）。
            self.conversation.push_pending_steering(text);
            self.flash_status("steering…");
        } else {
            self.flash_status("sending…");
        }
    }

    /// dsh 命令面（§2.5 终表；strip_prefix 匹配形状——命令分发不经
    /// TrustedProjectApplication，属 dsh 自有映射，架构门禁另有作用域）。
    fn dispatch_dsh_command(&mut self, input: &str) {
        let (head, args) = match input.split_once(' ') {
            Some((head, args)) => (head, args.trim()),
            None => (input, ""),
        };
        let name = head.strip_prefix('/').unwrap_or(head);
        let Some(dsh) = self.dsh.as_mut() else {
            self.flash_status("connecting to dsh…");
            return;
        };
        match name {
            "quit" | "exit" => self.should_quit = true,
            "new" => {
                let cwd = dsh.cwd();
                dsh.send_task(DshTask::Create {
                    session_id: None,
                    cwd: (!cwd.is_empty()).then_some(cwd),
                });
                self.flash_status("creating session…");
            }
            "rename" => {
                if args.is_empty() {
                    // RenameDialog 复用（§2.5）：预填当前标题，提交走
                    // dialog 的 dsh 分支（session.rename）。
                    let prefill = self.session_title.clone().unwrap_or_default();
                    self.rename_dialog = Some(RenameDialog::new(&prefill));
                } else if let Some(session) = dsh.current_session().map(str::to_owned) {
                    dsh.send_task(DshTask::Rename {
                        session,
                        title: args.to_owned(),
                    });
                    self.session_title = Some(args.to_owned());
                    self.flash_status("renaming…");
                } else {
                    self.flash_status("no active session");
                }
            }
            "resume" => {
                let current = dsh.current_session().map(str::to_owned);
                self.dsh_open_resume_picker(current);
            }
            "model" => {
                let session = dsh.current_session().map(str::to_owned);
                dsh.send_task(DshTask::Models { session });
                self.flash_status("loading models…");
            }
            "perm" | "permission" => {
                let current = dsh
                    .preset
                    .as_deref()
                    .and_then(PermissionMode::from_journal_value)
                    .unwrap_or_default();
                self.permission_picker = Some(
                    crate::tui::permission_picker::PermissionPicker::new_dsh(current),
                );
            }
            "clear" => {
                // 纯本地：清空会话视图（与 local /clear 语义一致，零 API）。
                self.conversation = ConversationModel::new();
                self.conversation_scroll_from_bottom = 0;
                self.flash_status("cleared");
            }
            "help" => {
                // InfoDialog(Help) 复用（§2.5）：命令清单按 dsh 表标注
                // 可用性（映射/本地 + 置灰全集不出现——弹框只列可用集）。
                self.help_commands = vec![
                    CommandInfo {
                        name: "new".into(),
                        aliases: Vec::new(),
                        description: "start a fresh session on the host".into(),
                    },
                    CommandInfo {
                        name: "resume".into(),
                        aliases: Vec::new(),
                        description: "pick a session (grouped by workspace)".into(),
                    },
                    CommandInfo {
                        name: "model".into(),
                        aliases: Vec::new(),
                        description: "show and switch the host's model groups".into(),
                    },
                    CommandInfo {
                        name: "perm".into(),
                        aliases: vec!["permission".into()],
                        description: "switch the host permission preset".into(),
                    },
                    CommandInfo {
                        name: "rename".into(),
                        aliases: Vec::new(),
                        description: "rename the current session".into(),
                    },
                    CommandInfo {
                        name: "clear".into(),
                        aliases: Vec::new(),
                        description: "clear the local conversation view".into(),
                    },
                    CommandInfo {
                        name: "quit".into(),
                        aliases: vec!["exit".into()],
                        description: "leave the TUI".into(),
                    },
                ];
                self.info_dialog = Some(InfoDialog::new(InfoDialogKind::Help));
            }
            other => {
                // 软置灰（可发现、不可用）：/compact /mcp 永久置灰
                //（方法名未经钉靶核实，不猜协议）；/resume /model /perm
                // 由 Stage E 接管为映射命令。
                self.flash_status(format!("/{other} is not available in clat dsh mode"));
            }
        }
    }

    /// dsh 态 Esc（running）：无栈式召回（DSH 无 recall API，INV-U3
    /// 例外②）——直接取消宿主 turn。
    pub(super) fn cancel_dsh(&mut self) {
        let Some(dsh) = self.dsh.as_ref() else {
            return;
        };
        if let Some(session) = dsh.current_session.clone() {
            dsh.send_task(DshTask::Cancel { session });
            self.flash_status("cancelling…");
        }
    }

    /// 主视图 Shift+Tab（档位接入 2026-08-23，与 local 的
    /// `cycle_thinking_level` 同键同义）：循环当前模型的档位——
    /// selectModel 携带 reasoningEffort 重选当前模型；落定以 Selected
    /// 回执 / request/header 事件为准（不乐观更新本地，失败不残留）。
    pub(super) fn cycle_dsh_effort(&mut self) {
        let Some(dsh) = self.dsh.as_ref() else {
            return;
        };
        let Some(session) = dsh.current_session.clone() else {
            self.flash_status("no session");
            return;
        };
        let Some(next) = dsh.next_effort() else {
            self.flash_status("this model has no reasoning efforts");
            return;
        };
        let (provider, model) = dsh.model_ids.clone();
        dsh.send_task(DshTask::Select {
            session,
            provider,
            model,
            effort: Some(next),
        });
        self.flash_status("switching effort…");
    }

    /// dsh 状态栏右侧段（§2.4：Wallet 隐藏——余额是本地 MonitorService
    /// 对本地 key 的监视，与宿主模型无关；Cache/Context 按 DSH 口径；
    /// 数据缺席整段隐藏，INV-U7 不编数字）。
    pub(crate) fn dsh_status_segments(&self) -> Vec<String> {
        let Some(dsh) = self.dsh.as_ref() else {
            return Vec::new();
        };
        let mut parts = Vec::new();
        if let Some(percent) = dsh.usage.cache_percent() {
            parts.push(format!("Cache: {percent}%"));
        }
        if let Some(window) = dsh.context_window {
            parts.push(format!(
                "Context: {}/{}",
                format_tokens(dsh.usage.context_current()),
                format_tokens(window),
            ));
        }
        parts
    }
}

/// DSH 单题 → CLAT ask 弹框（多题时题面前缀 `(i/N)`；选项题
/// allow_custom=false、自由题 true——D-1 同款取舍）。
fn dsh_ask_dialog(
    question: Value,
    index: usize,
    total: usize,
    answer_tx: Sender<AskAnswer>,
) -> PendingAskUser {
    let text = question
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let options: Vec<crate::interaction::AskOption> = question
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .map(|option| crate::interaction::AskOption {
                    label: option
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    let allow_custom = options.is_empty();
    let question_text = if total > 1 {
        format!("({}/{total}) {text}", index + 1)
    } else {
        text.to_owned()
    };
    PendingAskUser {
        question: crate::interaction::AskQuestion {
            question: question_text,
            options,
            allow_custom,
        },
        answer_tx,
        selection: 0,
        custom: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PermissionMode;
    use serde_json::json;

    /// 测试用 dsh 态 App：不触网（通道握在测试手里），WS 标记已开。
    fn dsh_app() -> (App, Receiver<DshTask>) {
        let mut app = App::open_dsh(3080).expect("dsh app opens");
        app.test_freeze_tick = true;
        app.clipboard_writer = discard_clipboard_sink;
        let (task_tx, task_rx) = mpsc::channel::<DshTask>();
        let (events_tx, _events_rx) = backend::event_channel();
        let mut state = DshState::new(3080, describe_fixture(), task_tx, events_tx);
        state.test_mark_ws_open();
        state.current_session = Some("session-test".into());
        app.dsh = Some(state);
        app.dsh_connect = None;
        app.dsh_connect_rx = None;
        // 记忆文件改道临时路径——测试不得触碰真实 ~/.clat。
        app.dsh_memory_path = std::env::temp_dir().join(format!(
            "clat-dsh-memo-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        (app, task_rx)
    }

    /// FIX-5/CA-08：丢弃 sink——dsh 测试不写真实终端。
    fn discard_clipboard_sink(_: &[u8]) -> bool {
        true
    }

    fn describe_fixture() -> Value {
        serde_json::json!({
            "version": "0.1.1-rc.2", "cwd": "/home/dev/dsh-project",
            "provider": "deepseek", "model": "test-model",
            "attachedSessions": 1, "home": "/home/dev"
        })
    }

    /// 绑一个临时端口再立刻释放 → 连接必拒（不写死端口，抗环境）。
    fn scratch_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("scratch port");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);
        port
    }

    fn event(app: &mut App, event: DshEvent) {
        app.handle_dsh_event(event);
    }

    /// 以 App 当前代际注入帧（代际不匹配的腿在测试里显式手写）。
    fn frame(app: &mut App, frame: DshFrame) {
        let generation = app.dsh.as_ref().map(|dsh| dsh.generation).unwrap_or(0);
        event(app, DshEvent::Frame { generation, frame });
    }

    fn session_event(kind: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent::new(kind, seq, 1_700_000_000_000 + seq as i64 * 1000, data)
    }

    /// 落盘级（surface）事件：转录装配只认带 items 的事件。
    fn surface(kind: &str, seq: u64, data: Value) -> SessionEvent {
        session_event(kind, seq, data).append(Vec::new())
    }

    /// 会话视图的纯文本投影（判内容用）。
    fn rendered_text(app: &mut App) -> String {
        use crate::tui::conversation::ToolCardVisibility;
        app.conversation.ensure_rendered(80);
        let total = app.conversation.total_lines(ToolCardVisibility::Collapsed);
        (0..total)
            .map(|row| {
                app.conversation
                    .row_plain_text(row, 80, ToolCardVisibility::Collapsed)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// §0-2（断线自动重连拍板）：LinkDown → 断线态 + 重连排程；到点
    /// poll 发 Reconnect 任务且单飞；任务在途不重发（判别：删掉排程
    /// 或单飞守卫即红）。
    #[test]
    fn link_down_schedules_and_poll_fires_a_single_reconnect() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::LinkDown {
                generation: 0,
                reason: "connection closed".into(),
            },
        );
        let dsh = app.dsh.as_ref().expect("state");
        assert!(!dsh.connected);
        assert!(dsh.reconnect_deadline().is_some(), "a retry is scheduled");
        assert!(
            dsh.banner
                .as_deref()
                .is_some_and(|b| b.contains("reconnecting"))
        );
        // 未到点：不发包。
        app.poll_dsh();
        assert!(task_rx.try_recv().is_err(), "not due yet — no task");
        // 到点（测试钩子把排程拨到过去）→ 恰一个 Reconnect。
        app.dsh.as_mut().unwrap().test_due_reconnect_now();
        app.poll_dsh();
        assert!(matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)));
        assert!(
            task_rx.try_recv().is_err(),
            "single flight — no second task"
        );
        assert!(app.dsh.as_ref().unwrap().reconnecting);
    }

    /// §0-5 + INV-U7（usage 口径与诚实呈现）：DSH 三计数不相交 →
    /// Cache = cacheRead/(input+cacheRead)；contextWindow 缺席整段
    /// 隐藏、出席显示 input+cacheRead 分子（判别：用本地口径公式即红）。
    #[test]
    fn usage_projection_uses_the_dsh_disjoint_counts() {
        let (mut app, _task_rx) = dsh_app();
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event(
                    "assistant/message",
                    1,
                    json!({"usage": {"inputTokens": 300, "cacheReadTokens": 100}}),
                ),
            },
        );
        assert_eq!(app.dsh_status_segments(), vec!["Cache: 25.00%".to_owned()]);
        // contextWindow 到场 → Context 段出现（分子 = 300+100）。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event(
                    "request/context",
                    2,
                    json!({"provider": "deepseek", "model": "test-model", "contextWindow": 65536}),
                ),
            },
        );
        let segments = app.dsh_status_segments();
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert!(segments[1].starts_with("Context: 400/"), "{}", segments[1]);
    }

    /// §2.6 步骤 ②（create 收养式判别）：收养任务在 App 侧携带目标
    /// 会话自己的 workspace_path 作为 cwd。
    #[test]
    fn adopt_session_sends_create_with_the_target_workspace_cwd() {
        let (mut app, task_rx) = dsh_app();
        app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
            session_id: "session-target".into(),
            workspace_title: "beta".into(),
            workspace_path: "/w/beta".into(),
            title: None,
            activity_ms: 0,
        });
        match task_rx.try_recv() {
            Ok(DshTask::Create { session_id, cwd }) => {
                assert_eq!(session_id.as_deref(), Some("session-target"));
                assert_eq!(cwd.as_deref(), Some("/w/beta"));
            }
            other => panic!("adoption sends Create: {other:?}"),
        }
    }

    /// §2.6 步骤 ③-⑥（Created 回执驱动切换）：转录重置、preset 投影
    /// 清零、标题清空（判别：任一步缺席即红；作用域随 2026-08-23
    /// 返工撤销，无⑦）。
    #[test]
    fn created_reply_switches_and_resets_session_state() {
        let (mut app, task_rx) = dsh_app();
        // 预置旧会话残留：内容 + preset + 标题。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event("sandbox/mode", 1, json!({"mode": "read-only"})),
            },
        );
        app.session_title = Some("old title".into());
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-new".into())),
        );
        let dsh = app.dsh.as_ref().expect("state");
        assert_eq!(dsh.current_session(), Some("session-new"));
        assert_eq!(dsh.session_tail, "session-new");
        assert_eq!(dsh.preset, None, "preset re-projects from the new session");
        assert_eq!(app.session_title, None);
        assert!(app.conversation.is_empty());
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::History { session }) if session == "session-new"
        ));
    }

    /// INV-U3（Ctrl+O 同一 ConversationModel 天然继承）：dsh 态三态循环。
    #[test]
    fn ctrl_o_cycles_card_visibility_in_dsh_mode() {
        use crate::tui::conversation::ToolCardVisibility;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let (mut app, _task_rx) = dsh_app();
        let before = app.card_visibility;
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))));
        assert_ne!(app.card_visibility, before);
        app.handle_ui_event(UiEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))));
        assert_eq!(
            app.card_visibility,
            ToolCardVisibility::default().next().next()
        );
    }

    /// §3（steering 计数校正）：session/queue 帧按 placement=="steering"
    /// 的条目数修剪本地回显（判别：删掉修剪即红）。
    #[test]
    fn queue_frame_trims_the_local_steering_echo() {
        let (mut app, _task_rx) = dsh_app();
        app.conversation.push_pending_steering("first".into());
        app.conversation.push_pending_steering("second".into());
        assert_eq!(app.conversation.pending_steering_count(), 2);
        frame(
            &mut app,
            DshFrame::Queue {
                session_id: "session-test".into(),
                items: json!([
                    {"id": "m1", "placement": "steering", "message": {}},
                    {"id": "m2", "placement": "queued", "message": {}}
                ]),
            },
        );
        assert_eq!(
            app.conversation.pending_steering_count(),
            1,
            "the host queue says one steering item remains"
        );
    }

    /// 退出清理（2026-08-23 拍板）：Drop 带走自己 spawn 的宿主；重连
    /// respawn 时旧句柄被替换、旧宿主一并带走（判别：去掉任一 kill
    /// 即红——sleep 30 会活过断言窗口）；旁观进程不受影响。unix 门控：
    /// 进程名不可移植。
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn sleeper() -> command_group::GroupChild {
        use command_group::CommandGroup as _;
        std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .group_spawn()
            .expect("sleep spawns")
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_state_kills_the_host_we_spawned() {
        // 旁观进程：只杀自己持有的句柄，别人的进程不动。
        let mut witness = sleeper();
        assert!(alive(witness.id()));

        // 腿 1：Drop 带走自己 spawn 的宿主。
        let (mut app, _task_rx) = dsh_app();
        let host = sleeper();
        let host_pid = host.id();
        app.dsh
            .as_mut()
            .unwrap()
            .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(host)));
        assert!(alive(host_pid));
        drop(app);
        assert!(!alive(host_pid), "Drop must take the spawned host down");

        // 腿 2：重连替换——旧宿主被带走、新句柄在位（随后 Drop 一并
        // 带走）。
        let (mut app, _task_rx) = dsh_app();
        let old_host = sleeper();
        let old_pid = old_host.id();
        app.dsh
            .as_mut()
            .unwrap()
            .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(old_host)));
        let new_host = sleeper();
        let new_pid = new_host.id();
        app.dsh
            .as_mut()
            .unwrap()
            .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(new_host)));
        assert!(!alive(old_pid), "replacement kills the old spawned host");
        assert!(alive(new_pid));
        drop(app);
        assert!(!alive(new_pid));

        // 旁观者全程存活，测试收尾带走。
        let witness_pid = witness.id();
        assert!(alive(witness_pid), "unrelated processes are never touched");
        let _ = witness.kill();
        let _ = witness.wait();
    }

    /// FIX-3/CA-03（2026-08-24 审计，pre-fix 红）：自启宿主的清理是
    /// **树级**的——忽视 TERM 的后代必须随 leader 一起消失。走真实
    /// 生产路径：ensure_online（spawn + 就绪行 + probe 指纹）→ 收养 →
    /// Drop。pre-fix：普通 spawn + leader-only kill/wait → 后代存活
    /// → 红。
    #[cfg(unix)]
    #[test]
    fn spawned_host_cleanup_takes_the_whole_tree() {
        use std::io::{Read as _, Write as _};

        // 一次性 describe 服务：ensure_online 的 probe 指纹闸门所需。
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let describe_port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = server.accept() else {
                return;
            };
            // 读走请求（头到 \r\n\r\n + content-length body）。
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            while stream.read(&mut byte).is_ok_and(|n| n > 0) {
                buf.push(byte[0]);
                if buf.ends_with(b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    if length > 0 {
                        let _ = stream.read_exact(&mut body);
                    }
                    // 信封 rpcId 必须回显（client 校验）。
                    let rpc_id = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|value| value.get("rpcId").cloned())
                        .unwrap_or(serde_json::json!("rpc-tree-test"));
                    let describe = serde_json::json!({
                        "version": "0.1.1-rc.2",
                        "cwd": "/Users/dev/project",
                        "attachedSessions": 1,
                        "home": "/Users/dev",
                    });
                    let response = serde_json::json!({
                        "type": "server-response",
                        "rpcId": rpc_id,
                        "result": {"ok": true, "value": describe},
                    });
                    let body = response.to_string();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                    break;
                }
            }
        });

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pidfile = std::env::temp_dir().join(format!("clat-dsh-tree-{stamp}.pid"));
        let script = std::env::temp_dir().join(format!("clat-dsh-tree-{stamp}.sh"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(trap '' TERM; exec sleep 60) &\necho $! > \"{pidfile}\"\n\
                 echo \"dsh web: http://127.0.0.1:{describe_port}\"\nsleep 60\n",
                pidfile = pidfile.display(),
                describe_port = describe_port,
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // preferred port：闲置 scratch（probe 失败 → 走 spawn 路径）。
        let scratch = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        let home = std::env::temp_dir();
        let online =
            crate::dsh::connect::ensure_online(scratch, script.to_str().unwrap(), Some(&home))
                .expect("fake dsh connects");
        let leader = online.child.as_ref().expect("we spawned it").id();

        // 等后代 pid 落盘（trap '' TERM + exec sleep：忽视 TERM 的后代）。
        let mut descendant = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                descendant = Some(pid);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let descendant = descendant.expect("descendant pid recorded");
        assert!(alive(leader), "the leader is up");
        assert!(alive(descendant), "the TERM-ignoring descendant is up");

        // 收养 + Drop：树级清理必须带走整棵树。
        let (mut app, _task_rx) = dsh_app();
        app.dsh.as_mut().unwrap().adopt_spawned_host(online.child);
        drop(app);
        assert!(!alive(leader), "the leader must be gone");
        assert!(
            !alive(descendant),
            "the TERM-ignoring descendant must not outlive the group cleanup"
        );
        std::fs::remove_file(&script).ok();
        std::fs::remove_file(&pidfile).ok();
    }

    /// §2.4（preset 投影 → 输入框右上角档位词汇）：journal 值 → DSH
    /// 产品标签；未知自定义值原样显示。
    #[test]
    fn preset_projection_maps_journal_values_to_web_labels() {
        assert_eq!(dsh_preset_label("read-only"), "Read Only");
        assert_eq!(dsh_preset_label("workspace-write"), "Workspace Write");
        assert_eq!(dsh_preset_label("danger-full-access"), "Full Access");
        assert_eq!(dsh_preset_label("custom-audit-mode"), "custom-audit-mode");
        // PermissionMode 与 journal 值互转（dsh /perm 的 Apply 通道）。
        assert_eq!(
            PermissionMode::from_journal_value("workspace-write"),
            Some(PermissionMode::ProjectWrite)
        );
        assert_eq!(
            PermissionMode::ProjectWrite.journal_value(),
            "workspace-write"
        );
    }

    /// 审计 P1-3 判别：重连失败回执必须复位单飞守卫并重新排程——
    /// 删掉复位即红（第二次 poll 不发包、deadline 永久 None）。
    #[test]
    fn reconnect_failure_re_arms_the_next_attempt() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::LinkDown {
                generation: 0,
                reason: "connection closed".into(),
            },
        );
        app.dsh.as_mut().unwrap().test_due_reconnect_now();
        app.poll_dsh();
        assert!(matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)));

        // 重连失败回执：复位 + 重新排程。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::ReconnectFailed("host is down".into())),
        );
        {
            let dsh = app.dsh.as_ref().unwrap();
            assert!(!dsh.reconnecting, "the single-flight guard must reset");
            assert!(
                dsh.reconnect_deadline().is_some(),
                "a retry must be scheduled again"
            );
        }

        // 再次到点 → 第二次重连尝试照发（一次失败不得永久停摆）。
        app.dsh.as_mut().unwrap().test_due_reconnect_now();
        app.poll_dsh();
        assert!(
            matches!(task_rx.try_recv(), Ok(DshTask::Reconnect)),
            "the second attempt must fire"
        );
    }

    /// 审计 P1-1 判别：旧会话有实质内容时切换——视图即刻清空、
    /// live 帧暂存、整页回执后只剩目标历史（旧内容绝不串线；删掉
    /// 视图清空或暂存态即红）。
    #[test]
    fn switching_sessions_replaces_the_view_and_replays_staged_frames() {
        let (mut app, task_rx) = dsh_app();
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: surface(
                    "user/message",
                    1,
                    json!({"content": [{"type": "text", "text": "old session text"}]}),
                ),
            },
        );
        let text = rendered_text(&mut app);
        assert!(text.contains("old session text"), "{text}");

        // 收养新会话：视图即刻接管为空白，History 任务发出。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-target".into())),
        );
        assert!(
            app.conversation.is_empty(),
            "the old session's content must leave the view at the switch"
        );
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::History { session }) if session == "session-target"
        ));

        // 竞态腿：整页回执未到，目标会话的 live 帧先到 → 暂存不显示。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-target".into(),
                event: surface(
                    "user/message",
                    8,
                    json!({"content": [{"type": "text", "text": "live while loading"}]}),
                ),
            },
        );
        assert!(
            app.conversation.is_empty(),
            "live frames stay staged until the history page lands"
        );

        // 整页回执：重建 + 暂存补放，旧内容绝不回来。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::History {
                session: "session-target".into(),
                events: vec![surface(
                    "user/message",
                    5,
                    json!({"content": [{"type": "text", "text": "target history"}]}),
                )],
            }),
        );
        let text = rendered_text(&mut app);
        assert!(text.contains("target history"), "{text}");
        assert!(
            text.contains("live while loading"),
            "the staged live frame is replayed after the page: {text}"
        );
        assert!(
            !text.contains("old session text"),
            "cross-session bleed must not survive the switch: {text}"
        );
    }

    /// 审计 P1-2 判别：后台会话的审批/问答不得弹进当前视图，别处
    /// 的落定不得关掉我们的弹框（删掉任一 session 关联即红）。
    #[test]
    fn approvals_and_questions_from_other_sessions_stay_out_of_the_view() {
        let (mut app, _task_rx) = dsh_app();
        // 后台会话的审批：不弹框（用户绝不能代答一个自己没在看的会话）。
        frame(
            &mut app,
            DshFrame::ApprovalRequested {
                rpc_id: "rpc-bg".into(),
                session_id: "session-other".into(),
                approval_id: "apr-1".into(),
                tool_name: "bash".into(),
                call_id: None,
                reason: Some("runs a command".into()),
            },
        );
        assert!(
            app.pending_permission.is_none(),
            "a background session's approval must not open the dialog"
        );
        // 当前会话的审批：弹框。
        frame(
            &mut app,
            DshFrame::ApprovalRequested {
                rpc_id: "rpc-mine".into(),
                session_id: "session-test".into(),
                approval_id: "apr-2".into(),
                tool_name: "bash".into(),
                call_id: None,
                reason: None,
            },
        );
        assert!(app.pending_permission.is_some());
        // 后台会话的落定：不动当前弹框。
        frame(
            &mut app,
            DshFrame::ApprovalResolved {
                session_id: "session-other".into(),
                approval_id: "apr-1".into(),
                outcome: "allowed-once".into(),
            },
        );
        assert!(
            app.pending_permission.is_some(),
            "another session's resolution must not close our dialog"
        );
        // 同会话但别的审批的落定：也不动（按 approvalId 精确关联）。
        frame(
            &mut app,
            DshFrame::ApprovalResolved {
                session_id: "session-test".into(),
                approval_id: "apr-9".into(),
                outcome: "rejected".into(),
            },
        );
        assert!(app.pending_permission.is_some());
        // 我们的落定：关框。
        frame(
            &mut app,
            DshFrame::ApprovalResolved {
                session_id: "session-test".into(),
                approval_id: "apr-2".into(),
                outcome: "allowed-once".into(),
            },
        );
        assert!(app.pending_permission.is_none());

        // 问答同规则：Requested 按 session 过滤、Resolved 按 session+rpc。
        frame(
            &mut app,
            DshFrame::QuestionRequested {
                rpc_id: "q-bg".into(),
                session_id: "session-other".into(),
                questions: json!([{"id": "q1", "question": "background"}]),
            },
        );
        assert!(app.pending_ask_user.is_none());
        frame(
            &mut app,
            DshFrame::QuestionRequested {
                rpc_id: "q-mine".into(),
                session_id: "session-test".into(),
                questions: json!([{"id": "q1", "question": "mine"}]),
            },
        );
        assert!(app.pending_ask_user.is_some());
        frame(
            &mut app,
            DshFrame::QuestionResolved {
                session_id: "session-other".into(),
                rpc_id: "q-bg".into(),
                outcome: json!({}),
            },
        );
        assert!(
            app.pending_ask_user.is_some(),
            "another session's resolution must not close our question"
        );
        frame(
            &mut app,
            DshFrame::QuestionResolved {
                session_id: "session-test".into(),
                rpc_id: "q-mine".into(),
                outcome: json!({}),
            },
        );
        assert!(app.pending_ask_user.is_none());
    }

    /// 审计 P1-4 判别：瞬时 WS 故障重连时探测命中仍健在的自家宿主
    ///（child=None）——句柄必须保留（删掉甄别即红：宿主被误杀）；
    /// Drop 仍然带走它。unix 门控：进程名不可移植。
    #[cfg(unix)]
    #[test]
    fn a_probe_hit_reconnect_never_kills_the_host_we_own() {
        let (mut app, _task_rx) = dsh_app();
        let host = sleeper();
        let pid = host.id();
        app.dsh
            .as_mut()
            .unwrap()
            .adopt_spawned_host(Some(crate::dsh::connect::OwnedDshHost::new(host)));
        assert!(alive(pid));
        // 探测直连的重连成功（child=None，downlink 开在死端口上失败
        // 无妨——断言只关心宿主生死）。
        event(
            &mut app,
            DshEvent::Reconnected {
                port: scratch_port(),
                describe: describe_fixture(),
                child: None,
            },
        );
        assert!(
            alive(pid),
            "a probe hit must never kill the host we spawned"
        );
        drop(app);
        assert!(!alive(pid), "Drop still owns the host for exit cleanup");
    }

    /// 审计 P2-2 判别：旧代际的迟到帧/断线一律作废——迟到的旧
    /// LinkDown 不得把健康的新连接再标成断线（删掉代际过滤即红）。
    #[test]
    fn stale_generation_frames_and_link_downs_are_ignored() {
        let (mut app, _task_rx) = dsh_app();
        app.dsh.as_mut().unwrap().test_simulate_stream_generation();
        let current = app.dsh.as_ref().unwrap().generation;
        assert_eq!(current, 1);

        // 旧代际的迟到帧：作废（重复流不得再进入视图）。
        event(
            &mut app,
            DshEvent::Frame {
                generation: current - 1,
                frame: DshFrame::SessionEvent {
                    session_id: "session-test".into(),
                    event: surface(
                        "user/message",
                        1,
                        json!({"content": [{"type": "text", "text": "from the stale stream"}]}),
                    ),
                },
            },
        );
        assert!(app.conversation.is_empty(), "stale frames must be dropped");

        // 旧代际的迟到断线：不得把健康的新连接标成断线。
        event(
            &mut app,
            DshEvent::LinkDown {
                generation: current - 1,
                reason: "late death of an old pump".into(),
            },
        );
        assert!(
            app.dsh.as_ref().unwrap().connected,
            "a stale LinkDown must not disconnect the live generation"
        );

        // 当前代际照常工作。
        event(
            &mut app,
            DshEvent::LinkDown {
                generation: current,
                reason: "real death".into(),
            },
        );
        assert!(!app.dsh.as_ref().unwrap().connected);
    }

    /// 审计 P2-2 场景腿：mux 握手成功、host 握手被拒（「第一条新 WS
    /// 开成、第二条失败」）——重试必须重新排程，代际已自增（孤儿 mux
    /// 泵属当前代际数据仍可用，下一次重连自增后自然作废）。
    #[test]
    fn a_partial_stream_open_failure_arms_a_retry() {
        // 迷你服务端：mux 路径回合法握手后静默持连；其余路径回 400。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        // 每连接一线程：mux 连接握手后要被静默持有，不能阻塞后续
        //（host 路径的）accept——否则客户端的第二次握手永远等不到回音。
        fn serve(mut stream: std::net::TcpStream) {
            use std::io::{Read, Write};
            let mut buffer = [0u8; 4096];
            let Ok(read) = stream.read(&mut buffer) else {
                return;
            };
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let key = request
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("sec-websocket-key")
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_default();
            if request.starts_with("GET /api/events.mux") {
                let accept = crate::dsh::ws::expected_accept(&key);
                let response = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                     Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes());
                // 静默持连到 EOF。
                let mut sink = [0u8; 4096];
                let _ = stream.read(&mut sink);
            } else {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            }
        }
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                std::thread::spawn(move || serve(stream));
            }
        });

        let (mut app, _task_rx) = dsh_app();
        let before = app.dsh.as_ref().unwrap().generation;
        event(
            &mut app,
            DshEvent::Reconnected {
                port,
                describe: describe_fixture(),
                child: None,
            },
        );
        let dsh = app.dsh.as_ref().unwrap();
        assert!(!dsh.connected);
        assert!(
            dsh.reconnect_deadline().is_some(),
            "a retry must be armed after a partial open failure"
        );
        assert!(
            dsh.banner
                .as_deref()
                .is_some_and(|b| b.contains("reconnect failed")),
            "banner: {:?}",
            dsh.banner
        );
        assert_eq!(
            dsh.generation,
            before + 1,
            "the generation advances even on partial failure"
        );
    }

    /// 审计 P2-1 UI 腿：启动链 Restore/History 失败（Failed 回执）时
    /// 不得永久悬挂——流照常尝试打开，开不了则转入重连机制（删掉
    /// fail-soft 开流即红：banner/排程缺席）。
    #[test]
    fn failed_startup_replies_never_hang_the_stream_open() {
        let mut app = App::open_dsh(3080).expect("dsh app opens");
        app.test_freeze_tick = true;
        app.clipboard_writer = discard_clipboard_sink;
        let (task_tx, _task_rx) = mpsc::channel::<DshTask>();
        let (events_tx, _events_rx) = backend::event_channel();
        // 不标记 ws_open：模拟启动链 Restore 失败、流尚未打开。
        let state = DshState::new(scratch_port(), describe_fixture(), task_tx, events_tx);
        app.dsh = Some(state);
        app.dsh_connect = None;
        app.dsh_connect_rx = None;
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Failed("session.list refused".into())),
        );
        let dsh = app.dsh.as_ref().unwrap();
        assert!(!dsh.ws_open);
        assert!(
            dsh.reconnect_deadline().is_some(),
            "fail-soft: the reconnect machinery takes over instead of hanging"
        );
        assert!(
            dsh.banner
                .as_deref()
                .is_some_and(|b| b.contains("reconnecting")),
            "the failure must be visible: {:?}",
            dsh.banner
        );
    }

    /// 审计 P2-4 判别：`request/context` 是会话级模型真来源——live 帧
    /// 与历史折叠都刷新 model_label（删掉任一刷新即红：标签残留
    /// 前一会话的模型）。
    #[test]
    fn request_context_refreshes_the_model_label_per_session() {
        let (mut app, _task_rx) = dsh_app();
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "deepseek · test-model"
        );
        // live 腿。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event(
                    "request/context",
                    1,
                    json!({"provider": "anthropic", "model": "claude-x", "contextWindow": 200000}),
                ),
            },
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "anthropic · claude-x"
        );
        // 切换 + 历史折叠腿：目标会话的模型接管标签。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-target".into())),
        );
        event(
            &mut app,
            DshEvent::Reply(TaskReply::History {
                session: "session-target".into(),
                events: vec![session_event(
                    "request/context",
                    1,
                    json!({"provider": "openai", "model": "gpt-test", "contextWindow": 128000}),
                )],
            }),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "openai · gpt-test",
            "the target session's model must take over the header label"
        );
    }

    /// 负责人 dogfood（2026-08-23 第三轮）判别：标题栏标签用展示名
    ///（web 端同源 groups[].name / models[].name），不是裸 id；prime
    /// 回执不开 picker、不 flash；索引未命中诚实回落裸 id（删掉 fold
    /// 即红：标签停在 `deepseek · test-model`）。
    #[test]
    fn model_label_resolves_display_names_from_the_prime() {
        let (mut app, _task_rx) = dsh_app();
        // 索引未 prime：裸 id 回落（诚实，不编名字）。
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "deepseek · test-model"
        );
        event(
            &mut app,
            DshEvent::Reply(TaskReply::ModelNames(json!({
                "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                    {"id": "test-model", "name": "Test Model Pro"}
                ]}],
                "current": {"provider": "deepseek", "model": "test-model"}
            }))),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "DeepSeek · Test Model Pro",
            "the header must show display names, not raw ids"
        );
        // prime 是装饰性获取：不开 picker。
        assert!(app.picker.is_none());
    }

    /// prime 只发一次（每连接）：首次切换 History + ModelNames 两任务，
    /// 再切换只有 History（目录是宿主全局的）。
    #[test]
    fn the_name_catalog_primes_once_per_connection() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-target".into())),
        );
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::History { session }) if session == "session-target"
        ));
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::ModelNames { session }) if session == "session-target"
        ));
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-target-2".into())),
        );
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::History { session }) if session == "session-target-2"
        ));
        assert!(
            task_rx.try_recv().is_err(),
            "the catalog primes once per connection"
        );
    }

    /// /model 应答同步解析名字并按 `current` 校正（会话权威选择——
    /// 兼收无 request/context 的新会话），picker 照开。
    #[test]
    fn models_reply_resolves_names_and_corrects_via_current() {
        let (mut app, _task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Models(json!({
                "groups": [
                    {"id": "deepseek", "name": "DeepSeek", "models": [
                        {"id": "test-model", "name": "Test Model Pro"}
                    ]},
                    {"id": "custom-ollama", "name": "Ollama (custom)", "models": [
                        {"id": "llama-local", "name": "Llama Local"}
                    ]}
                ],
                "failures": [],
                "current": {"provider": "custom-ollama", "model": "llama-local"}
            }))),
        );
        assert!(
            app.picker.is_some(),
            "the /model reply still opens the picker"
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().model_label,
            "Ollama (custom) · Llama Local",
            "current corrects the label with display names"
        );
    }

    /// 档位接入（2026-08-23）判别：`request/header` 是选择与档位的
    /// 权威重投影源（历史 fold + 实时同通路）——切换会话后档位由
    /// 历史恢复，标题栏档位段显示 efforts 表解析的展示名；切换本身
    /// 先清档位（残留即红）。
    #[test]
    fn request_header_reprojects_selection_and_effort() {
        let (mut app, _task_rx) = dsh_app();
        // efforts 目录 prime（名字 + 档位表 + 当前档位 low）。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::ModelNames(json!({
                "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                    {"id": "test-model", "name": "Test Model Pro",
                     "reasoning": {"efforts": [
                        {"id": "off", "name": "Off"},
                        {"id": "low", "name": "Low"},
                        {"id": "high", "name": "High"}
                     ]}}
                ]}],
                "current": {"provider": "deepseek", "model": "test-model",
                            "reasoningEffort": "low"}
            }))),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().effort_display().as_deref(),
            Some("Low"),
            "current.reasoningEffort seeds the header effort segment"
        );
        // 切换：档位先清零。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-target".into())),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().effort_display(),
            None,
            "the effort resets with the session switch"
        );
        // 历史折叠腿：request/header 恢复选择与档位。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::History {
                session: "session-target".into(),
                events: vec![session_event(
                    "request/header",
                    1,
                    json!({"header": {"config": {
                        "provider": "deepseek", "model": "test-model",
                        "reasoningEffort": "high"
                    }}, "reason": "change"}),
                )],
            }),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().effort_display().as_deref(),
            Some("High"),
            "history request/header re-projects the effort"
        );
        // 实时腿：后续 request/header 到场照收。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-target".into(),
                event: session_event(
                    "request/header",
                    2,
                    json!({"header": {"config": {
                        "provider": "deepseek", "model": "test-model"
                    }}, "reason": "change"}),
                ),
            },
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().effort_display(),
            None,
            "a request header without reasoningEffort clears the segment"
        );
    }

    /// 档位接入判别：selectModel 落定回执携带宿主解析后的档位（可能
    /// 与请求值不同——以回执为准）；主视图 Shift+Tab 从当前档位循环
    /// 发出 Select（当前 low → 下一档 high）。
    #[test]
    fn main_view_shift_tab_cycles_the_current_effort() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::ModelNames(json!({
                "groups": [{"id": "deepseek", "name": "DeepSeek", "models": [
                    {"id": "test-model", "name": "Test Model Pro",
                     "reasoning": {"efforts": [
                        {"id": "off", "name": "Off"},
                        {"id": "low", "name": "Low"},
                        {"id": "high", "name": "High"}
                     ]}}
                ]}],
                "current": {"provider": "deepseek", "model": "test-model",
                            "reasoningEffort": "low"}
            }))),
        );
        // 清空 prime 期间积压的任务通道。
        while task_rx.try_recv().is_ok() {}
        app.handle_ui_event(UiEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        )));
        match task_rx.try_recv() {
            Ok(DshTask::Select {
                provider,
                model,
                effort,
                ..
            }) => {
                assert_eq!(provider, "deepseek");
                assert_eq!(model, "test-model");
                assert_eq!(effort.as_deref(), Some("high"), "low cycles to high");
            }
            other => panic!("Shift+Tab cycles the current model's effort: {other:?}"),
        }
        // 落定回执（宿主解析后的权威值）刷新档位段。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Selected {
                provider: "deepseek".into(),
                model: "test-model".into(),
                effort: Some("max".into()),
            }),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().effort_display().as_deref(),
            Some("max"),
            "the resolved reply wins even when it differs from the request"
        );
    }

    /// MM-3 失败恢复判别：dsh 不支持附件时，文本与结构化草稿都保留，
    /// 且不得只把文本发给宿主造成一次不可逆的半提交。
    #[test]
    fn dsh_submit_with_attachments_preserves_the_complete_draft() {
        let (mut app, task_rx) = dsh_app();
        app.attachments
            .add_unchecked_for_test(std::path::PathBuf::from("/tmp/evidence.png"));
        app.input.insert_str("look at this");
        app.submit_input();
        assert!(
            app.status.contains("attachments are not supported"),
            "the warning must actually fire, status: {}",
            app.status
        );
        assert_eq!(app.input.text(), "look at this");
        assert_eq!(app.attachments.len(), 1);
        assert!(task_rx.try_recv().is_err(), "no partial prompt is sent");
    }

    /// B-4（2026-08-24 负责人对齐：clat dsh 是宿主的终端客户端）判别：
    /// 恢复带回会话自己的 workspace——状态栏/标题栏第二行显示会话
    /// 项目目录，不再是 describe.cwd（宿主进程目录；被我们 spawn 时
    /// 即本地运行目录）。缺席（未记录）诚实回落 describe.cwd。
    #[test]
    fn restored_session_carries_its_workspace_display() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Restored {
                session: Some("session-ws".into()),
                cwd: Some("/w/target".into()),
            }),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().current_session(),
            Some("session-ws")
        );
        assert_eq!(app.dsh.as_ref().unwrap().cwd(), "/w/target");
        assert_eq!(
            app.default_status, "/w/target",
            "the persistent status line follows the session workspace"
        );
        // 切换会发出 History 任务（既有行为不变）。
        assert!(matches!(
            task_rx.try_recv(),
            Ok(DshTask::History { session }) if session == "session-ws"
        ));
        // FIX-4/CA-04（2026-08-24 审计，pre-fix 红）：缺席腿改为**同一
        // App** 序列——旧 workspace 必须被 None 覆盖（清除 → 回落
        // describe.cwd），否则旧值遮住回落（pre-fix：残留 /w/target →
        // 红），且 /new 不得在错误目录建会话。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Restored {
                session: Some("session-bare".into()),
                cwd: None,
            }),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().cwd(),
            "/home/dev/dsh-project",
            "absent cwd must clear the stale workspace and fall back to describe.cwd"
        );
        while task_rx.try_recv().is_ok() {}
        app.submit_dsh("/new".into());
        match task_rx.try_recv() {
            Ok(DshTask::Create { session_id, cwd }) => {
                assert_eq!(session_id, None);
                assert_ne!(
                    cwd.as_deref(),
                    Some("/w/target"),
                    "/new must not inherit the previous session's workspace"
                );
            }
            other => panic!("cwd-less restore then /new: {other:?}"),
        }
    }

    /// B-4 判别：/resume 收养的 workspace 经 pending_adoption 按回执
    /// id 匹配跟随；失败（session-conflict）与不匹配回执都不残留。
    #[test]
    fn adoption_follows_the_workspace_and_failures_do_not_leak() {
        let (mut app, _task_rx) = dsh_app();
        // 成功收养：workspace 跟随目标会话。
        app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
            session_id: "session-x".into(),
            workspace_title: "x".into(),
            workspace_path: "/w/x".into(),
            title: None,
            activity_ms: 0,
        });
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-x".into())),
        );
        assert_eq!(app.dsh.as_ref().unwrap().cwd(), "/w/x");

        // 失败收养（session-conflict）：停留原会话，workspace 不动。
        app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
            session_id: "session-y".into(),
            workspace_title: "y".into(),
            workspace_path: "/w/y".into(),
            title: None,
            activity_ms: 0,
        });
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Failed("session-conflict".into())),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().cwd(),
            "/w/x",
            "a failed adoption must not move the workspace"
        );

        // 不匹配回执（/new 的新 id 等）：pending 清空、不跟随。
        app.dsh_adopt_session(crate::tui::session_picker::DshResumeRow {
            session_id: "session-z".into(),
            workspace_title: "z".into(),
            workspace_path: "/w/z".into(),
            title: None,
            activity_ms: 0,
        });
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-unrelated".into())),
        );
        assert_eq!(
            app.dsh.as_ref().unwrap().cwd(),
            "/w/x",
            "a non-matching Created reply must not adopt the pending workspace"
        );
    }

    /// B-4 功能腿：/new 继承**当前会话的 workspace**（此前继承
    /// describe.cwd——宿主进程目录，恢复跨工作区会话后会在错误的
    /// 目录创建新会话）。
    #[test]
    fn new_session_inherits_the_current_workspace() {
        let (mut app, task_rx) = dsh_app();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Restored {
                session: Some("session-ws".into()),
                cwd: Some("/w/target".into()),
            }),
        );
        while task_rx.try_recv().is_ok() {}
        app.submit_dsh("/new".into());
        match task_rx.try_recv() {
            Ok(DshTask::Create { session_id, cwd }) => {
                assert_eq!(session_id, None);
                assert_eq!(cwd.as_deref(), Some("/w/target"));
            }
            other => panic!("/new creates in the current workspace: {other:?}"),
        }
    }

    /// B-5 铃判别（2026-08-24 负责人 dogfood：后台等待 turn 结束无铃）：
    /// dsh turn 结束响铃（此前只有审批/问答弹框响）；aborted = 用户
    /// 主动取消不响（同本地语义）。端到端 marker 腿——删掉 turn/end
    /// 的 notify 即红（completed 的 marker 不出现）。
    #[test]
    fn dsh_turn_end_rings_the_bell_except_when_aborted() {
        let marker = std::env::temp_dir().join(format!(
            "clat-dsh-bell-{}.marker",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (mut app, _task_rx) = dsh_app();
        app.bell = BellMode::Command(format!("printf ok > {:?}", marker));
        // 后台（B-1 三态：失焦必响）。
        app.focused = Some(false);
        // 用户取消：不响。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event(
                    "turn/end",
                    1,
                    json!({"turn": 1, "reason": {"kind": "aborted"}}),
                ),
            },
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !marker.exists(),
            "an aborted (user-cancelled) turn must not ring"
        );
        // 自然结束：响。
        frame(
            &mut app,
            DshFrame::SessionEvent {
                session_id: "session-test".into(),
                event: session_event(
                    "turn/end",
                    2,
                    json!({"turn": 2, "reason": {"kind": "completed"}}),
                ),
            },
        );
        let mut appeared = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if marker.exists() {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(appeared, "a completed turn rings while unfocused");
        let _ = std::fs::remove_file(&marker);
    }

    /// 拍板 A（2026-08-24 客户端自记）判别：切换即写入「最后打开会话」
    /// 记忆（restore/收养//new 全经 dsh_switch_session，单点写入；最后一
    /// 次获胜）。删掉写入即红（文件缺席）。
    #[test]
    fn switching_sessions_remembers_the_last_open_session() {
        let (mut app, _task_rx) = dsh_app();
        let memo = app.dsh_memory_path.clone();
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Restored {
                session: Some("session-aite".into()),
                cwd: Some("/w/aite".into()),
            }),
        );
        assert_eq!(
            crate::dsh::last_session::read_last_session_at(&memo),
            Some("session-aite".to_owned()),
            "a switch writes the client-side memory"
        );
        // 再切换 → 覆写（最后一次获胜）。
        event(
            &mut app,
            DshEvent::Reply(TaskReply::Created("session-clat".into())),
        );
        assert_eq!(
            crate::dsh::last_session::read_last_session_at(&memo),
            Some("session-clat".to_owned())
        );
        let _ = std::fs::remove_file(memo);
    }
}
