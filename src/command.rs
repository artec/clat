//! 人机命令领域域（`core.commands`，docs/todo/commands-core.md）。
//!
//! 斜杠命令的**语义**（命令表、名字/别名、门控、对 Application 门面的
//! 调用）属于核心：这是 DSH `ctx.commands` 的 CLAT 对应物——"Plugins
//! register direct human commands without sending invocations to the
//! model"。前端只做「输入解析 → 查注册表 → 渲染 outcome」。
//!
//! 分层契约：
//! - 处理器经 [`CommandHandler`] 拿到 `&mut TrustedProjectApplication` 与
//!   已解析参数，返回前端中立的 [`CommandOutcome`]；它绝不构造 run/model
//!   请求（INV-C5），持久效果由它调用的门面方法的既有事件词表承载
//!   （INV-C7：不生产 `command/run`·`command/done`）。
//! - [`CommandOutcome`] 是意图+数据的 DTO（同 RunEvent 哲学）：各
//!   `Start*` 变体表达「该命令的延续是一次某类交互」，TUI/exec/将来的
//!   桌面端各自决定怎么呈现。
//! - [`CommandRegistry`] 沿用 ToolRegistry/PromptRegistry 模式：贡献必经
//!   不可伪造的 `PluginOwner`，lease 可在逆序拆解时撤销，mount 后 freeze
//!   挡注册不挡撤销（INV-C3）。

use crate::application::{CompactHandle, McpStatusDto, TrustedProjectApplication};
use crate::permission::PermissionMode;
use crate::session::use_cases::SessionSummary;
use std::fmt;
use std::sync::Arc;

/// 帮助/目录 DTO（INV-C4：`command_catalog()` 是帮助表与未知命令提示的
/// 唯一事实源）。`name` 是主名 token（不带前导 `/`），`aliases` 同格式。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInfo {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
}

/// 命令分发的前端中立结果。变体表达意图与数据，不是渲染指令。
pub enum CommandOutcome {
    /// 纯信息性消息（前端以状态提示呈现）。
    Status(String),
    /// `/help`：携带完整命令目录，帮助表从载荷派生。
    ShowHelp { commands: Vec<CommandInfo> },
    /// `/mcp`：MCP 状态快照。
    ShowMcpStatus(McpStatusDto),
    /// `/model`：延续是模型选择交互（前端开各自的选择器）。
    StartModelSelection,
    /// `/resume`：延续是会话选择交互，携带候选列表。
    StartSessionSelection { sessions: Vec<SessionSummary> },
    /// `/perm`：延续是权限档位选择交互，携带当前档。
    StartPermissionModeSelection { current: PermissionMode },
    /// `/rename`：延续是标题编辑交互，携带预填文本。
    StartTitleEdit { prefill: String },
    /// `/compact`：压缩已启动，携带可取消/可 join 的句柄。
    StartCompaction(CompactHandle),
    /// `/new`：会话已重置，前端清空自己的视图状态。
    SessionReset,
    /// `/quit`：请求退出应用（前端生命周期概念，headless 为无操作）。
    QuitRequested,
}

/// 手写 `Debug`（只打印变体名）：载荷含 `CompactHandle` 等不实现
/// `Debug` 的句柄类型，而测试需要 `assert_eq!`/`unwrap_err` 可格式化。
impl fmt::Debug for CommandOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Status(_) => "Status(..)",
            Self::ShowHelp { .. } => "ShowHelp { .. }",
            Self::ShowMcpStatus(_) => "ShowMcpStatus(..)",
            Self::StartModelSelection => "StartModelSelection",
            Self::StartSessionSelection { .. } => "StartSessionSelection { .. }",
            Self::StartPermissionModeSelection { .. } => "StartPermissionModeSelection { .. }",
            Self::StartTitleEdit { .. } => "StartTitleEdit { .. }",
            Self::StartCompaction(_) => "StartCompaction(..)",
            Self::SessionReset => "SessionReset",
            Self::QuitRequested => "QuitRequested",
        })
    }
}

