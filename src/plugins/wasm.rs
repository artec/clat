//! WASM 组件插件运行时（插件桥 Phase 2a，docs/todo/wasm-plugin-runtime.md）。
//!
//! WIT world `wit/plugin.wit`（`clat:plugin@0.1.0`）逐字镜像 MCP 叶子
//! 语义：组件导出 `tools`（= tools/list + tools/call），导入
//! sampling/elicitation——由 `PluginHostBridge` 提供实现，与 MCP 传输
//! 共用同一语义面（INV-W4：一个对外契约、三种传输）。
//!
//! 沙箱（INV-W1，2026-08-21 修正措辞）：**零授权 WASI**——wasm32-
//! wasip2 组件天然导入 wasi:io/poll 等接口，宿主经 wasmtime-wasi 提
//! 供接口但 WasiCtx 不授予任何能力：无 preopen（文件系统可达面为
//! 空）、无环境变量、stdio 关闭、sockets 无地址授权。组件的授权面
//! 只有 world 声明的 sampling/elicitation（全部过桥）；Phase 2b 的能
//! 力授予就是往 WasiCtx 里按权限档位加 preopen。
//! 有界执行（INV-W3，2d 起为 fuel 计量 + W1-01 取消中断）：燃料只在
//! wasm 实际执行时消耗——host 调用阻塞等人（elicitation/sampling）不
//! 烧预算；每次工具调用重置预算（校准 ≈120s 纯执行），超耗 trap 为
//! 工具错误；内存经 StoreLimits 上限 256MB。取消令牌经 epoch 中断
//! 成为**执行期**能力（W1-01）：调用期间轮询 `CancelToken`，置位即
//! `engine.increment_epoch()`，组件在下一个执行点 trap——Esc 不必等
//! 燃料耗尽。epoch 刻度不经时间流逝推进，"等待不烧预算"不变量保持。

use super::services::{
    MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus, TOOL_SERVICE, TOOL_SERVICE_ID,
};
use super::wasm_grants;
use crate::mcp::client::qualify_prefixed_tool_name;
use crate::model::CancelToken;
use crate::plugin::{
    Plugin as PluginTrait, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::plugin_host::{
    ElicitField, ElicitFieldKind, ElicitForm, ElicitOutcome, PluginHostBridge, PluginSource,
    SamplingMessage, SamplingRequest, SamplingRole,
};
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};
use wasmtime::component::{HasData, Linker, Resource, ResourceTable};
use wasmtime::{Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::bindings::clocks::monotonic_clock;
use wasmtime_wasi::p2::{DynPollable, Pollable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "wit",
    world: "plugin",
});

const ID: PluginId = PluginId::new("builtin.wasm_adapter");
const PROVIDES: &[ServiceId] = &[];
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const OPTIONAL: &[ServiceId] = &[MCP_STATUS_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: REQUIRES,
    optional: OPTIONAL,
};

/// 单次工具调用的燃料预算（INV-W3）：fuel ≈ 指令数，10^11 量级约
/// 合百秒级纯 wasm 执行（校准值，非精确秒表——它是防失控的兜底，
/// 不是计量工具）；host 调用（等人/等模型）不消耗。
const CALL_FUEL: u64 = 100_000_000_000;
/// 挂载期列工具（compile_plugin 的临时实例）的燃料预算：秒级——
/// 挂载是同步路径，恶意/失控组件的初始化循环不能拖住启动（对抗
/// 自审 2026-08-21：此前误用全额 CALL_FUEL）。
const LIST_FUEL: u64 = 1_000_000_000;
/// 单组件线性内存上限。
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
/// W1-10：时钟等待的宿主侧切片上限——两次取消/预算检查点之间的最长
/// 间隔。组件的 `subscribe-duration`/`subscribe-instant` 等待以不超过
/// 此值的切片睡眠，取消令牌在片间可达（wasmtime 官方立场：epoch/fuel
/// 无法唤醒阻塞在 wasi:io/poll 睡眠里的宿主调用，见
/// wasmtime-48 `config.rs` "Interaction with blocking host calls"）。
const CLOCK_SLICE: Duration = Duration::from_millis(250);
/// W1-10：单次 invoke 的累计纯时钟等待预算（对齐 MCP `tools/call` 的
/// 120s 壁钟纪律）。超预算后的时钟订阅立即"就绪"——组件重新进入执行
/// 点，后续要么继续订阅（忙转，燃料迅速耗尽 trap）要么干活（烧燃料），
/// 所有既有防线（fuel/epoch）恢复可达。
const CALL_CLOCK_BUDGET: Duration = Duration::from_secs(120);
/// A4-3（W1-20）：组件文件大小上限——加载前判定，不进编译器。
pub(crate) const MAX_COMPONENT_BYTES: u64 = 32 * 1024 * 1024;
/// `/mcp` 面板显示的协议标签。
const WIT_PROTOCOL: &str = "clat-wit/0.1.0";
/// 单插件工具数上限（对齐 MCP 的 512 纪律——防御恶意/失控组件）。
const MAX_PLUGIN_TOOLS: usize = 512;

/// `~/.clat/plugins.json` 中一个插件的配置。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WasmPluginConfig {
    /// 组件文件路径：`~/…` 展开到 home；相对路径相对 `~/.clat`；
    /// 绝对路径原样。
    pub path: String,
    /// 额外授予目录（Phase 2b，仅 FullAccess 档且插件具备 fs 上限时
    /// 授予 RW）：绝对路径或 `~/…`；guest 路径 = 清洗后的目录名。
    #[serde(default)]
    pub dirs: Vec<String>,
    /// 插件自己的配置（Phase 2c，INV-K2）：任意 JSON 对象，宿主序列化
    /// 为字符串经 `clat:plugin/config` 导入供组件读取。
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// 组件文件 sha256 钉扎（A4-3/W1-20，可选）：hex（大小写不敏感）。
    /// 供应链静默替换面：配置了即校验，失配拒载。
    #[serde(default)]
    pub sha256: Option<String>,
}

pub type WasmPluginMap = BTreeMap<String, WasmPluginConfig>;

/// 读 `plugins.json`：文件缺席 = 空配置（零插件零成本）；存在但解析
/// 失败 = fail-fast（用户手误应当被看见）。
pub(crate) fn load_wasm_config(root: &std::path::Path) -> Result<WasmPluginMap, std::io::Error> {
    let file = root.join("plugins.json");
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| std::io::Error::other(format!("parse {}: {error}", file.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error),
    }
}

/// 解析 `path` 字段：`~/x` → home/x（home 从 storage_root 的父目录
/// 推导），相对 → storage_root/x，绝对原样。
fn resolve_component_path(root: &std::path::Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("~/") {
        root.parent().unwrap_or(root).join(rest)
    } else if trimmed.starts_with('/') {
        PathBuf::from(trimmed)
    } else {
        root.join(trimmed)
    }
}

/// W1-10：一次 invoke 内共享的时钟等待状态（`PluginState.clock` 持有，
/// 每次 invoke 重置）。等待累计跨全部订阅求和——预算是调用级纪律，
/// 不是单次订阅纪律（恶意组件不能用"N 个小睡眠"绕过）。
#[derive(Clone, Default)]
struct ClockShared {
    cancel: Option<CancelToken>,
    waited_ns: Arc<AtomicU64>,
    exhausted: Arc<AtomicBool>,
}

impl ClockShared {
    fn begin_invoke(cancel: &CancelToken) -> Self {
        Self {
            cancel: Some(cancel.clone()),
            waited_ns: Arc::new(AtomicU64::new(0)),
            exhausted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn budget_ns(&self) -> u64 {
        CALL_CLOCK_BUDGET.as_nanos() as u64
    }

    /// 本订阅是否应立即返回（视为就绪）：已取消或调用级预算耗尽。
    fn should_stop_waiting(&self) -> bool {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return true;
        }
        self.waited_ns.load(AtomicOrdering::Relaxed) >= self.budget_ns()
    }
}

/// 一个有界的时钟订阅（W1-10）。与 wasmtime-wasi 默认实现的关键差异：
/// 默认对无法表示的远期时长（u64::MAX 纳秒级）落 `Deadline::Never` —
/// `pending().await` 永久阻塞宿主线程，取消/燃料/epoch 全部失效；本实现
/// 片式睡眠，片间检查取消与调用级预算，等待总时长语义不变（合法睡眠
/// 不提前就绪）。
struct ClockWait {
    remaining_ns: u64,
    shared: ClockShared,
}

#[async_trait::async_trait]
impl Pollable for ClockWait {
    async fn ready(&mut self) {
        loop {
            if self.remaining_ns == 0 || self.shared.should_stop_waiting() {
                if self.shared.waited_ns.load(AtomicOrdering::Relaxed) >= self.shared.budget_ns() {
                    self.shared.exhausted.store(true, AtomicOrdering::Release);
                }
                return;
            }
            let slice = self.remaining_ns.min(CLOCK_SLICE.as_nanos() as u64);
            std::thread::sleep(Duration::from_nanos(slice));
            self.remaining_ns -= slice;
            self.shared
                .waited_ns
                .fetch_add(slice, AtomicOrdering::Relaxed);
        }
    }
}

/// 每组件的宿主状态：桥引用、发起方标签、零授权 WASI 上下文与资
/// 源限额（store data）。
struct PluginState {
    bridge: Arc<PluginHostBridge>,
    source: PluginSource,
    limits: StoreLimits,
    /// 零授权 WASI（INV-W1）：builder 不加 preopen/env/stdio/sockets，
    /// 接口可用而能力为空——2b 的能力授予即往这里加。
    wasi: WasiCtx,
    table: ResourceTable,
    /// 本插件的配置 JSON（Phase 2c，INV-K2：只有自己的，未配置为
    /// None → config::get 报错而非静默空串）。
    config: Option<String>,
    /// W1-10：时钟等待状态（每次 invoke 重置；列工具的临时实例用
    /// 默认值——其燃料本就秒级，且不接取消令牌）。
    clock: ClockShared,
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// 单调时钟的任意纪元（WASI 语义：纳秒、起点任意、只保证单调）。
static MONOTONIC_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

impl PluginState {
    /// 建立一个有界时钟订阅（W1-10）。
    fn subscribe_clock_wait(
        &mut self,
        duration_ns: u64,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        // 预算已耗尽：立即就绪（组件回到执行点，忙转由燃料收尾）。
        let wait = ClockWait {
            remaining_ns: duration_ns,
            shared: self.clock.clone(),
        };
        let resource = self.table.push(wait)?;
        wasmtime_wasi::p2::subscribe(&mut self.table, resource)
    }
}

/// W1-10：替换 wasmtime-wasi 默认 `monotonic-clock` 宿主——默认实现对
/// 无法表示的远期时长落 `Deadline::Never`（永久阻塞，见
/// wasmtime-wasi-48 `p2/host/clocks.rs`），取消/燃料/epoch 全部失效；
/// 本实现对每个订阅走 [`ClockWait`] 的片式有界等待。
impl monotonic_clock::Host for PluginState {
    fn now(&mut self) -> wasmtime::Result<monotonic_clock::Instant> {
        Ok(MONOTONIC_EPOCH.elapsed().as_nanos() as u64)
    }

    fn resolution(&mut self) -> wasmtime::Result<monotonic_clock::Instant> {
        Ok(1)
    }

    fn subscribe_instant(
        &mut self,
        when: monotonic_clock::Instant,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        let remaining = when.saturating_sub(self.now()?);
        self.subscribe_clock_wait(remaining)
    }

