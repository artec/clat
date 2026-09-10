//! WASM 组件插件运行时（插件桥 Phase 2a，docs/todo/wasm-plugin-runtime.md）。
//!
//! WIT world `wit/plugin.wit`（`clat:plugin@0.1.0`）逐字镜像 MCP 叶子
//! 语义：组件导出 `tools`（= tools/list + tools/call），导入
//! sampling/elicitation/config/host——由 `PluginHostBridge` 提供
//! 实现，与 MCP 传输共用同一语义面（INV-W4：一个对外
//! 契约、三种传输）。
//!
//! 沙箱（INV-W1，2026-08-21 修正措辞）：**零授权 WASI**——wasm32-
//! wasip2 组件天然导入 wasi:io/poll 等接口，宿主经 wasmtime-wasi 提
//! 供接口但 WasiCtx 不授予任何能力：无 preopen（文件系统可达面为
//! 空）、无环境变量、stdio 关闭、sockets 无地址授权。组件的授权面
//! 只有 world 声明的 sampling/elicitation/config/host（受 manifest
//! 能力声明和宿主权限门共同限制），以及按权限档位授予的
//! preopen。
//! 有界执行（INV-W3，2d 起为 fuel 计量 + W1-01 取消中断）：燃料只在
//! wasm 实际执行时消耗——host 调用阻塞等人（elicitation/sampling）不
//! 烧预算；每次工具调用重置预算（校准 ≈120s 纯执行），超耗 trap 为
//! 工具错误；内存经 StoreLimits 上限 256MB。取消令牌经 epoch 中断
//! 成为**执行期**能力（W1-01）：调用期间轮询 `CancelToken`，置位即
//! `engine.increment_epoch()`，组件在下一个执行点 trap——Esc 不必等
//! 燃料耗尽。epoch 刻度不经时间流逝推进，"等待不烧预算"不变量保持。

use super::services::{
    MCP_STATUS_SERVICE, MCP_STATUS_SERVICE_ID, McpServerStatus, PROMPT_SERVICE, PROMPT_SERVICE_ID,
    TOOL_SERVICE, TOOL_SERVICE_ID,
};
mod clock;
mod grants;
use crate::mcp::client::qualify_prefixed_tool_name;
use crate::model::CancelToken;
use crate::plugin::{
    Plugin as PluginTrait, PluginCapabilities, PluginContext, PluginDescriptor, PluginError,
    PluginId, PluginPackageManifest, PluginRuntimeKind, ScopeKind, ServiceId,
};
use crate::plugin_host::{
    ElicitField, ElicitFieldKind, ElicitForm, ElicitOutcome, PluginHostBridge, PluginSource,
    SamplingMessage, SamplingRequest, SamplingRole,
};
use crate::project::Project;
use crate::tool::{Tool, ToolDefinition, ToolEffect, ToolError};
use clock::ClockShared;
use grants as wasm_grants;
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
    path: "../../wit",
    world: "plugin",
});

const ID: PluginId = PluginId::new("builtin.wasm_adapter");
const PROVIDES: &[ServiceId] = &[];
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID];
const OPTIONAL: &[ServiceId] = &[MCP_STATUS_SERVICE_ID, PROMPT_SERVICE_ID];
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
    #[serde(default)]
    pub path: String,
    /// 可分发包清单（CLAT plugin manifest v1）。配置该字段时，组件
    /// entry、digest、版本、能力声明和静态 prompt 从清单读取；`path`
    /// 可省略。相对路径仍相对 `~/.clat`。
    #[serde(default)]
    pub manifest: Option<String>,
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

/// Merge installed manifest packages under the user-owned legacy file. The
/// legacy entry is the explicit escape hatch and therefore wins by id.
struct EffectiveWasmConfig {
    entries: WasmPluginMap,
    failures: Vec<String>,
}