/// 命令分发错误。`Display` 保持抽取前 TUI 的提示文案，快照尽量零刷新。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandError {
    /// 输入不以 `/` 起头（防御：分发方本应先路由）。
    NotACommand { input: String },
    /// 未知命令（文案对齐 `unknown command: …`）。
    NotFound { input: String },
    /// 无参命令收到了多余参数（INV-C6）。
    TakesNoArguments { name: String },
    /// 命令语义执行失败（消息自带的上下文，如
    /// `compaction unavailable: …`）。
    Failed { message: String },
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotACommand { input } => {
                write!(formatter, "not a command: {input}")
            }
            Self::NotFound { input } => {
                write!(formatter, "unknown command: {input}")
            }
            Self::TakesNoArguments { name } => {
                write!(formatter, "/{name} takes no arguments")
            }
            Self::Failed { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CommandError {}

/// 命令处理器：前端中立，绝不向模型转发。`args` 是命令名之后的原文
/// （已 trim）；是否接受参数由注册表的 `takes_args` 声明统一裁决
/// （INV-C6 在分发点集中执行，处理器无需重复检查）。
pub trait CommandHandler: Send + Sync {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError>;
}

/// 解析命令输入：剥离前导 `/`，切出命令名与剩余参数（trim 后）。
pub(crate) fn parse_command_input(input: &str) -> Result<(String, &str), CommandError> {
    let trimmed = input.trim();
    let body = trimmed
        .strip_prefix('/')
        .ok_or_else(|| CommandError::NotACommand {
            input: input.to_owned(),
        })?;
    let (name, rest) = match body.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (body, ""),
    };
    if name.is_empty() {
        return Err(CommandError::NotFound {
            input: input.to_owned(),
        });
    }
    Ok((name.to_owned(), rest))
}

/// 注册名与 parser 同源的 token 校验（W1-07）：非空、无空白/控制字
/// 符、不带前导 `/`——parser 在首个空白处切分并剥 `/`，违反任何一条
/// 的名字都是"帮助可见、派发不可达"的死条目。
fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

/// 一条命令的注册规格。`names` 首元素是主名（帮助/目录展示用），
/// 其余为别名；`takes_args` 声明命令是否接受参数（v1 内建全部 false）。
pub(crate) struct CommandSpec {
    pub(crate) names: Vec<String>,
    pub(crate) description: String,
    pub(crate) takes_args: bool,
    pub(crate) handler: Arc<dyn CommandHandler>,
}

/// 分发点取回的条目快照（owned，避免借用与 `&mut Application` 冲突）。
pub(crate) struct CommandEntry {
    pub(crate) name: String,
    pub(crate) takes_args: bool,
    pub(crate) handler: Arc<dyn CommandHandler>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandRegistryError {
    Frozen,
    Duplicate {
        name: String,
        existing_owner: crate::plugin::PluginId,
        attempted_owner: crate::plugin::PluginId,
    },
    Invalid,
    Poisoned,
}

impl fmt::Display for CommandRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frozen => formatter.write_str("command registry is frozen"),
            Self::Duplicate {
                name,
                existing_owner,
                attempted_owner,
            } => write!(
                formatter,
                "duplicate command `{name}` (existing owner {existing_owner}, \
                 attempted owner {attempted_owner})"
            ),
            Self::Invalid => formatter.write_str(
                "command names must be non-empty dispatchable tokens: no whitespace or control \
                 characters, no leading `/`, no duplicates within a spec",
            ),
            Self::Poisoned => formatter.write_str("command registry lock poisoned"),
        }
    }
}

impl std::error::Error for CommandRegistryError {}

/// 命令注册表：贡献序即帮助/目录序；freeze 后贡献失败、撤销仍可用。
pub struct CommandRegistry {
    entries: std::sync::RwLock<Vec<(u64, crate::plugin::PluginId, CommandSpec)>>,
    next_contribution: std::sync::atomic::AtomicU64,
    frozen: std::sync::atomic::AtomicBool,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            entries: std::sync::RwLock::new(Vec::new()),
            next_contribution: std::sync::atomic::AtomicU64::new(0),
            frozen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 按名查条目（主名与别名皆可），返回 owned 快照供分发。
    pub(crate) fn lookup(&self, name: &str) -> Option<CommandEntry> {
        self.entries.read().ok().and_then(|entries| {
            entries.iter().find_map(|(_, _, spec)| {
                spec.names.iter().position(|candidate| candidate == name)?;
                Some(CommandEntry {
                    name: spec.names[0].clone(),
                    takes_args: spec.takes_args,
                    handler: Arc::clone(&spec.handler),
                })
            })
        })
    }