    fn subscribe_duration(
        &mut self,
        duration: monotonic_clock::Duration,
    ) -> wasmtime::Result<Resource<DynPollable>> {
        self.subscribe_clock_wait(duration)
    }
}

/// 组装 WASI linker（W1-10）：逐接口镜像 wasmtime-wasi
/// `p2::add_to_linker_sync` 的内部组合，唯独把 `clocks/monotonic-clock`
/// 的宿主从默认 `WasiClocks` 换成本文件的 [`PluginState`] 有界实现。
/// wasmtime-wasi 升级时须对照其 `p2/mod.rs` 的
/// `add_to_linker_with_options_sync` / `add_sync_wasi_io` 复核本清单
/// （guard 测试：现有 wat 组件测试全量走本组装，接口缺失会在实例化
/// 处立即报错）。
fn add_wasi_to_linker_bounded_clocks(linker: &mut Linker<PluginState>) -> wasmtime::Result<()> {
    use wasmtime_wasi::cli::{WasiCli, WasiCliView};
    use wasmtime_wasi::clocks::{WasiClocks, WasiClocksView};
    use wasmtime_wasi::filesystem::{WasiFilesystem, WasiFilesystemView};
    use wasmtime_wasi::p2::bindings::{cli, clocks, filesystem, random, sockets, sync};
    use wasmtime_wasi::random::{WasiRandom, WasiRandomView};
    use wasmtime_wasi::sockets::{WasiSockets, WasiSocketsView};

    struct IoTable;
    impl HasData for IoTable {
        type Data<'a> = &'a mut ResourceTable;
    }

    let l = linker;
    // wasi:io（错误/poll/流）——同步变体，Host 直接实现在 ResourceTable。
    wasmtime_wasi_io::bindings::wasi::io::error::add_to_linker::<PluginState, IoTable>(l, |t| {
        t.ctx().table
    })?;
    sync::io::poll::add_to_linker::<PluginState, IoTable>(l, |t| t.ctx().table)?;
    sync::io::streams::add_to_linker::<PluginState, IoTable>(l, |t| t.ctx().table)?;
    // 时钟：wall 用默认实现，monotonic 换有界实现（本文件 W1-10）。
    clocks::wall_clock::add_to_linker::<PluginState, WasiClocks>(
        l,
        <PluginState as WasiClocksView>::clocks,
    )?;
    clocks::monotonic_clock::add_to_linker::<PluginState, wasmtime::component::HasSelf<PluginState>>(
        l,
        |t| t,
    )?;
    // 文件系统（preopens + 同步 types）。
    filesystem::preopens::add_to_linker::<PluginState, WasiFilesystem>(
        l,
        <PluginState as WasiFilesystemView>::filesystem,
    )?;
    sync::filesystem::types::add_to_linker::<PluginState, WasiFilesystem>(
        l,
        <PluginState as WasiFilesystemView>::filesystem,
    )?;
    // 随机。
    random::random::add_to_linker::<PluginState, WasiRandom>(l, |t| t.random())?;
    random::insecure::add_to_linker::<PluginState, WasiRandom>(l, |t| t.random())?;
    random::insecure_seed::add_to_linker::<PluginState, WasiRandom>(l, |t| t.random())?;
    // cli 十件套。
    cli::exit::add_to_linker::<PluginState, WasiCli>(l, <PluginState as WasiCliView>::cli)?;
    cli::environment::add_to_linker::<PluginState, WasiCli>(l, <PluginState as WasiCliView>::cli)?;
    cli::stdin::add_to_linker::<PluginState, WasiCli>(l, <PluginState as WasiCliView>::cli)?;
    cli::stdout::add_to_linker::<PluginState, WasiCli>(l, <PluginState as WasiCliView>::cli)?;
    cli::stderr::add_to_linker::<PluginState, WasiCli>(l, <PluginState as WasiCliView>::cli)?;
    cli::terminal_input::add_to_linker::<PluginState, WasiCli>(
        l,
        <PluginState as WasiCliView>::cli,
    )?;
    cli::terminal_output::add_to_linker::<PluginState, WasiCli>(
        l,
        <PluginState as WasiCliView>::cli,
    )?;
    cli::terminal_stdin::add_to_linker::<PluginState, WasiCli>(
        l,
        <PluginState as WasiCliView>::cli,
    )?;
    cli::terminal_stdout::add_to_linker::<PluginState, WasiCli>(
        l,
        <PluginState as WasiCliView>::cli,
    )?;
    cli::terminal_stderr::add_to_linker::<PluginState, WasiCli>(
        l,
        <PluginState as WasiCliView>::cli,
    )?;
    // sockets（同步变体 + 非 IO 的四个配套接口）。
    sync::sockets::tcp::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sync::sockets::udp::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sync::sockets::udp_create_socket::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sockets::tcp_create_socket::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sockets::instance_network::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sockets::network::add_to_linker::<PluginState, WasiSockets>(
        l,
        &Default::default(),
        <PluginState as WasiSocketsView>::sockets,
    )?;
    sockets::ip_name_lookup::add_to_linker::<PluginState, WasiSockets>(
        l,
        <PluginState as WasiSocketsView>::sockets,
    )?;
    Ok(())
}

impl clat::plugin::sampling::Host for PluginState {
    fn create_message(
        &mut self,
        request: clat::plugin::sampling::Request,
    ) -> Result<clat::plugin::sampling::Outcome, String> {
        let domain = SamplingRequest {
            system_prompt: request.system_prompt,
            messages: request
                .messages
                .into_iter()
                .map(|message| SamplingMessage {
                    role: match message.role {
                        clat::plugin::sampling::Role::User => SamplingRole::User,
                        clat::plugin::sampling::Role::Assistant => SamplingRole::Assistant,
                    },
                    text: message.text,
                })
                .collect(),
            max_tokens: request.max_tokens,
            stop_sequences: Vec::new(),
            temperature: request.temperature,
        };
        match self.bridge.sample(self.source.clone(), domain) {
            Ok(outcome) => Ok(clat::plugin::sampling::Outcome {
                text: outcome.text,
                model: outcome.model,
                stop_reason: outcome.stop_reason,
            }),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl clat::plugin::elicitation::Host for PluginState {
    fn elicit(
        &mut self,
        form: clat::plugin::elicitation::Form,
    ) -> Result<clat::plugin::elicitation::Outcome, String> {
        let domain = ElicitForm {
            message: form.message,
            fields: form
                .fields
                .into_iter()
                .map(|field| ElicitField {
                    name: field.name,
                    title: field.title,
                    description: field.description,
                    kind: match field.kind {
                        clat::plugin::elicitation::FieldKind::Text => ElicitFieldKind::Text,
                        clat::plugin::elicitation::FieldKind::Number => ElicitFieldKind::Number,
                        clat::plugin::elicitation::FieldKind::Boolean => ElicitFieldKind::Boolean,
                        clat::plugin::elicitation::FieldKind::Choice => {
                            ElicitFieldKind::Choice(field.options)
                        }
                    },
                    required: field.required,
                })
                .collect(),
        };
        match self.bridge.elicit(domain) {
            Ok(ElicitOutcome::Accepted(content)) => {
                Ok(clat::plugin::elicitation::Outcome::Accepted(
                    content
                        .into_iter()
                        .map(|(name, value)| (name, wit_value(value)))
                        .collect(),
                ))
            }
            Ok(ElicitOutcome::Declined) => Ok(clat::plugin::elicitation::Outcome::Declined),
            Ok(ElicitOutcome::Cancelled) => Ok(clat::plugin::elicitation::Outcome::Cancelled),
            Err(error) => Err(error.to_string()),
        }
    }
}

impl clat::plugin::config::Host for PluginState {
    fn get(&mut self) -> Result<String, String> {
        self.config.clone().ok_or_else(|| {
            "no config provided for this plugin (add a `config` object to its              plugins.json entry)"
                .to_owned()
        })
    }
}

/// serde 值 → WIT value（elicitation 应答的回传方向）。
fn wit_value(value: Value) -> clat::plugin::elicitation::Value {
    match value {
        Value::Bool(flag) => clat::plugin::elicitation::Value::Boolean(flag),
        Value::Number(number) => {
            clat::plugin::elicitation::Value::Number(number.as_f64().unwrap_or_default())
        }
        Value::String(text) => clat::plugin::elicitation::Value::Text(text),
        other => clat::plugin::elicitation::Value::Text(other.to_string()),
    }
}

/// WIT effect → CLAT ToolEffect（八值一一对应）。
fn tool_effect(effect: exports::clat::plugin::tools::Effect) -> ToolEffect {
    match effect {
        exports::clat::plugin::tools::Effect::Pure => ToolEffect::Pure,
        exports::clat::plugin::tools::Effect::Read => ToolEffect::Read,
        exports::clat::plugin::tools::Effect::Write => ToolEffect::Write,
        exports::clat::plugin::tools::Effect::Execute => ToolEffect::Execute,
        exports::clat::plugin::tools::Effect::Network => ToolEffect::Network,
        exports::clat::plugin::tools::Effect::ExternalRead => ToolEffect::ExternalRead,
        exports::clat::plugin::tools::Effect::Destructive => ToolEffect::Destructive,
        exports::clat::plugin::tools::Effect::SessionWrite => ToolEffect::SessionWrite,
    }
}

/// 一项 preopen 授予：宿主目录 + guest 路径 + 读写性。
struct Grant {
    host: PathBuf,
    guest: String,
    read_write: bool,
}

/// Phase 2b 授予策略（INV-G1/G2）：per-plugin 构造（fs 上限与额外目
/// 录随插件配置/声明），实例化时求值。
struct GrantPolicy {
    /// 档位 cell（TUI Shared 模式；None = Classic/exec → 恒 RO）。
    mode: Option<Arc<std::sync::RwLock<crate::permission::PermissionMode>>>,
    /// 项目根（guest 路径恒为 `project`）。
    project_root: PathBuf,
    /// 插件声明了 Write/Execute/Destructive 工具（fs 上限——对齐
    /// "原生读工具物理上不能写"的能力形状）。
    fs_cap: bool,
    /// 额外目录（仅 FA 档 + fs 上限时授予 RW）。
    extra_dirs: Vec<PathBuf>,
}

impl GrantPolicy {
    fn current_mode(&self) -> Option<crate::permission::PermissionMode> {
        self.mode
            .as_ref()
            .and_then(|cell| cell.read().ok())
            .map(|guard| *guard)
    }

    /// B5：当前档位下「若过写授予门」将获 RW 的宿主目录集（空 =
    /// 无需写授予：RO 档 / Classic / 组件无 fs_cap）。审批请求与记录
    /// 比对都用它——审批面 = 实际授予面（INV-W1）。
    fn write_dirs(&self, mode: Option<crate::permission::PermissionMode>) -> Vec<PathBuf> {
        use crate::permission::PermissionMode;
        if !self.fs_cap {
            return Vec::new();
        }
        match mode {
            Some(PermissionMode::ProjectWrite) => vec![self.project_root.clone()],
            Some(PermissionMode::FullAccess) => {
                let mut dirs = vec![self.project_root.clone()];
                dirs.extend(self.extra_dirs.iter().cloned());
                dirs
            }
            _ => Vec::new(),
        }
    }

    /// 求值当前授予集（INV-G1：授予 = min(档位, 插件能力上限)）。
    /// B5 起 `write_allowed` 是写授予门的裁决（INV-W2：无记录/被拒 =
    /// 物理只读 preopen）；mode 由调用方快照传入，与门裁决同源——
    /// 门问的是这个档位的目录集，建 slot 授予的必须是同一份。
    fn grants(
        &self,
        mode: Option<crate::permission::PermissionMode>,
        write_allowed: bool,
    ) -> Vec<Grant> {
        use crate::permission::PermissionMode;
        let mut grants = vec![Grant {
            host: self.project_root.clone(),
            guest: "project".to_owned(),
            read_write: false,
        }];
        let rw_project = write_allowed
            && self.fs_cap
            && matches!(
                mode,
                Some(PermissionMode::ProjectWrite) | Some(PermissionMode::FullAccess)
            );
        if rw_project {
            grants[0].read_write = true;
        }
        if write_allowed && self.fs_cap && mode == Some(PermissionMode::FullAccess) {
            let mut used = std::collections::HashSet::from(["project".to_owned()]);
            for (index, host) in self.extra_dirs.iter().enumerate() {
                let guest = guest_path_for(host, index, &mut used);
                grants.push(Grant {
                    host: host.clone(),
                    guest,
                    read_write: true,
                });
            }
        }
        grants
    }
}

/// 额外目录的 guest 路径：清洗后的目录名；空/撞名回落 `dirN`。
fn guest_path_for(
    host: &std::path::Path,
    index: usize,
    used: &mut std::collections::HashSet<String>,
) -> String {
    let candidate: String = host
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let candidate = candidate.trim_matches('-').to_owned();
    if !candidate.is_empty() && !used.contains(&candidate) {
        used.insert(candidate.clone());
        return candidate;
    }
    let fallback = format!("dir{index}");
    used.insert(fallback.clone());
    fallback
}

/// 实例化产物：store + 类型化实例 + 建立时的档位与写授予快照（重建键）。
struct InstanceSlot {
    mode_key: Option<crate::permission::PermissionMode>,
    /// B5：建立时写授予门的裁决（INV-W2）——与档位一起构成重建键：
    /// 跨 run 的「记录生效/本 run 被拒」状态变化正确触发重建。
    write_key: bool,
    store: Store<PluginState>,
    instance: Plugin,
}

/// 组件的共享单元：编译产物 + linker + 授予策略；实例按"档位或写授予
/// 变更即重建"缓存（INV-G3：重建即失效——组件内存态不跨档位保留）。
/// teardown 置 None（在途调用自然失败），调用按需加锁。
struct WasmInstance {
    name: String,
    engine: Engine,
    linker: Arc<Linker<PluginState>>,
    component: Arc<wasmtime::component::Component>,
    host: Arc<PluginHostBridge>,
    grants: GrantPolicy,
    /// 本插件的配置 JSON 字符串（Phase 2c）。
    config: Option<String>,
    /// 单次调用燃料预算（INV-W3；测试用小额验证打断）。
    fuel: u64,
    /// B5：组件 sha256——写授予记录三要素之一。
    digest: String,
    /// B5：写授予记录文件路径（storage_root/plugin-grants.json）。
    grants_path: PathBuf,
    /// B5：本插件写授予的 per-run 裁决缓存（桥纪元, denied）——同 run
    /// 内被拒后不再重问（INV-W3）；新 run（新纪元）重新问。
    write_state: Mutex<Option<(u64, bool)>>,
    slot: Mutex<Option<InstanceSlot>>,
}

impl WasmInstance {
    /// B5（INV-W2/W3/W5/W6）：解析本插件在快照档位下能否获得写授予。
    /// 可能阻塞等人审批（持 slot 锁串行化同插件调用，无并发重问）；
    /// 返回 false 时本 slot 一律物理只读。
    fn resolve_write_grant(
        &self,
        mode: Option<crate::permission::PermissionMode>,
        cancel: &CancelToken,
    ) -> bool {
        let requested = self.grants.write_dirs(mode);
        if requested.is_empty() {
            return false;
        }
        // 有效记录（INV-W1：请求 ⊆ 记录并集）→ 静默授予。
        let mut records = wasm_grants::load_grants(&self.grants_path);
        if wasm_grants::covers(&records, &self.name, &self.digest, &requested) {
            return true;
        }
        // INV-W6：无活动 run（boot/mount 期、run 间隙、headless 无桥）
        // → 无审批面 → fail-closed（INV-W2）。
        let Some((epoch, context)) = self.host.context() else {
            return false;
        };
        // INV-W3：同 run 已裁决 → 不再问（被拒的 run 内不重复打扰）。
        if let Ok(state) = self.write_state.lock()
            && let Some((cached_epoch, denied)) = *state
            && cached_epoch == epoch
        {
            return !denied;
        }
        // INV-W5：审批走当前 run 的 approver + 取消令牌；请求列出全部
        // 将获 RW 的目录与组件摘要。弹窗中途升档（w/f）会改变目录集，
        // Allow 后复算——一致才落记录，变了以新集合重问（记录绑定实际
        // 授予面）。
        let mut asked = requested;
        let digest_head = &self.digest[..self.digest.len().min(8)];
        let decision = loop {
            let request = crate::permission::PermissionRequest {
                tool: format!("wasm:{}", self.name),
                effect: crate::tool::ToolEffect::Write,
                reason: format!(
                    "wasm plugin `{}` requests filesystem WRITE access to {} \
                     directories (component sha256 {digest_head}…); approving \
                     persists a grant bound to this component and these directories",
                    self.name,
                    asked.len(),
                ),
                arguments: serde_json::json!({
                    "plugin": self.name,
                    "component_sha256": self.digest,
                    "write_dirs": asked
                        .iter()
                        .map(|dir| dir.display().to_string())
                        .collect::<Vec<String>>(),
                }),
                call_id: String::new(),
            };
            match context.approver.decide(request, &context.cancel) {
                crate::permission::PermissionDecision::Allow => {
                    let now = self.grants.write_dirs(self.grants.current_mode());
                    if now == asked || now.is_empty() {
                        break crate::permission::PermissionDecision::Allow;
                    }
                    asked = now;
                }
                other => break other,
            }
        };
        let allowed = matches!(decision, crate::permission::PermissionDecision::Allow);
        if let Ok(mut state) = self.write_state.lock() {
            *state = Some((epoch, !allowed));
        }
        if allowed {
            // INV-W4：落记录失败不致命（本 run 仍放行，下次重问）。
            wasm_grants::upsert(&mut records, &self.name, &self.digest, &asked);
            if let Err(error) = wasm_grants::save_grants(&self.grants_path, &records) {
                eprintln!(
                    "clat: warning: cannot persist the wasm write grant for `{}` \
                     (you will be asked again): {error}",
                    self.name
                );
            }
        } else if !cancel.is_cancelled() {
            let reason = match decision {
                crate::permission::PermissionDecision::Deny { reason }
                | crate::permission::PermissionDecision::Ask { reason }
                | crate::permission::PermissionDecision::Unavailable { reason } => reason,
                crate::permission::PermissionDecision::Allow => String::new(),
            };
            eprintln!(
                "clat: warning: wasm plugin `{}` write access not granted{reason_note}; \
                 its writes will fail read-only for this run",
                self.name,
                reason_note = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" ({reason})")
                },
            );
        }
        allowed
    }

    /// 在当前档位下的实例上执行闭包；档位或写授予变更（或首调）时先
    /// 重建（INV-G1：授予面 = 当次调用时的档位；INV-G3：重建即失效）。
    fn with_slot<R>(
        &self,
        cancel: &CancelToken,
        call: impl FnOnce(&mut InstanceSlot) -> R,
    ) -> Result<R, ToolError> {
        let mode_now = self.grants.current_mode();
        // B5：写授予门在 slot 锁内解析（同插件并发调用自然串行）。
        let mut guard = self
            .slot
            .lock()
            .map_err(|_| ToolError::new("wasm plugin state poisoned"))?;
        let write_allowed = self.resolve_write_grant(mode_now, cancel);
        let rebuild = guard
            .as_ref()
            .map(|slot| slot.mode_key != mode_now || slot.write_key != write_allowed)
            .unwrap_or(true);
        if rebuild {
            *guard = Some(build_slot(self, mode_now, write_allowed)?);
        }
        let slot = guard
            .as_mut()
            .ok_or_else(|| ToolError::new("wasm plugin is shutting down"))?;
        // INV-W3：每次调用重置燃料预算（host 等待不消耗——等人不
        // 再烧预算，2d 修复）。W1-01：epoch deadline = 当前刻度 + 1——
        // 只有取消观察者推进刻度才会 trap；store 创建时的远置 deadline
        //（见 build_slot）在此收紧到执行期语义。W1-10：时钟等待状态
        // 同步重置（取消令牌 + 调用级累计预算归零）。
        slot.store
            .set_fuel(self.fuel)
            .map_err(|error| ToolError::new(format!("set fuel: {error}")))?;
        slot.store.set_epoch_deadline(1);
        slot.store.data_mut().clock = ClockShared::begin_invoke(cancel);
        Ok(call(slot))
    }
}

/// 按当前授予集构建实例（preopen → store → instantiate）。B5 起 RW
/// 授予以写授予门裁决为准（INV-W2：无记录/被拒 = 物理只读 preopen）。
fn build_slot(
    instance: &WasmInstance,
    mode_key: Option<crate::permission::PermissionMode>,
    write_allowed: bool,
) -> Result<InstanceSlot, ToolError> {
    let mut builder = WasiCtxBuilder::new();
    for grant in instance.grants.grants(mode_key, write_allowed) {
        builder
            .preopened_dir(
                &grant.host,
                &grant.guest,
                if grant.read_write {
                    wasmtime_wasi::FsPerms::ReadWrite
                } else {
                    wasmtime_wasi::FsPerms::ReadOnly
                },
            )
            .map_err(|error| {
                ToolError::new(format!(
                    "preopen {} as `{}`: {error}",
                    grant.host.display(),
                    grant.guest
                ))
            })?;
    }
    let limits = StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build();
    let mut store = Store::new(
        &instance.engine,
        PluginState {
            bridge: Arc::clone(&instance.host),
            source: PluginSource::Wasm(instance.name.clone()),
            limits,
            wasi: builder.build(),
            table: ResourceTable::new(),
            config: instance.config.clone(),
            clock: ClockShared::default(),
        },
    );
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(instance.fuel)
        .map_err(|error| ToolError::new(format!("set fuel: {error}")))?;
    // epoch 中断开启后 deadline 缺省为 0（立即 trap）：store 创建期先
    // 远置，进 invoke 时再收紧到 +1（W1-01）。
    store.set_epoch_deadline(u64::MAX / 2);
    let component_instance = Plugin::instantiate(&mut store, &instance.component, &instance.linker)
        .map_err(|error| {
            ToolError::new(format!(
                "wasm plugin `{}` failed to instantiate: {error}",
                instance.name
            ))
        })?;
    Ok(InstanceSlot {
        mode_key,
        write_key: write_allowed,
        store,
        instance: component_instance,
    })
}

/// 取消观察者（W1-01）：invoke 期间短轮询取消令牌，置位即推进
/// engine epoch——本 store 的 deadline（当前刻度 + 1）使组件在下一个
/// 执行点 trap。轮询而非回调：`CancelToken` 是纯原子标志。组件阻塞
/// 在 host 调用（等人/等模型）时不执行指令、不吃 epoch trap——
/// "等待不烧预算"不变量保持；取消后从 host 调用返回的第一个执行点
/// 即中断。同一 run 的取消令牌是共享的，跨实例的刻度推进语义一致
/// （工具调用在 run 内串行，实际不并发）。
struct CancelWatcher {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CancelWatcher {
    fn start(engine: &Engine, cancel: &CancelToken) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_done = std::sync::Arc::clone(&done);
        let engine = engine.clone();
        let cancel = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("clat-wasm-cancel".into())
            .spawn(move || {
                while !watcher_done.load(std::sync::atomic::Ordering::Acquire) {
                    if cancel.is_cancelled() {
                        engine.increment_epoch();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
            .expect("spawn wasm cancel watcher");
        Self {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for CancelWatcher {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 一个 wasm 组件工具（`wasm_{plugin}_{tool}`）。
pub struct WasmTool {
    remote_name: String,
    definition: ToolDefinition,
    instance: Arc<WasmInstance>,
}

impl Tool for WasmTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new(
                "wasm tool skipped: the run was already cancelled",
            ));
        }
        let arguments = serde_json::to_string(arguments)
            .map_err(|error| ToolError::new(format!("serialize wasm tool arguments: {error}")))?;
        let remote_name = self.remote_name.clone();
        let plugin = self.instance.name.clone();
        // W1-01：取消令牌是执行期能力——观察者在组件执行期间推进
        // epoch，spin 类组件在毫秒级 trap，不必等燃料耗尽。
        let watcher = CancelWatcher::start(&self.instance.engine, cancel);
        // INV-W3：超时/超限以 trap 返回，映射为工具错误（run 不死）。
        let call = self.instance.with_slot(cancel, |slot| {
            slot.instance
                .clat_plugin_tools()
                .call_call(&mut slot.store, &remote_name, &arguments)
        });
        drop(watcher);
        match call? {
            Err(error) => Err(ToolError::new(if cancel.is_cancelled() {
                // epoch trap 或取消后的首个失败都归因为中断。
                format!("wasm plugin `{plugin}` interrupted: run was cancelled ({error})")
            } else {
                format!("wasm plugin `{plugin}` failed: {error}")
            })),
            Ok(Ok(text)) => Ok(serde_json::from_str(&text).unwrap_or(Value::String(text))),
            Ok(Err(message)) => Err(ToolError::new(message)),
        }
    }
}

pub(crate) struct WasmAdapterPlugin {
    storage_root: PathBuf,
    host: Arc<PluginHostBridge>,
    /// Phase 2b：授予求值输入——项目根与档位 cell（None = Classic）。
    project_root: PathBuf,
    permission_mode: Option<Arc<std::sync::RwLock<crate::permission::PermissionMode>>>,
    /// 单次调用燃料预算（INV-W3）；测试用小额验证打断。
    fuel: u64,
}

impl WasmAdapterPlugin {
    pub(crate) fn new(
        storage_root: PathBuf,
        host: Arc<PluginHostBridge>,
        project_root: PathBuf,
        permission_mode: Option<Arc<std::sync::RwLock<crate::permission::PermissionMode>>>,
    ) -> Self {
        Self {
            storage_root,
            host,
            project_root,
            permission_mode,
            fuel: CALL_FUEL,
        }
    }

    /// 缩小燃料预算（门控测试验证 INV-W3 的打断用）。
    #[cfg(test)]
    pub(crate) fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }
}

impl PluginTrait for WasmAdapterPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let status = context
            .try_require(MCP_STATUS_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let config = load_wasm_config(&self.storage_root)
            .map_err(|error| PluginError::new(error.to_string()))?;
        // INV-W6：与 MCP 同一状态面板；configured 分母先扩（wasm 同步
        // 挂载，随后逐插件落定）。
        if let Some(status) = &status {
            status.extend_configured(config.len());
        }
        if config.is_empty() {
            return Ok(());
        }

        // 引擎（INV-W3：fuel 计量——无 ticker 线程，host 等待不烧预算；
        // W1-01：epoch 中断——取消观察者推进刻度，执行期 trap）。
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(true);
        engine_config.epoch_interruption(true);
        let engine = Engine::new(&engine_config)
            .map_err(|error| PluginError::new(format!("wasmtime engine: {error}")))?;

        let mut linker: Linker<PluginState> = Linker::new(&engine);
        // WASI 接口（能力边界在 WasiCtx：preopen 授予按档位求值，
        // INV-G1/G2）。W1-10：monotonic-clock 换有界实现（默认实现对
        // 远期时长永久阻塞宿主线程，取消/燃料不可达）。
        add_wasi_to_linker_bounded_clocks(&mut linker)
            .map_err(|error| PluginError::new(format!("wasi linker: {error}")))?;
        // v48 绑定约定：D = HasSelf<PluginState>（Data<'a> = &'a mut
        // PluginState，Host 经 &mut 转发到 PluginState 的实现）。
        Plugin::add_to_linker::<PluginState, wasmtime::component::HasSelf<PluginState>>(
            &mut linker,
            |state: &mut PluginState| state,
        )
        .map_err(|error| PluginError::new(format!("wasm linker: {error}")))?;
        let linker = Arc::new(linker);

        // 逐插件加载（INV-W5：失败隔离——坏插件记入状态，其余照常）。
        let owner = context.owner();
        let mut instances: Vec<Arc<WasmInstance>> = Vec::new();
        let mut leases = Vec::new();
        // 非 server 级失败（空名段/工具注册失败）上报状态面板（对齐
        // MCP 适配器的 record_failure 语义——对抗自审 2026-08-21：此前
        // 仅收集进本地 vec 后丢弃，工具静默失踪）。
        let record_failure = |message: String| {
            if let Some(status) = &status {
                status.record_failure(message);
            }
        };
        for (name, plugin_config) in &config {
            match compile_plugin(&engine, &linker, &self.storage_root, name, plugin_config) {
                Ok(compiled) => {
                    // A4-2：元数据消毒诊断上状态面板（不拖垮整个插件）。
                    for diagnostic in &compiled.diagnostics {
                        record_failure(diagnostic.clone());
                    }
                    let mut tools = 0usize;
                    let fs_cap = compiled.definitions.iter().any(|definition| {
                        matches!(
                            definition.effect,
                            ToolEffect::Write | ToolEffect::Execute | ToolEffect::Destructive
                        )
                    });
                    let shared = Arc::new(WasmInstance {
                        name: name.clone(),
                        engine: engine.clone(),
                        linker: Arc::clone(&linker),
                        component: Arc::new(compiled.component),
                        host: Arc::clone(&self.host),
                        grants: GrantPolicy {
                            mode: self.permission_mode.clone(),
                            project_root: self.project_root.clone(),
                            fs_cap,
                            extra_dirs: compiled.extra_dirs,
                        },
                        config: compiled.config,
                        fuel: self.fuel,
                        digest: compiled.digest,
                        grants_path: wasm_grants::grants_path(&self.storage_root),
                        write_state: Mutex::new(None),
                        slot: Mutex::new(None),
                    });
                    for definition in compiled.definitions {
                        let remote_name = definition.name.clone();
                        let Some(qualified) =
                            qualify_prefixed_tool_name("wasm", name, &remote_name)
                        else {
                            record_failure(format!(
                                "wasm `{name}` tool `{remote_name}`: empty name segment"
                            ));
                            continue;
                        };
                        let tool = WasmTool {
                            remote_name,
                            definition: ToolDefinition {
                                name: qualified,
                                ..definition
                            },
                            instance: Arc::clone(&shared),
                        };
                        match registry.register(owner, Arc::new(tool)) {
                            Ok(lease) => {
                                leases.push(lease);
                                tools += 1;
                            }
                            Err(error) => {
                                record_failure(format!("wasm `{name}` tool registration: {error}"));
                            }
                        }
                    }
                    instances.push(shared);
                    if let Some(status) = &status {
                        status.record_connected(McpServerStatus {
                            name: name.clone(),
                            server_version: WASMTIME_VERSION.to_owned(),
                            protocol_version: WIT_PROTOCOL.to_owned(),
                            tools,
                            transport: "wasm".to_owned(),
                        });
                    }
                }
                Err(error) => {
                    if let Some(status) = &status {
                        status.record_failed_server(format!("wasm `{name}`: {error}"));
                    }
                }
            }
        }

        // teardown：撤工具 lease → 释放实例/store（fuel 化后无后台
        // 线程需要停机）。
        context.defer(move || {
            for lease in leases.drain(..) {
                let _ = lease.revoke();
            }
            for instance in instances.drain(..) {
                if let Ok(mut slot) = instance.slot.lock() {
                    *slot = None;
                }
            }
            Ok(())
        });
        Ok(())
    }
}

/// wasmtime 无运行时版本查询 API——随升级手动同步（面板显示用）。
const WASMTIME_VERSION: &str = "48";

/// 编译产物：组件 + 工具声明 + 已验证的额外授予目录。
struct CompiledPlugin {
    component: wasmtime::component::Component,
    definitions: Vec<ToolDefinition>,
    extra_dirs: Vec<PathBuf>,
    /// A4-2：元数据消毒诊断（mount 记入状态面板）。
    diagnostics: Vec<String>,
    /// 本插件的配置 JSON 字符串（Phase 2c）。
    config: Option<String>,
    /// B5：组件 sha256（小写 hex）——写授予记录三要素之一，无条件
    /// 计算（A4-3 钉扎校验复用同一摘要，不再只配 pin 才算）。
    digest: String,
}

/// WIT 工具声明到宿主 ToolDefinition 字段的最小投影（A4-2 消毒的
/// 输入面——具体 bindgen 类型随 wasmtime 版本变，投影保持稳定）。
trait WitToolDef {
    fn wit_name(&self) -> String;
    fn wit_description(&self) -> String;
    fn wit_input_schema(&self) -> String;
    fn wit_effect(&self) -> crate::tool::ToolEffect;
}

impl WitToolDef for exports::clat::plugin::tools::Definition {
    fn wit_name(&self) -> String {
        self.name.clone()
    }
    fn wit_description(&self) -> String {
        self.description.clone()
    }
    fn wit_input_schema(&self) -> String {
        self.input_schema.clone()
    }
    fn wit_effect(&self) -> ToolEffect {
        tool_effect(self.effect)
    }
}

/// 编译组件并用零授权临时实例列出其工具（列工具不需要 fs；正式实例
/// 首次调用时按当前档位惰性建立——授予面永远等于调用时档位）。
fn compile_plugin(
    engine: &Engine,
    linker: &Linker<PluginState>,
    storage_root: &std::path::Path,
    name: &str,
    plugin_config: &WasmPluginConfig,
) -> Result<CompiledPlugin, String> {
    let path = resolve_component_path(storage_root, &plugin_config.path);
    if !path.is_file() {
        return Err(format!(
            "component file not found: {} (from `{}`)",
            path.display(),
            plugin_config.path
        ));
    }
    // A4-3：大小闸先于读取/编译——启动同步路径不被大文件拖住。
    let size = std::fs::metadata(&path)
        .map_err(|error| format!("stat {}: {error}", path.display()))?
        .len();
    if size > MAX_COMPONENT_BYTES {
        return Err(format!(
            "component {} is {} bytes; the cap is {MAX_COMPONENT_BYTES} (32 MiB)",
            path.display(),
            size
        ));
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    // B5：无条件计算组件 digest（写授予记录绑定它）；A4-3 钉扎比对
    // 同一摘要（配了 pin 才校验，语义不变）。
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(&bytes);
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if let Some(expected) = &plugin_config.sha256
        && !digest_hex.eq_ignore_ascii_case(expected.trim().trim_start_matches("sha256:"))
    {
        return Err(format!(
            "component {} sha256 mismatch: pinned {expected}, actual {digest_hex}",
            path.display()
        ));
    }
    let component = wasmtime::component::Component::from_binary(engine, &bytes)
        .map_err(|error| format!("compile {}: {error}", path.display()))?;
    // 额外授予目录（Phase 2b）：展开 + 前置校验（缺失/非目录即拒）。
    let mut extra_dirs = Vec::new();
    for raw in &plugin_config.dirs {
        let dir = resolve_component_path(storage_root, raw);
        if !dir.is_dir() {
            return Err(format!(
                "configured dir `{raw}` is not a directory: {}",
                dir.display()
            ));
        }
        extra_dirs.push(dir);
    }
    // 零授权临时实例列工具（工具声明不依赖授予）。
    let limits = StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build();
    let mut store = Store::new(
        engine,
        PluginState {
            bridge: crate::plugin_host::PluginHostBridge::shared(),
            source: PluginSource::Wasm(name.to_owned()),
            limits,
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            config: None,
            clock: ClockShared::default(),
        },
    );
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(LIST_FUEL)
        .map_err(|error| format!("set fuel: {error}"))?;
    store.set_epoch_deadline(u64::MAX / 2);
    let instance =
        Plugin::instantiate(&mut store, &component, linker).map_err(|error| error.to_string())?;
    let definitions = instance
        .clat_plugin_tools()
        .call_list_tools(&mut store)
        .map_err(|error| format!("list-tools: {error}"))?;
    if definitions.len() > MAX_PLUGIN_TOOLS {
        return Err(format!("plugin exposes more than {MAX_PLUGIN_TOOLS} tools"));
    }
    let (parsed, diagnostics) = sanitize_definitions(name, definitions);
    Ok(CompiledPlugin {
        component,
        definitions: parsed,
        extra_dirs,
        diagnostics,
        config: plugin_config
            .config
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok()),
        digest: digest_hex,
    })
}

/// A4-2（W1-19）：组件工具元数据消毒——description 超长截断（模型可
/// 见面防注入洪水）；input_schema 解析失败**拒注册**（fail-loud，此前
/// 静默降级 `{"type":"object"}` 让模型以无约束参数调用）。返回
/// (放行的定义, 诊断)——诊断由 mount 记入 /mcp 状态面板。
fn sanitize_definitions<T>(plugin: &str, definitions: Vec<T>) -> (Vec<ToolDefinition>, Vec<String>)
where
    T: WitToolDef,
{
    let mut parsed = Vec::new();
    let mut diagnostics = Vec::new();
    for definition in definitions {
        let tool = definition.wit_name();
        let mut description = format!("[wasm:{plugin}] {}", definition.wit_description());
        if description.chars().count() > crate::tool::MAX_TOOL_DESCRIPTION_CHARS {
            let kept: String = description
                .chars()
                .take(crate::tool::MAX_TOOL_DESCRIPTION_CHARS)
                .collect();
            description = format!("{kept}… [truncated by host]");
            diagnostics.push(format!(
                "wasm `{plugin}` tool `{tool}`: description exceeded \
                 {} chars and was truncated",
                crate::tool::MAX_TOOL_DESCRIPTION_CHARS
            ));
        }
        match serde_json::from_str(&definition.wit_input_schema()) {
            Ok(input_schema) => parsed.push(ToolDefinition {
                // 组件内名（mount 端限定为 wasm_{plugin}_{tool}）。
                name: definition.wit_name(),
                description,
                input_schema,
                effect: definition.wit_effect(),
                strict: false,
            }),
            Err(error) => diagnostics.push(format!(
                "wasm `{plugin}` tool `{tool}`: input_schema is not valid JSON \
                 ({error}); the tool was not registered"
            )),
        }
    }
    (parsed, diagnostics)
}

#[cfg(test)]
mod tests {
    /// A1：decide 携带 `&CancelToken` 后的具名 allow-all（闭包有 HRTB
    /// 推断限制）。
    fn allow_all_approver(
        _request: crate::permission::PermissionRequest,
        _cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }

    use super::super::wasm_grants;
    use super::*;
    use std::time::Duration;

    #[test]
    fn config_path_resolution_expands_tilde_and_relatives() {
        let root = std::path::Path::new("/home/user/.clat");
        assert_eq!(
            resolve_component_path(root, "~/plugins/d.wasm"),
            PathBuf::from("/home/user/plugins/d.wasm")
        );
        assert_eq!(
            resolve_component_path(root, "plugins/d.wasm"),
            PathBuf::from("/home/user/.clat/plugins/d.wasm")
        );
        assert_eq!(
            resolve_component_path(root, "/opt/d.wasm"),
            PathBuf::from("/opt/d.wasm")
        );
    }

    #[test]
    fn missing_config_file_means_no_plugins() {
        let root = std::env::temp_dir().join("clat-wasm-missing-config");
        std::fs::create_dir_all(&root).expect("root");
        assert!(load_wasm_config(&root).expect("load").is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_config_file_fails_fast() {
        let root = std::env::temp_dir().join("clat-wasm-bad-config");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("plugins.json"), b"{not json").expect("write");
        assert!(load_wasm_config(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    // ---- 门控端到端（需要 tests/fixtures/wasm/ 的组件；本地构建插件
    // 需 wasm32-wasip2 target，见 plugins/digest 与 plugins/probe）。

    use super::super::services::{MCP_STATUS_SERVICE, PROVIDER_SERVICE, TOOL_SERVICE};
    use crate::interaction::{AskAnswer, AskQuestion, UserAsker};
    use crate::model::{
        CancelToken, FinishReason, Model, ModelConfig, ModelError, ModelEventSink, ModelFactory,
        ModelProtocol, ModelRequest, ModelResponse, ProviderCredentials, ProviderDescriptor,
        Usage as ModelUsage,
    };
    use crate::permission::{PermissionApprover, PermissionDecision};
    use crate::plugin::{PluginId, PluginManager, PluginOwner, ScopeKind};
    use crate::plugins::{McpAdapterPlugin, ProviderRegistryPlugin, ToolRegistryPlugin};

    /// INV-G1 纯函数：授予 = min(档位, 插件能力上限, 写授予门裁决)。
    #[test]
    fn grant_matrix_maps_mode_and_capability() {
        use crate::permission::PermissionMode;
        let cell = std::sync::Arc::new(std::sync::RwLock::new(PermissionMode::ReadOnly));
        let policy = |fs_cap: bool, extra: Vec<PathBuf>| GrantPolicy {
            mode: Some(std::sync::Arc::clone(&cell)),
            project_root: PathBuf::from("/proj"),
            fs_cap,
            extra_dirs: extra,
        };
        let mode = |expected: PermissionMode| {
            *cell.write().expect("mode") = expected;
            Some(expected)
        };

        // Read Only：恒 RO（即使插件声明了写工具）。
        let grants = policy(true, Vec::new()).grants(Some(PermissionMode::ReadOnly), true);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].guest, "project");
        assert!(!grants[0].read_write);

        // Project Write：有 fs 上限 → RW；纯读插件 → RO。
        mode(PermissionMode::ProjectWrite);
        assert!(
            policy(true, Vec::new()).grants(Some(PermissionMode::ProjectWrite), true)[0].read_write
        );
        assert!(
            !policy(false, Vec::new()).grants(Some(PermissionMode::ProjectWrite), true)[0]
                .read_write
        );

        // Full Access：项目根 RW + 额外目录 RW（guest = 清洗目录名）。
        mode(PermissionMode::FullAccess);
        let grants = policy(
            true,
            vec![
                PathBuf::from("/Volumes/Data 1"),
                PathBuf::from("/x/project"),
            ],
        )
        .grants(Some(PermissionMode::FullAccess), true);
        assert_eq!(grants.len(), 3);
        assert!(grants.iter().all(|grant| grant.read_write));
        assert_eq!(grants[1].guest, "Data-1");
        assert_eq!(
            grants[2].guest, "dir1",
            "collision with `project` falls back"
        );
        // 纯读插件在 FA 也不得 RW/额外目录。
        let grants = policy(false, vec![PathBuf::from("/extra")])
            .grants(Some(PermissionMode::FullAccess), true);
        assert_eq!(grants.len(), 1);
        assert!(!grants[0].read_write);

        // B5（INV-W2）：写授予门未过（无记录且未获批/被拒）→ 即使档位
        // 与 fs_cap 都满足，一切 preopen 物理只读。
        mode(PermissionMode::ProjectWrite);
        let grants = policy(true, Vec::new()).grants(Some(PermissionMode::ProjectWrite), false);
        assert_eq!(grants.len(), 1);
        assert!(!grants[0].read_write);
        mode(PermissionMode::FullAccess);
        let grants = policy(true, vec![PathBuf::from("/extra")])
            .grants(Some(PermissionMode::FullAccess), false);
        assert_eq!(
            grants.len(),
            1,
            "extras are part of the write grant; without it only the project root mounts, read-only"
        );
        assert!(!grants[0].read_write);

        // B5：write_dirs = 审批面（档位 × fs_cap 求值，与 grants 的 RW
        // 分支同源）。RO/Classic/纯读 → 空；PW → [根]；FA → [根+extras]。
        mode(PermissionMode::ReadOnly);
        assert!(
            policy(true, vec![PathBuf::from("/extra")])
                .write_dirs(Some(PermissionMode::ReadOnly))
                .is_empty()
        );
        assert!(
            policy(false, vec![PathBuf::from("/extra")])
                .write_dirs(Some(PermissionMode::ProjectWrite))
                .is_empty()
        );
        assert_eq!(
            policy(true, Vec::new()).write_dirs(Some(PermissionMode::ProjectWrite)),
            vec![PathBuf::from("/proj")]
        );
        mode(PermissionMode::FullAccess);
        assert_eq!(
            policy(true, vec![PathBuf::from("/extra")])
                .write_dirs(Some(PermissionMode::FullAccess)),
            vec![PathBuf::from("/proj"), PathBuf::from("/extra")]
        );
        assert!(
            GrantPolicy {
                mode: None,
                project_root: PathBuf::from("/proj"),
                fs_cap: true,
                extra_dirs: vec![PathBuf::from("/extra")],
            }
            .write_dirs(None)
            .is_empty()
        );

        // Classic（exec，无档位 cell）：恒 RO。
        let grants = GrantPolicy {
            mode: None,
            project_root: PathBuf::from("/proj"),
            fs_cap: true,
            extra_dirs: vec![PathBuf::from("/extra")],
        }
        .grants(None, true);
        assert_eq!(grants.len(), 1);
        assert!(!grants[0].read_write);
    }

    /// INV-G1/G3 门控：read 插件在 RO 档读得到、写被拒；切 PW 后同一
    /// 挂载重建实例可写；FA 档额外目录按 guest 名可写。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn fs_grants_follow_the_permission_mode_and_rebuild() {
        use crate::permission::PermissionMode;
        let root = unique_root("fs");
        let project = unique_root("fs-project");
        std::fs::write(project.join("hello.txt"), b"hi wasm").expect("seed");
        let extra = unique_root("fs-extra");
        let extra_guest = extra
            .file_name()
            .and_then(|name| name.to_str())
            .expect("extra name")
            .to_owned();
        let map: BTreeMap<String, WasmPluginConfig> = BTreeMap::from([(
            "read".to_owned(),
            WasmPluginConfig {
                path: fixture("read.wasm").display().to_string(),
                dirs: vec![extra.display().to_string()],
                config: None,
                sha256: None,
            },
        )]);
        std::fs::write(
            root.join("plugins.json"),
            serde_json::to_vec(&map).expect("serialize"),
        )
        .expect("config");
        // B5：本测试不装桥上下文（无审批面）→ 写授予门 fail-closed。
        // 成功腿预置一份覆盖 FA 全集（根 + extra）的授权记录：RO 腿
        // 依旧失败（记录永不越过档位），PW/FA 腿静默 RW——本测试聚焦
        // 档位 × 物理授予，审批语义由下方 write_grants_* 四条钉住。
        let mut records = Vec::new();
        wasm_grants::upsert(
            &mut records,
            "read",
            &fixture_sha256("read.wasm"),
            &[project.clone(), extra.clone()],
        );
        wasm_grants::save_grants(&wasm_grants::grants_path(&root), &records).expect("seed grants");

        let mode = std::sync::Arc::new(std::sync::RwLock::new(PermissionMode::ReadOnly));
        let mcp_root = unique_root("fs-mcp");
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.clone(),
                crate::plugin_host::PluginHostBridge::shared(),
                project.clone(),
                Some(std::sync::Arc::clone(&mode)),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project_view = crate::project::Project::new(&project);
        let cancel = CancelToken::new();

        // RO：读放行（对齐原生读工具），写被 capability 边界拒绝。
        let output = registry
            .get("wasm_read_read_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "hello.txt"}),
                &project_view,
                &cancel,
            )
            .expect("read under Read Only");
        assert_eq!(output["content"], "hi wasm");
        let output = registry
            .get("wasm_read_list_dir")
            .expect("tool")
            .invoke(&serde_json::json!({"path": ""}), &project_view, &cancel)
            .expect("list under Read Only");
        assert!(
            output["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == "hello.txt")),
            "unexpected listing: {output}"
        );
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "denied"}),
                &project_view,
                &cancel,
            )
            .expect_err("write must be refused under Read Only");
        assert!(
            !project.join("out.txt").exists(),
            "no file may escape a read-only grant"
        );