fn load_effective_wasm_config(
    root: &std::path::Path,
) -> Result<EffectiveWasmConfig, std::io::Error> {
    let user = load_wasm_config(root)?;
    let excluded = user.keys().cloned().collect();
    let mut effective = BTreeMap::new();
    let installed = crate::plugin::active_packages_for_runtime_excluding(
        root,
        PluginRuntimeKind::WasmComponent,
        &excluded,
    )
    .map_err(std::io::Error::other)?;
    for package in installed.packages {
        effective.insert(
            package.id,
            WasmPluginConfig {
                path: String::new(),
                manifest: Some(package.manifest_path.display().to_string()),
                dirs: Vec::new(),
                config: package.config,
                sha256: None,
            },
        );
    }
    effective.extend(user);
    Ok(EffectiveWasmConfig {
        entries: effective,
        failures: installed.failures,
    })
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
    /// Manifest capability ceiling. `None` preserves the pre-manifest legacy
    /// configuration; a package manifest is deny-by-default for every import.
    capabilities: Option<PluginCapabilities>,
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

impl PluginState {
    fn require_declared(&self, capability: &str, declared: bool) -> Result<(), String> {
        if self.capabilities.is_some() && !declared {
            return Err(format!(
                "plugin manifest does not declare capability `{capability}`"
            ));
        }
        Ok(())
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
        self.require_declared(
            "sampling",
            self.capabilities
                .as_ref()
                .is_none_or(|capabilities| capabilities.sampling),
        )?;
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
        self.require_declared(
            "elicitation",
            self.capabilities
                .as_ref()
                .is_none_or(|capabilities| capabilities.elicitation),
        )?;
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

impl clat::plugin::host::Host for PluginState {
    fn context(&mut self) -> Result<String, String> {
        self.require_declared(
            "hostContext",
            self.capabilities
                .as_ref()
                .is_none_or(|capabilities| capabilities.host_context),
        )?;
        self.bridge
            .host_context()
            .and_then(|context| {
                serde_json::to_string(&context).map_err(|error| {
                    crate::plugin_host::PluginHostError::HostTool(error.to_string())
                })
            })
            .map_err(|error| error.to_string())
    }

    fn call_tool(&mut self, name: String, arguments: String) -> Result<String, String> {
        let declared = self
            .capabilities
            .as_ref()
            .is_none_or(|capabilities| capabilities.host_tools.iter().any(|tool| tool == &name));
        self.require_declared(&format!("hostTools.{name}"), declared)?;
        let arguments: Value = serde_json::from_str(&arguments)
            .map_err(|error| format!("invalid host tool arguments: {error}"))?;
        self.bridge
            .call_host_tool(self.source.clone(), &name, arguments)
            .and_then(|output| {
                serde_json::to_string(&output).map_err(|error| {
                    crate::plugin_host::PluginHostError::HostTool(error.to_string())
                })
            })
            .map_err(|error| error.to_string())
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
    /// Manifest capability ceiling copied into every rebuilt store.
    capabilities: Option<PluginCapabilities>,
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
            capabilities: instance.capabilities.clone(),
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
        let prompt_registry = context
            .try_require(PROMPT_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let effective = load_effective_wasm_config(&self.storage_root)
            .map_err(|error| PluginError::new(error.to_string()))?;
        // INV-W6：与 MCP 同一状态面板；configured 分母先扩（wasm 同步
        // 挂载，随后逐插件落定）。
        if let Some(status) = &status {
            status.extend_configured(effective.entries.len() + effective.failures.len());
            for failure in &effective.failures {
                status.record_failed_server(failure.clone());
            }
        }
        let config = effective.entries;
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
        let mut prompt_leases = Vec::new();
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
                    if let Some(prompt_registry) = &prompt_registry {
                        for prompt in &compiled.prompts {
                            if prompt.system.trim().is_empty() {
                                continue;
                            }
                            match prompt_registry.contribute(owner, prompt.system.clone()) {
                                Ok(lease) => prompt_leases.push(lease),
                                Err(error) => record_failure(format!(
                                    "wasm `{name}` prompt `{}`: {error}",
                                    prompt.name
                                )),
                            }
                        }
                    } else if !compiled.prompts.is_empty() {
                        record_failure(format!(
                            "wasm `{name}` manifest declares prompts but PromptRegistry is unavailable"
                        ));
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
                        capabilities: compiled.capabilities,
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
                            server_version: compiled
                                .package_version
                                .clone()
                                .unwrap_or_else(|| WASMTIME_VERSION.to_owned()),
                            protocol_version: compiled
                                .package_version
                                .as_ref()
                                .map(|_| "clat-plugin-manifest/1".to_owned())
                                .unwrap_or_else(|| WIT_PROTOCOL.to_owned()),
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
            for lease in prompt_leases.drain(..) {
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
    /// Manifest capability ceiling; None is the legacy path-only mode.
    capabilities: Option<PluginCapabilities>,
    /// B5：组件 sha256（小写 hex）——写授予记录三要素之一，无条件
    /// 计算（A4-3 钉扎校验复用同一摘要，不再只配 pin 才算）。
    digest: String,
    /// Manifest-owned static system-prompt contributions.
    prompts: Vec<crate::plugin::ManifestPrompt>,
    /// Package version for status/market identity; None is the legacy
    /// path-only configuration.
    package_version: Option<String>,
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
    let ResolvedPackage {
        path,
        sha256,
        prompts,
        version,
        manifest_declares_tools,
        capabilities,
    } = resolve_package(storage_root, name, plugin_config)?;
    if !path.is_file() {
        return Err(format!(
            "component file not found: {} (plugin `{name}`)",
            path.display(),
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
    if let Some(expected) = &sha256
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
            capabilities: capabilities.clone(),
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
    if manifest_declares_tools == Some(false) && !parsed.is_empty() {
        return Err(
            "component exports tools but its manifest declares capabilities.tools=false".into(),
        );
    }
    Ok(CompiledPlugin {
        component,
        definitions: parsed,
        extra_dirs,
        diagnostics,
        config: plugin_config
            .config
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok()),
        capabilities,
        digest: digest_hex,
        prompts,
        package_version: version,
    })
}

struct ResolvedPackage {
    path: PathBuf,
    sha256: Option<String>,
    prompts: Vec<crate::plugin::ManifestPrompt>,
    version: Option<String>,
    manifest_declares_tools: Option<bool>,
    capabilities: Option<PluginCapabilities>,
}

/// Resolve legacy `path` entries and v1 package manifests into the same
/// compile input. A manifest is authoritative: path/digest cannot have a
/// second conflicting source in plugins.json.
fn resolve_package(
    storage_root: &std::path::Path,
    name: &str,
    plugin_config: &WasmPluginConfig,
) -> Result<ResolvedPackage, String> {
    let Some(raw_manifest) = plugin_config.manifest.as_deref() else {
        if plugin_config.path.trim().is_empty() {
            return Err("plugin entry requires `path` or `manifest`".into());
        }
        return Ok(ResolvedPackage {
            path: resolve_component_path(storage_root, &plugin_config.path),
            sha256: plugin_config.sha256.clone(),
            prompts: Vec::new(),
            version: None,
            manifest_declares_tools: None,
            capabilities: None,
        });
    };
    if !plugin_config.path.trim().is_empty() {
        return Err("manifest-backed plugin must not also set `path`".into());
    }
    let manifest_path = resolve_component_path(storage_root, raw_manifest);
    let manifest = PluginPackageManifest::load(&manifest_path)?;
    if manifest.id != name {
        return Err(format!(
            "manifest id `{}` does not match plugins.json key `{name}`",
            manifest.id
        ));
    }
    if manifest.runtime.kind != PluginRuntimeKind::WasmComponent {
        return Err(format!(
            "manifest `{name}` runtime is not `wasm-component` (MCP packages belong in the MCP installer/config path)"
        ));
    }
    if !manifest.runtime.args.is_empty() {
        return Err("wasm-component runtime does not accept runtime.args".into());
    }
    manifest.validate_config(plugin_config.config.as_ref())?;
    if let Some(config_pin) = &plugin_config.sha256
        && !config_pin
            .trim()
            .trim_start_matches("sha256:")
            .eq_ignore_ascii_case(manifest.runtime.sha256.trim().trim_start_matches("sha256:"))
    {
        return Err("plugins.json sha256 conflicts with manifest runtime.sha256".into());
    }
    let path = manifest.entry_path(&manifest_path)?;
    Ok(ResolvedPackage {
        path,
        sha256: Some(manifest.runtime.sha256.clone()),
        prompts: manifest.prompts.clone(),
        version: Some(manifest.version.clone()),
        manifest_declares_tools: Some(manifest.capabilities.tools),
        capabilities: Some(manifest.capabilities.clone()),
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
mod tests;