    /// 目录折叠（保贡献序）：帮助表与未知命令提示的唯一事实源。
    pub fn catalog(&self) -> Vec<CommandInfo> {
        self.entries
            .read()
            .map(|entries| {
                entries
                    .iter()
                    .map(|(_, _, spec)| CommandInfo {
                        name: spec.names[0].clone(),
                        aliases: spec.names[1..].to_vec(),
                        description: spec.description.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        owner: crate::plugin::PluginOwner,
        spec: CommandSpec,
    ) -> Result<CommandLease, CommandRegistryError> {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Err(CommandRegistryError::Frozen);
        }
        // 名字必须是 parser 可派发的 token（W1-07）：含空白/控制字符或
        // 带前导 `/` 的名字在帮助里可见、却永远 lookup 不到；同一 spec
        // 内的重名此前也漏网（Duplicate 只对比已存在 entries）。
        let mut seen = std::collections::HashSet::new();
        if spec.names.is_empty()
            || spec
                .names
                .iter()
                .any(|name| !valid_command_name(name) || !seen.insert(name.as_str()))
        {
            return Err(CommandRegistryError::Invalid);
        }
        let owner = owner.id();
        let contribution = self
            .next_contribution
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut entries = self
                .entries
                .write()
                .map_err(|_| CommandRegistryError::Poisoned)?;
            for (_, existing_owner, existing) in entries.iter() {
                if let Some(name) = existing.names.iter().find(|name| spec.names.contains(name)) {
                    return Err(CommandRegistryError::Duplicate {
                        name: name.clone(),
                        existing_owner: *existing_owner,
                        attempted_owner: owner,
                    });
                }
            }
            entries.push((contribution, owner, spec));
        }
        Ok(CommandLease {
            registry: Arc::downgrade(self),
            contribution,
        })
    }

    pub(crate) fn freeze(&self) {
        self.frozen
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries
            .read()
            .map(|entries| entries.is_empty())
            .unwrap_or(true)
    }
}

pub(crate) struct CommandLease {
    registry: std::sync::Weak<CommandRegistry>,
    contribution: u64,
}

impl CommandLease {
    pub(crate) fn revoke(self) -> Result<(), CommandRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Ok(());
        };
        registry
            .entries
            .write()
            .map_err(|_| CommandRegistryError::Poisoned)?
            .retain(|(contribution, _, _)| *contribution != self.contribution);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginId;

    struct NoopHandler;

    impl CommandHandler for NoopHandler {
        fn run(
            &self,
            _application: &mut TrustedProjectApplication,
            _args: &str,
        ) -> Result<CommandOutcome, CommandError> {
            Ok(CommandOutcome::Status("noop".into()))
        }
    }

    fn spec(names: &[&str]) -> CommandSpec {
        CommandSpec {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            description: "test".into(),
            takes_args: false,
            handler: Arc::new(NoopHandler),
        }
    }

    fn register(names: &[&str]) -> Result<(), CommandRegistryError> {
        let registry = Arc::new(CommandRegistry::new());
        registry
            .register(
                crate::plugin::PluginOwner::for_test(PluginId::new("test.commands")),
                spec(names),
            )
            .map(|_| ())
    }

    #[test]
    fn valid_names_register_normally() {
        assert!(register(&["help", "h"]).is_ok());
    }

    /// W1-07：parser 永远无法派发的名字必须注册即拒——含空白的名字
    /// 会在帮助里展示 `/foo bar`，但用户输入后 parser 查的是 `foo`。
    #[test]
    fn parser_unreachable_names_are_rejected() {
        for names in [
            vec!["foo bar"],
            vec!["foo\tbar"],
            vec!["/foo"],
            vec!["foo\u{7}"],
            vec!["ok", "bad name"],
            vec![""],
        ] {
            assert_eq!(
                register(&names),
                Err(CommandRegistryError::Invalid),
                "names {names:?} must be rejected at registration"
            );
        }
    }

    /// 同一 spec 内的重复别名（跨 spec 的 Duplicate 检查此前覆盖不到）。
    #[test]
    fn duplicate_aliases_within_one_spec_are_rejected() {
        assert_eq!(
            register(&["foo", "foo"]),
            Err(CommandRegistryError::Invalid)
        );
    }

    /// 跨 spec 重名仍是 Duplicate（带双方 owner），不是 Invalid。
    #[test]
    fn cross_spec_duplicates_still_report_the_owner() {
        let registry = Arc::new(CommandRegistry::new());
        let owner = crate::plugin::PluginOwner::for_test(PluginId::new("test.commands"));
        registry.register(owner, spec(&["foo"])).expect("first");
        assert!(matches!(
            registry.register(owner, spec(&["foo"])),
            Err(CommandRegistryError::Duplicate { .. })
        ));
    }

    /// 名字校验与 parser 同源：被接受的名字一定能被 parse 到。
    #[test]
    fn accepted_names_are_parser_reachable() {
        assert_eq!(
            parse_command_input("/help me"),
            Ok(("help".to_owned(), "me"))
        );
        // 校验拒绝的 `foo bar` 形态：parser 只会切出 `foo`。
        let (name, rest) = parse_command_input("/foo bar").expect("parse");
        assert_eq!((name.as_str(), rest), ("foo", "bar"));
    }
}