        // PW：同一挂载下档位变更 → 实例重建 → 项目根可写。
        *mode.write().expect("mode") = PermissionMode::ProjectWrite;
        registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "pw"}),
                &project_view,
                &cancel,
            )
            .expect("write after mode switch");
        assert_eq!(
            std::fs::read_to_string(project.join("out.txt")).expect("written"),
            "pw"
        );

        // FA：额外目录按 guest 名可写（显式 /<根>/ 寻址）。
        *mode.write().expect("mode") = PermissionMode::FullAccess;
        let output = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({
                    "path": format!("/{extra_guest}/note.txt"),
                    "content": "fa",
                }),
                &project_view,
                &cancel,
            )
            .expect("write into the extra grant");
        assert_eq!(output["bytes"], 2);
        assert_eq!(
            std::fs::read_to_string(extra.join("note.txt")).expect("extra"),
            "fa"
        );

        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(extra);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    // ---- B5：写授予审批化（INV-W1..W6；验收①—④判别测试）。

    /// 组件 fixture 的 sha256（与 compile_plugin 的无条件摘要同一算法）。
    fn fixture_sha256(name: &str) -> String {
        use sha2::Digest as _;
        let bytes = std::fs::read(fixture(name)).expect("fixture bytes");
        sha2::Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// 记录所见审批请求、按固定脚本裁决的 approver（B5 判别测试）。
    struct ScriptedGrantApprover {
        seen: Mutex<Vec<crate::permission::PermissionRequest>>,
        verdict: PermissionDecision,
    }

    impl ScriptedGrantApprover {
        fn with_verdict(verdict: PermissionDecision) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                verdict,
            })
        }

        fn seen_count(&self) -> usize {
            self.seen.lock().expect("seen log").len()
        }
    }

    impl PermissionApprover for ScriptedGrantApprover {
        fn decide(
            &self,
            request: crate::permission::PermissionRequest,
            _cancel: &CancelToken,
        ) -> PermissionDecision {
            self.seen.lock().expect("seen log").push(request);
            self.verdict.clone()
        }
    }

    /// 挂载 read 插件（带档位 cell）；桥由调用方自持——不装上下文
    /// 即无审批面（INV-W6 的 fail-closed 腿）。
    fn mount_read_plugin(
        root: &std::path::Path,
        mcp_root: &std::path::Path,
        project: &std::path::Path,
        mode: crate::permission::PermissionMode,
        bridge: std::sync::Arc<crate::plugin_host::PluginHostBridge>,
    ) -> (
        PluginManager,
        std::sync::Arc<std::sync::RwLock<crate::permission::PermissionMode>>,
    ) {
        write_config(
            root,
            &[("read", fixture("read.wasm").display().to_string())],
        );
        let cell = std::sync::Arc::new(std::sync::RwLock::new(mode));
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.to_owned(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.to_owned(),
                bridge,
                project.to_owned(),
                Some(std::sync::Arc::clone(&cell)),
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        (manager, cell)
    }

    /// 装一个带指定 approver 的桥上下文（等价 start_run 的 install；
    /// 每次 install 分配新纪元）。
    fn install_grant_context(
        bridge: &crate::plugin_host::PluginHostBridge,
        approver: std::sync::Arc<dyn PermissionApprover>,
    ) {
        bridge.install(crate::plugin_host::RunHostContext {
            providers: fake_providers(),
            model_config: ModelConfig {
                model: "fake-model".into(),
                ..Default::default()
            },
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver,
            permission_mode: None,
            asker: None,
            cancel: CancelToken::new(),
            usage_cell: std::sync::Arc::new(Mutex::new(ModelUsage::default())),
            budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
        });
    }

    /// 验收①（INV-W1/W3）：首调弹一次审批（参数列出目录集与组件
    /// 摘要）→ Allow 落记录；新 run（新纪元）同 hash 静默授予。
    /// pre-fix 判别力：无写授予门——0 次审批、无记录文件、写静默成功
    ///（见实施补记的门删除复验）。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn write_grants_ask_once_then_persist_silence_for_the_same_hash() {
        use crate::permission::PermissionMode;
        let root = unique_root("grant-ask");
        let project = unique_root("grant-ask-project");
        let mcp_root = unique_root("grant-ask-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let (mut manager, _cell) = mount_read_plugin(
            &root,
            &mcp_root,
            &project,
            PermissionMode::ProjectWrite,
            std::sync::Arc::clone(&bridge),
        );
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project_view = crate::project::Project::new(&project);
        let cancel = CancelToken::new();
        let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
        install_grant_context(&bridge, approver.clone());

        // 首调：恰好一问，审批面 = 实际授予面（目录集 + 组件摘要）。
        registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "granted"}),
                &project_view,
                &cancel,
            )
            .expect("write after an explicit grant");
        {
            let seen = approver.seen.lock().expect("seen log");
            assert_eq!(seen.len(), 1, "exactly one approval per plugin per run");
            assert_eq!(seen[0].tool, "wasm:read");
            assert_eq!(seen[0].effect, crate::tool::ToolEffect::Write);
            assert_eq!(
                seen[0].arguments["component_sha256"],
                fixture_sha256("read.wasm")
            );
            assert_eq!(
                seen[0].arguments["write_dirs"],
                serde_json::json!([project.display().to_string()])
            );
        }
        assert_eq!(
            std::fs::read_to_string(project.join("out.txt")).expect("written"),
            "granted"
        );

        // 记录落盘（三要素齐全）。
        let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
        assert!(wasm_grants::covers(
            &records,
            "read",
            &fixture_sha256("read.wasm"),
            std::slice::from_ref(&project)
        ));

        // 新 run（新纪元）：有效记录 → 0 问、写照常。
        bridge.clear();
        let silent_approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
        install_grant_context(&bridge, silent_approver.clone());
        registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out2.txt", "content": "silent"}),
                &project_view,
                &cancel,
            )
            .expect("write with a valid record stays silent");
        assert_eq!(
            silent_approver.seen_count(),
            0,
            "a valid record must not re-ask"
        );

        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// 验收②（INV-W1）：组件 hash 失配的记录不覆盖 → 重问。
    /// pre-fix 判别力：无门——0 问且写静默成功。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn write_grants_reask_when_the_component_hash_changes() {
        use crate::permission::PermissionMode;
        let root = unique_root("grant-stale");
        let project = unique_root("grant-stale-project");
        let mcp_root = unique_root("grant-stale-mcp");
        // 预置 hash 不符的记录（目录集是对的也不行——三要素缺一不可）。
        let mut records = Vec::new();
        wasm_grants::upsert(
            &mut records,
            "read",
            &"f".repeat(64),
            std::slice::from_ref(&project),
        );
        wasm_grants::save_grants(&wasm_grants::grants_path(&root), &records)
            .expect("seed stale record");

        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let (mut manager, _cell) = mount_read_plugin(
            &root,
            &mcp_root,
            &project,
            PermissionMode::ProjectWrite,
            std::sync::Arc::clone(&bridge),
        );
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Allow);
        install_grant_context(&bridge, approver.clone());
        registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "re-asked"}),
                &crate::project::Project::new(&project),
                &CancelToken::new(),
            )
            .expect("write after re-asking");
        assert_eq!(
            approver.seen_count(),
            1,
            "a stale-hash record must not silently cover"
        );

        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// 验收③（INV-W2/W3）：Deny → 本 run 物理只读（写工具失败、磁盘
    /// 无文件）、不落记录、同 run 不再问；下一 run（新纪元）重问。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn write_grants_denial_downgrades_to_read_only_for_the_run() {
        use crate::permission::PermissionMode;
        let root = unique_root("grant-deny");
        let project = unique_root("grant-deny-project");
        let mcp_root = unique_root("grant-deny-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let (mut manager, _cell) = mount_read_plugin(
            &root,
            &mcp_root,
            &project,
            PermissionMode::ProjectWrite,
            std::sync::Arc::clone(&bridge),
        );
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project_view = crate::project::Project::new(&project);
        let cancel = CancelToken::new();
        let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Deny {
            reason: "not today".into(),
        });
        install_grant_context(&bridge, approver.clone());

        // 拒绝 → 物理只读：写失败、磁盘无文件。
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "denied"}),
                &project_view,
                &cancel,
            )
            .expect_err("denied write grant must fail physically");
        assert!(
            !project.join("out.txt").exists(),
            "no file may escape a read-only grant"
        );
        assert_eq!(approver.seen_count(), 1);

        // 同 run 二次调用：不重复问，依旧只读。
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "still denied"}),
                &project_view,
                &cancel,
            )
            .expect_err("denial sticks for the run");
        assert_eq!(approver.seen_count(), 1, "no re-ask within the same run");

        // 不落记录。
        let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
        assert!(!wasm_grants::covers(
            &records,
            "read",
            &fixture_sha256("read.wasm"),
            std::slice::from_ref(&project)
        ));

        // 下一 run（新纪元）重问（per-run 拒绝语义）。
        bridge.clear();
        let approver2 = ScriptedGrantApprover::with_verdict(PermissionDecision::Deny {
            reason: "still no".into(),
        });
        install_grant_context(&bridge, approver2.clone());
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "denied again"}),
                &project_view,
                &cancel,
            )
            .expect_err("denied again");
        assert_eq!(approver2.seen_count(), 1, "a new run must re-ask");

        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// 验收④（INV-W2/W6）：无活动 run（headless 无桥上下文）→
    /// fail-closed 只读；Unavailable（exec 非交互先例）同样拒写——
    /// 两腿都不落记录、不 panic。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn write_grants_fail_closed_without_an_interactive_run() {
        use crate::permission::PermissionMode;
        let root = unique_root("grant-headless");
        let project = unique_root("grant-headless-project");
        let mcp_root = unique_root("grant-headless-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let (mut manager, _cell) = mount_read_plugin(
            &root,
            &mcp_root,
            &project,
            PermissionMode::ProjectWrite,
            std::sync::Arc::clone(&bridge),
        );
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project_view = crate::project::Project::new(&project);
        let cancel = CancelToken::new();

        // 腿一：无桥上下文（boot/mount 期、run 间隙）——无审批面即无
        // 写授予。
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "headless"}),
                &project_view,
                &cancel,
            )
            .expect_err("no active run means no write grant");
        assert!(!project.join("out.txt").exists());

        // 腿二：Unavailable（exec NonInteractive 形态）——有审批面但
        // fail-closed。
        let approver = ScriptedGrantApprover::with_verdict(PermissionDecision::Unavailable {
            reason: "non-interactive run denied `wasm:read`".into(),
        });
        install_grant_context(&bridge, approver.clone());
        let _error = registry
            .get("wasm_read_write_file")
            .expect("tool")
            .invoke(
                &serde_json::json!({"path": "out.txt", "content": "unavailable"}),
                &project_view,
                &cancel,
            )
            .expect_err("unavailable approval means no write grant");
        assert!(!project.join("out.txt").exists());
        assert_eq!(approver.seen_count(), 1);

        // 两腿都不落记录。
        let records = wasm_grants::load_grants(&wasm_grants::grants_path(&root));
        assert!(records.is_empty());

        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-K2/K4 门控：greeter 经 SDK DSL 声明，config 从 plugins.json
    /// 流入组件；未配置时报错而非静默空值。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn greeter_reads_its_config_through_the_sdk() {
        fn mount_greeter(
            root: &std::path::Path,
            mcp_root: &std::path::Path,
            config: Option<serde_json::Value>,
        ) -> PluginManager {
            let map: BTreeMap<String, WasmPluginConfig> = BTreeMap::from([(
                "greeter".to_owned(),
                WasmPluginConfig {
                    path: fixture("greeter.wasm").display().to_string(),
                    dirs: Vec::new(),
                    config,
                    sha256: None,
                },
            )]);
            std::fs::write(
                root.join("plugins.json"),
                serde_json::to_vec(&map).expect("serialize"),
            )
            .expect("config");
            let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
                std::sync::Arc::new(ToolRegistryPlugin),
                std::sync::Arc::new(McpAdapterPlugin::new(
                    mcp_root.to_owned(),
                    Vec::new(),
                    crate::plugin_host::PluginHostBridge::shared(),
                )),
                std::sync::Arc::new(WasmAdapterPlugin::new(
                    root.to_owned(),
                    crate::plugin_host::PluginHostBridge::shared(),
                    root.to_owned(),
                    None,
                )),
            ];
            let mut manager = PluginManager::root(ScopeKind::TrustedProject);
            manager.mount_all(catalog).expect("mount");
            manager
        }

        // 配置流入：greeting + upper 生效。
        let root = unique_root("greeter");
        let mcp_root = unique_root("greeter-mcp");
        let mut manager = mount_greeter(
            &root,
            &mcp_root,
            Some(serde_json::json!({ "greeting": "Hola", "upper": true })),
        );
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project = crate::project::Project::new(&root);
        let output = registry
            .get("wasm_greeter_greet")
            .expect("tool")
            .invoke(
                &serde_json::json!({"name": "clat"}),
                &project,
                &CancelToken::new(),
            )
            .expect("greet");
        assert_eq!(output["greeting"], "Hola, CLAT!");
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);

        // 未配置：报错而非静默（INV-K2）。
        let root = unique_root("greeter-nocfg");
        let mcp_root = unique_root("greeter-nocfg-mcp");
        let mut manager = mount_greeter(&root, &mcp_root, None);
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project = crate::project::Project::new(&root);
        let error = registry
            .get("wasm_greeter_greet")
            .expect("tool")
            .invoke(
                &serde_json::json!({"name": "clat"}),
                &project,
                &CancelToken::new(),
            )
            .expect_err("unconfigured plugin must fail loudly");
        assert!(
            error.to_string().contains("no config provided"),
            "unexpected error: {error}"
        );
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-W1/G2 门控（2d 测试化）：网络全地址拒绝、环境变量不可达、
    /// 内存 256MB 上限——三条沙箱声明从"文档声称"升为测试钉住。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn sandbox_claims_are_pinned_by_tests() {
        let root = unique_root("sandbox");
        let mcp_root = unique_root("sandbox-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let mut manager = mount_probe(&root, &mcp_root, bridge, CALL_FUEL);
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project = crate::project::Project::new(&root);
        let cancel = CancelToken::new();

        // 网络：连接必须失败（sockets 可建、全地址拒绝）。
        let output = registry
            .get("wasm_probe_net")
            .expect("tool")
            .invoke(
                &serde_json::json!({"host": "127.0.0.1", "port": 9}),
                &project,
                &cancel,
            )
            .expect("net probe returns data, not a trap");
        assert_eq!(
            output["connected"], false,
            "no address may be reachable from a plugin: {output}"
        );
        assert!(
            output["error"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "the denial must be observable: {output}"
        );

        // 环境变量：可见集必须为空（宿主环境不透传）。
        let output = registry
            .get("wasm_probe_env")
            .expect("tool")
            .invoke(&serde_json::json!({}), &project, &cancel)
            .expect("env probe");
        assert_eq!(
            output["count"], 0,
            "no host environment variable may leak into a plugin: {output}"
        );

        // 内存：撑到 256MB 上限必须 trap 成工具错误（及时返回）。
        let started = std::time::Instant::now();
        let _error = registry
            .get("wasm_probe_alloc")
            .expect("tool")
            .invoke(&serde_json::json!({}), &project, &cancel)
            .expect_err("the memory cap must trap the allocation loop");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "memory-cap trap must be prompt: {:?}",
            started.elapsed()
        );

        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-W3（fuel 修订）的正面对照：等人的时间不烧燃料——小额燃料
    /// 下 elicitation 慢应答（2×1s 睡眠）照常成功，同额燃料跑死循环
    /// 立刻耗尽。epoch 语义（壁钟刻度）在此测试上会红。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn waiting_for_the_user_does_not_burn_fuel() {
        struct SlowAsker {
            asked: Mutex<u32>,
        }
        impl UserAsker for SlowAsker {
            fn ask(&self, _question: AskQuestion, _cancel: &CancelToken) -> AskAnswer {
                std::thread::sleep(Duration::from_secs(1));
                let mut asked = self.asked.lock().expect("asker state");
                *asked += 1;
                // probe 表单两字段：flavor（choice）→ vanilla；servings
                //（number）→ 2。
                if *asked == 1 {
                    AskAnswer::Selected("vanilla".into())
                } else {
                    AskAnswer::Custom("2".into())
                }
            }
        }

        let root = unique_root("fuelwait");
        let mcp_root = unique_root("fuelwait-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        // 小额燃料：够 probe 的胶水逻辑，绝不够秒级死循环。
        let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), 20_000_000);
        let registry = manager.require(TOOL_SERVICE).expect("registry");

        bridge.install(crate::plugin_host::RunHostContext {
            providers: fake_providers(),
            model_config: crate::model::ModelConfig {
                model: "fake-model".into(),
                ..Default::default()
            },
            credentials: crate::model::ProviderCredentials::for_protocol(
                crate::model::ModelProtocol::OpenAiCompatible,
            ),
            approver: std::sync::Arc::new(allow_all_approver)
                as std::sync::Arc<dyn PermissionApprover>,
            permission_mode: None,
            asker: Some(std::sync::Arc::new(SlowAsker {
                asked: Mutex::new(0),
            })),
            cancel: CancelToken::new(),
            usage_cell: std::sync::Arc::new(Mutex::new(ModelUsage::default())),
            budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
        });

        let project = crate::project::Project::new(&root);
        let output = registry
            .get("wasm_probe_probe")
            .expect("tool")
            .invoke(
                &serde_json::json!({"elicit": true}),
                &project,
                &CancelToken::new(),
            )
            .expect("elicitation wait must not consume fuel");
        assert!(
            output["elicit"]["flavor"].is_string() || output["elicit"].is_string(),
            "unexpected output: {output}"
        );

        // 对照：同额燃料，真实执行立刻耗尽。
        let _error = registry
            .get("wasm_probe_spin")
            .expect("tool")
            .invoke(&serde_json::json!({}), &project, &CancelToken::new())
            .expect_err("the same fuel must exhaust on real execution");

        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    fn fixture(name: &str) -> PathBuf {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/wasm")
            .join(name);
        assert!(path.is_file(), "missing fixture: {}", path.display());
        path
    }

    fn write_config(root: &std::path::Path, entries: &[(&str, String)]) {
        write_config_json(
            root,
            &entries
                .iter()
                .map(|(name, path)| {
                    (
                        (*name).to_owned(),
                        serde_json::json!({ "path": path.clone() }),
                    )
                })
                .collect::<Vec<_>>(),
        );
    }

    fn write_config_json(root: &std::path::Path, entries: &[(String, serde_json::Value)]) {
        let map: BTreeMap<String, WasmPluginConfig> = entries
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_owned(),
                    serde_json::from_value(value.clone()).expect("config shape"),
                )
            })
            .collect();
        std::fs::write(
            root.join("plugins.json"),
            serde_json::to_vec(&map).expect("serialize"),
        )
        .expect("write config");
    }

    fn unique_root(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-wasm-{tag}-{unique}"));
        std::fs::create_dir_all(&root).expect("root");
        root
    }

    /// INV-W2/W6：digest 注册为 `wasm_digest_digest`、状态面板呈现
    /// transport `wasm`；teardown 撤销 lease（close 后注册表为空）。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn digest_plugin_registers_invokes_and_revokes() {
        let root = unique_root("digest");
        write_config(
            &root,
            &[("digest", fixture("digest.wasm").display().to_string())],
        );
        // 空 MCP 根：状态服务由 McpAdapter 提供（settled 立即）。
        let mcp_root = unique_root("digest-mcp");

        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.clone(),
                bridge,
                root.clone(),
                None,
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");

        let definitions = registry.definitions();
        assert_eq!(
            definitions
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            ["wasm_digest_digest"],
            "the digest tool must be registered under its qualified name"
        );
        assert_eq!(definitions[0].effect, ToolEffect::Pure);
        assert!(definitions[0].description.contains("[wasm:digest]"));

        let snapshot = status.snapshot();
        assert_eq!(snapshot.configured, 1, "wasm entries extend the panel");
        assert_eq!(snapshot.connected, 1);
        assert_eq!(snapshot.servers[0].transport, "wasm");
        assert_eq!(snapshot.servers[0].tools, 1);
        assert_eq!(snapshot.servers[0].protocol_version, WIT_PROTOCOL);

        // 调用：sha256 / base64 两条真实路径。
        let project = crate::project::Project::new(&root);
        let cancel = CancelToken::new();
        let tool = registry.get("wasm_digest_digest").expect("tool handle");
        let output = tool
            .invoke(
                &serde_json::json!({"op": "sha256", "text": "abc"}),
                &project,
                &cancel,
            )
            .expect("sha256");
        assert_eq!(
            output["sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let output = tool
            .invoke(
                &serde_json::json!({"op": "base64-encode", "text": "hello"}),
                &project,
                &cancel,
            )
            .expect("base64");
        assert_eq!(output["base64"], "aGVsbG8=");
        // 组件内错误按工具错误返回（run 不死）。
        let error = tool
            .invoke(&serde_json::json!({"op": "sha256"}), &project, &cancel)
            .expect_err("missing text must fail");
        assert!(!error.to_string().is_empty());

        manager.close().expect("close project scope");
        assert!(
            registry.is_empty(),
            "wasm tool lease was not revoked on teardown"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-W5：坏组件只隔离自己——digest 照常注册，坏条目进状态面板。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn broken_component_is_isolated() {
        let root = unique_root("isolation");
        let garbage = root.join("garbage.wasm");
        std::fs::write(&garbage, b"definitely not a component").expect("write garbage");
        write_config(
            &root,
            &[
                ("digest", fixture("digest.wasm").display().to_string()),
                ("garbage", garbage.display().to_string()),
            ],
        );
        let mcp_root = unique_root("isolation-mcp");
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.clone(),
                crate::plugin_host::PluginHostBridge::shared(),
                root.clone(),
                None,
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");
        assert_eq!(
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            ["wasm_digest_digest"],
            "the good plugin must survive the broken one"
        );
        let snapshot = status.snapshot();
        assert_eq!(snapshot.configured, 2);
        assert_eq!(snapshot.connected, 1);
        assert!(
            snapshot
                .failures
                .iter()
                .any(|message| message.contains("wasm `garbage`"))
        );
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    struct FakeFactory;

    impl ModelFactory for FakeFactory {
        fn protocol(&self) -> ModelProtocol {
            ModelProtocol::OpenAiCompatible
        }

        fn describe(&self, _credentials: &ProviderCredentials) -> ProviderDescriptor {
            unimplemented!("not needed for wasm plugin tests")
        }

        fn build(
            &self,
            _config: &ModelConfig,
            _credentials: &ProviderCredentials,
        ) -> Result<Box<dyn Model>, ModelError> {
            Ok(Box::new(FakeModel))
        }
    }

    struct FakeModel;

    impl Model for FakeModel {
        fn provider(&self) -> &str {
            "wasm-test-fake"
        }

        fn model_id(&self) -> &str {
            "fake-model"
        }

        fn stream(
            &mut self,
            _request: ModelRequest<'_>,
            _events: &mut dyn ModelEventSink,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                text: "fake".into(),
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Completed,
                usage: Some(ModelUsage {
                    input_tokens: 11,
                    output_tokens: 4,
                    ..ModelUsage::default()
                }),
                provider_response_id: None,
                provider_state: Vec::new(),
                reasoning: None,
            })
        }
    }

    fn fake_providers() -> std::sync::Arc<crate::plugins::services::ProviderRegistry> {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                std::sync::Arc::new(ToolRegistryPlugin),
                std::sync::Arc::new(ProviderRegistryPlugin),
            ])
            .expect("mount");
        let providers = manager.require(PROVIDER_SERVICE).expect("providers");
        providers
            .register(
                PluginOwner::for_test(PluginId::new("test.wasm_plugins")),
                std::sync::Arc::new(FakeFactory),
            )
            .expect("register fake factory");
        providers
    }

    /// 桥的假 asker：按脚本逐字段作答（vanilla / 2）。
    struct ScriptedAsker {
        answers: Mutex<std::collections::VecDeque<AskAnswer>>,
    }

    impl UserAsker for ScriptedAsker {
        fn ask(&self, _question: AskQuestion, _cancel: &CancelToken) -> AskAnswer {
            self.answers
                .lock()
                .expect("asker script")
                .pop_front()
                .expect("scripted answer exhausted")
        }
    }

    fn mount_probe(
        root: &std::path::Path,
        mcp_root: &std::path::Path,
        bridge: std::sync::Arc<crate::plugin_host::PluginHostBridge>,
        fuel: u64,
    ) -> PluginManager {
        write_config(
            root,
            &[("probe", fixture("probe.wasm").display().to_string())],
        );
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.to_owned(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(
                WasmAdapterPlugin::new(root.to_owned(), bridge, root.to_owned(), None)
                    .with_fuel(fuel),
            ),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        manager
    }

    /// INV-W4：probe 经 WIT 调 sampling（过权限门 + 计账）与
    /// elicitation（顺序单问）——与 MCP 路径同一桥。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn probe_round_trips_sampling_and_elicitation_through_the_bridge() {
        let root = unique_root("probe");
        let mcp_root = unique_root("probe-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let usage_cell = std::sync::Arc::new(Mutex::new(ModelUsage::default()));

        let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), CALL_FUEL);
        let registry = manager.require(TOOL_SERVICE).expect("registry");

        // 安装桥上下文（等价 start_run 的 install）：fake 模型 + Allow
        // 审批 + 脚本 asker。
        let providers = fake_providers();
        let config = ModelConfig {
            model: "fake-model".into(),
            ..ModelConfig::default()
        };
        bridge.install(crate::plugin_host::RunHostContext {
            providers,
            model_config: config,
            credentials: ProviderCredentials::for_protocol(ModelProtocol::OpenAiCompatible),
            approver: std::sync::Arc::new(allow_all_approver)
                as std::sync::Arc<dyn PermissionApprover>,
            permission_mode: None,
            asker: Some(std::sync::Arc::new(ScriptedAsker {
                answers: Mutex::new(
                    vec![
                        AskAnswer::Selected("vanilla".into()),
                        AskAnswer::Custom("2".into()),
                    ]
                    .into(),
                ),
            })),
            cancel: CancelToken::new(),
            usage_cell: std::sync::Arc::clone(&usage_cell),
            budget: std::sync::Arc::new(Mutex::new(crate::plugin_host::SamplingBudget::per_run())),
        });

        let project = crate::project::Project::new(&root);
        let output = registry
            .get("wasm_probe_probe")
            .expect("probe tool")
            .invoke(
                &serde_json::json!({"sample": true, "elicit": true, "text": "hi"}),
                &project,
                &CancelToken::new(),
            )
            .expect("probe output");
        assert_eq!(output["sampling"]["model"], "fake-model");
        assert_eq!(output["sampling"]["text"], "fake");
        assert_eq!(output["elicit"]["flavor"], "vanilla");
        // WIT 的 number 是 f64（契约层形状）：整数经往返变 2.0——
        // INV-W4 允许的 wire 层差异，语义等价。
        assert_eq!(output["elicit"]["servings"].as_f64(), Some(2.0));
        manager.close().expect("close");
        bridge.clear();
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-W1（fail-closed 复用）：无 run 上下文时，probe 的 sampling
    /// 拿到 no-active-run 错误（elicitation 直接使工具失败）。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn probe_without_a_run_context_fails_closed() {
        let root = unique_root("norun");
        let mcp_root = unique_root("norun-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let mut manager = mount_probe(&root, &mcp_root, std::sync::Arc::clone(&bridge), CALL_FUEL);
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project = crate::project::Project::new(&root);
        let cancel = CancelToken::new();
        let output = registry
            .get("wasm_probe_probe")
            .expect("probe tool")
            .invoke(&serde_json::json!({"sample": true}), &project, &cancel)
            .expect("sampling error is data, not a trap");
        assert!(
            output["sampling"]["error"]
                .as_str()
                .is_some_and(|message| message.contains("no active run")),
            "unexpected output: {output}"
        );
        let error = registry
            .get("wasm_probe_probe")
            .expect("probe tool")
            .invoke(&serde_json::json!({"elicit": true}), &project, &cancel)
            .expect_err("elicitation without a frontend must fail the tool");
        assert!(
            error.to_string().contains("no active run"),
            "unexpected error: {error}"
        );
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// INV-W3：死循环组件被燃料预算打断成工具错误（小额燃料下秒
    /// 级返回，run 不死）。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn spinning_component_is_interrupted_by_the_fuel_budget() {
        let root = unique_root("spin");
        let mcp_root = unique_root("spin-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        // 小额燃料：死循环很快耗尽 → trap（无壁钟 ticker，2d 起为 fuel）。
        let mut manager = mount_probe(&root, &mcp_root, bridge, 5_000_000);
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let project = crate::project::Project::new(&root);
        let started = std::time::Instant::now();
        let _error = registry
            .get("wasm_probe_spin")
            .expect("spin tool")
            .invoke(&serde_json::json!({}), &project, &CancelToken::new())
            .expect_err("spin must be interrupted");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "interruption must be prompt: {:?}",
            started.elapsed()
        );
        // 具体文案随 wasmtime 版本变化（v48 是 wasm backtrace 形
        // 态）；不变量是"及时被打断成工具错误"，不锁实现文案。

        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// W1-01：取消令牌是执行期能力。全额燃料（纯执行 ≈百秒量级）下，
    /// spin 组件开始执行后触发同一个 `CancelToken`，invoke 必须在秒级
    /// 返回"被中断"的工具错误——而不是等燃料耗尽。pre-fix 红：`_cancel`
    /// 被忽略，本测试要跑满燃料预算（分钟级）且报 fuel trap 文案。
    #[test]
    #[ignore = "loads the wasm fixture; run explicitly with --ignored"]
    fn spinning_component_is_interrupted_by_the_run_cancel_token() {
        let root = unique_root("cancel");
        let mcp_root = unique_root("cancel-mcp");
        let bridge = crate::plugin_host::PluginHostBridge::shared();
        let mut manager = mount_probe(&root, &mcp_root, bridge, CALL_FUEL);
        let registry = manager.require(TOOL_SERVICE).expect("registry");
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let worker_root = root.clone();

        let started = std::time::Instant::now();
        let invoke = std::thread::spawn(move || {
            registry
                .get("wasm_probe_spin")
                .expect("spin tool")
                .invoke(
                    &serde_json::json!({}),
                    &crate::project::Project::new(&worker_root),
                    &worker_cancel,
                )
                .expect_err("a cancelled spin must fail, not return")
                .to_string()
        });
        // 等组件确实进入执行（略宽于挂载 + 实例化），再 Esc。
        std::thread::sleep(Duration::from_millis(500));
        cancel.cancel();
        let message = invoke.join().expect("invoke thread");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancellation must interrupt promptly (fuel alone would burn for ~a minute): {:?}",
            started.elapsed()
        );
        assert!(
            message.contains("interrupted"),
            "the error must attribute the trap to cancellation: {message}"
        );

        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    // ---- W1-10：有界时钟订阅（monotonic-clock 宿主直达） ----

    /// 测试用 PluginState（不挂组件，宿主函数可直接调用）。
    fn clock_state(cancel: &CancelToken) -> PluginState {
        PluginState {
            bridge: crate::plugin_host::PluginHostBridge::shared(),
            source: PluginSource::Wasm("clock-test".into()),
            limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            config: None,
            clock: ClockShared::begin_invoke(cancel),
        }
    }

    /// 经同步 poll 宿主驱动一个订阅的就绪（同步 `wasi:io/poll` 的
    /// Host 直接实现在 ResourceTable 上）。
    /// 返回就绪列表长度（wasi 0.3 语义：poll 返回就绪 pollable 的
    /// 下标列表；单订阅就绪即 `[0]`）。
    fn poll_ready(state: &mut PluginState, pollable: Resource<DynPollable>) -> usize {
        use wasmtime_wasi::p2::bindings::sync::io::poll::Host as _;
        ResourceTable::poll(&mut state.table, vec![pollable])
            .expect("poll")
            .len()
    }

    /// W1-10 判别：`u64::MAX` 时长（wasmtime-wasi 默认实现落
    /// `Deadline::Never` 永久阻塞）的订阅在取消令牌置位后必须秒级
    /// 返回控制——epoch/燃料防线因此恢复可达。若回归到默认实现，
    /// 本测试会挂死在 poll 上（默认 `pending().await` 不可中断），
    /// 即为红。
    #[test]
    fn never_tier_clock_wait_is_interruptible_by_the_cancel_token() {
        let cancel = CancelToken::new();
        let mut state = clock_state(&cancel);
        let pollable =
            monotonic_clock::Host::subscribe_duration(&mut state, u64::MAX).expect("subscribe");
        cancel.cancel();
        let started = Instant::now();
        let ready = poll_ready(&mut state, pollable);
        assert_eq!(ready, 1, "the cancelled sleep reports ready");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel must reach the wait within a bounded interval: {:?}",
            started.elapsed()
        );
    }

    /// 调用级预算耗尽后的新订阅立即就绪（组件回到执行点，忙转由
    /// 燃料收尾）——恶意组件不能用"N 个小睡眠"或单个超长睡眠拖垮
    /// 调用。
    #[test]
    fn clock_wait_budget_exhaustion_makes_new_subscriptions_ready_at_once() {
        let cancel = CancelToken::new();
        let mut state = clock_state(&cancel);
        state
            .clock
            .waited_ns
            .store(u64::MAX, AtomicOrdering::Relaxed);
        let pollable =
            monotonic_clock::Host::subscribe_duration(&mut state, u64::MAX).expect("subscribe");
        let started = Instant::now();
        let ready = poll_ready(&mut state, pollable);
        assert_eq!(ready, 1);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an exhausted budget must not wait: {:?}",
            started.elapsed()
        );
    }

    /// 合法短睡眠的时长语义保持：不提前（切片只中断不缩短——
    /// ready 内部循环补满剩余时长）、不明显拖后。
    #[test]
    fn short_clock_waits_keep_their_duration_semantics() {
        let cancel = CancelToken::new();
        let mut state = clock_state(&cancel);
        // 600ms：跨两个 250ms 切片，验证片间续等。
        let pollable =
            monotonic_clock::Host::subscribe_duration(&mut state, 600_000_000).expect("subscribe");
        let started = Instant::now();
        let ready = poll_ready(&mut state, pollable);
        let elapsed = started.elapsed();
        assert_eq!(ready, 1);
        assert!(
            elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(5),
            "a legal sleep keeps its duration (got {elapsed:?})"
        );
    }

    /// A4-3（W1-20）：组件文件大小闸——超 32MiB 拒载（加载前判定，
    /// 不进编译器）。pre-fix 红：垃圾大文件走到 compile 错误（不同文案）。
    #[test]
    fn oversized_components_are_refused_before_compilation() {
        let root = unique_root("oversize");
        let big = root.join("big.wasm");
        let mut blob = vec![0u8; 33 * 1024 * 1024];
        blob[..9].copy_from_slice(b"garbage!!");
        std::fs::write(&big, &blob).expect("write big garbage");
        write_config(&root, &[("big", big.display().to_string())]);
        let mcp_root = unique_root("oversize-mcp");
        let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
            std::sync::Arc::new(ToolRegistryPlugin),
            std::sync::Arc::new(McpAdapterPlugin::new(
                mcp_root.clone(),
                Vec::new(),
                crate::plugin_host::PluginHostBridge::shared(),
            )),
            std::sync::Arc::new(WasmAdapterPlugin::new(
                root.clone(),
                crate::plugin_host::PluginHostBridge::shared(),
                root.clone(),
                None,
            )),
        ];
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager.mount_all(catalog).expect("mount");
        let status = manager.require(MCP_STATUS_SERVICE).expect("status");
        let snapshot = status.snapshot();
        assert!(
            snapshot
                .failures
                .iter()
                .any(|message| message.contains("32 MiB")),
            "the size cap must refuse the load: {snapshot:?}"
        );
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// A4-3：sha256 钉扎——失配拒载，匹配放行。
    #[test]
    fn sha256_pins_are_verified_at_load() {
        use sha2::Digest as _;
        let root = unique_root("pinning");
        let component = fixture("digest.wasm");
        let digest_hex = {
            let bytes = std::fs::read(&component).expect("fixture");
            let digest = sha2::Sha256::digest(&bytes);
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        // 失配：错误钉扎拒载（好组件也不放行）。
        write_config_json(
            &root,
            &[(
                "digest".to_owned(),
                serde_json::json!({
                    "path": component.display().to_string(),
                    "sha256": "deadbeef".to_owned(),
                }),
            )],
        );
        let mcp_root = unique_root("pinning-mcp");
        let mount_and_status = || {
            let catalog: Vec<std::sync::Arc<dyn PluginTrait>> = vec![
                std::sync::Arc::new(ToolRegistryPlugin),
                std::sync::Arc::new(McpAdapterPlugin::new(
                    mcp_root.clone(),
                    Vec::new(),
                    crate::plugin_host::PluginHostBridge::shared(),
                )),
                std::sync::Arc::new(WasmAdapterPlugin::new(
                    root.clone(),
                    crate::plugin_host::PluginHostBridge::shared(),
                    root.clone(),
                    None,
                )),
            ];
            let mut manager = PluginManager::root(ScopeKind::TrustedProject);
            manager.mount_all(catalog).expect("mount");
            let status = manager.require(MCP_STATUS_SERVICE).expect("status");
            (manager, status)
        };
        let (mut manager, status) = mount_and_status();
        let snapshot = status.snapshot();
        assert!(
            snapshot
                .failures
                .iter()
                .any(|message| message.contains("sha256")),
            "a mismatched pin must refuse the load: {snapshot:?}"
        );
        assert_eq!(snapshot.connected, 0);
        manager.close().expect("close");

        // 匹配：正确钉扎放行。
        write_config_json(
            &root,
            &[(
                "digest".to_owned(),
                serde_json::json!({
                    "path": component.display().to_string(),
                    "sha256": digest_hex,
                }),
            )],
        );
        let (mut manager, status) = mount_and_status();
        let snapshot = status.snapshot();
        assert_eq!(
            snapshot.connected, 1,
            "a matching pin must load the plugin: {snapshot:?}"
        );
        manager.close().expect("close");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(mcp_root);
    }

    /// A4-2（W1-19）：元数据消毒——超长 description 截断并记诊断；
    /// 非法 input_schema 拒注册（不再静默降级为空 schema）。
    #[test]
    fn tool_metadata_is_sanitized_with_diagnostics() {
        use exports::clat::plugin::tools::{Definition, Effect};
        let long_description: String = "x".repeat(5000);
        let definitions = vec![
            Definition {
                name: "ok".into(),
                description: "fine".into(),
                input_schema: r#"{"type":"object"}"#.into(),
                effect: Effect::Pure,
            },
            Definition {
                name: "verbose".into(),
                description: long_description,
                input_schema: r#"{"type":"object"}"#.into(),
                effect: Effect::Pure,
            },
            Definition {
                name: "badschema".into(),
                description: "schema is broken".into(),
                input_schema: "{not json".into(),
                effect: Effect::Pure,
            },
        ];
        let (parsed, diagnostics) = sanitize_definitions("test", definitions);
        assert_eq!(parsed.len(), 2, "only the valid-schema tools register");
        assert!(
            parsed.iter().any(|definition| definition.name == "ok"
                && definition.description.contains("[wasm:test] fine")),
            "the healthy tool passes through: {parsed:?}"
        );
        let verbose = parsed
            .iter()
            .find(|definition| definition.name == "verbose")
            .expect("verbose still registers (truncated, not dropped)");
        assert!(
            verbose.description.chars().count() < 4200,
            "the description is truncated: {}",
            verbose.description.chars().count()
        );
        assert!(verbose.description.contains("[truncated by host]"));
        assert_eq!(
            diagnostics.len(),
            2,
            "one diagnostic per violation: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("verbose") && message.contains("truncated"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains("badschema") && message.contains("not registered"))
        );
    }
}
