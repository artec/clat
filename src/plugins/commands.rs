//! `core.commands` 的挂载与内建命令贡献（docs/todo/commands-core.md）。
//!
//! `CommandsPlugin` 挂注册表；`BuiltinCommandsPlugin` 贡献十条内建命令，
//! 处理器体从 TUI 分发器各臂平移（语义不变，改调门面、返回 outcome）。
//! 注册序即帮助/目录序（与抽取前的帮助表一致，帮助表改由目录派生）。

use super::services::{COMMAND_SERVICE, COMMAND_SERVICE_ID};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandRegistry, CommandSpec};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use std::sync::Arc;

const REGISTRY_ID: PluginId = PluginId::new("builtin.command_registry");
const BUILTIN_ID: PluginId = PluginId::new("builtin.commands");
const PROVIDES: &[ServiceId] = &[COMMAND_SERVICE_ID];
const REQUIRES: &[ServiceId] = &[COMMAND_SERVICE_ID];
const REGISTRY_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: REGISTRY_ID,
    scope: ScopeKind::TrustedProject,
    provides: PROVIDES,
    requires: &[],
    optional: &[],
};
const BUILTIN_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: BUILTIN_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        context
            .provide(COMMAND_SERVICE, Arc::new(CommandRegistry::new()))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

/// 内建命令的处理器形态：无状态函数指针（INV-C6 的参数裁决在分发点
/// 按 `takes_args` 集中执行，处理器不再重复检查）。
struct Builtin {
    run: fn(&mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError>,
}

impl CommandHandler for Builtin {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        _args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        (self.run)(application)
    }
}

pub(crate) struct BuiltinCommandsPlugin;

impl Plugin for BuiltinCommandsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &BUILTIN_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let registry = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        for spec in builtin_specs() {
            let lease = registry
                .register(context.owner(), spec)
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
        }
        Ok(())
    }
}

fn spec(
    group: crate::command::CommandGroup,
    order: u16,
    names: &[&str],
    description: &str,
    run: fn(&mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError>,
) -> CommandSpec {
    CommandSpec {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        description: description.to_owned(),
        takes_args: false,
        group,
        order,
        handler: Arc::new(Builtin { run }),
    }
}

/// 出厂命令的（组, 表内序）按权威顺序表落位
/// （docs/todo/skills-and-command-order.md，SC 组 A1 裁定 2026-09-02）。
/// 展示序由 `catalog()` 折叠，与这里的声明序无关；描述串沿用原文。
fn builtin_specs() -> Vec<CommandSpec> {
    use crate::command::CommandGroup::{Context, Conversation, Extensions, Meta, Model, Safety};
    vec![
        spec(
            Conversation,
            1,
            &["new", "clear"],
            "start a new conversation",
            run_new,
        ),
        spec(
            Conversation,
            2,
            &["resume"],
            "pick a previous conversation to continue",
            run_resume,
        ),
        spec(
            Conversation,
            3,
            &["rename"],
            "rename the current conversation",
            run_rename,
        ),
        spec(
            Context,
            4,
            &["compact"],
            "summarize earlier turns into a compact context",
            run_compact,
        ),
        spec(
            Model,
            6,
            &["model"],
            "configure the active model/provider",
            run_model,
        ),
        spec(
            Safety,
            7,
            &["perm", "permission"],
            "switch the permission mode (Read Only / Project Write / Full Access)",
            run_perm,
        ),
        spec(
            Extensions,
            9,
            &["mcp"],
            "inspect MCP servers, tools, and failures",
            run_mcp,
        ),
        spec(Meta, 14, &["help"], "this help", run_help),
        spec(Meta, 15, &["quit", "exit"], "exit", run_quit),
        // VP-1（2026-09-03）：custom 一次性视觉探针——Experiments 组、
        // /sub（order 13）之后。探测与覆盖位门控都在
        // application::vision_probe；这里只声明延续意图。
        spec(
            crate::command::CommandGroup::Experiments,
            16,
            &["vision-probe"],
            "verify whether the current custom model reads images (one-shot probe)",
            run_vision_probe,
        ),
    ]
}

fn run_vision_probe(
    application: &mut TrustedProjectApplication,
) -> Result<CommandOutcome, CommandError> {
    // 异步：立即返回 handle，判定经 VisionProbeNotice 事件回流；
    // headless 调用方用 join_report 等结果。
    application
        .start_vision_probe()
        .map(CommandOutcome::StartVisionProbe)
        .map_err(|error| CommandError::Failed {
            message: format!("vision probe unavailable: {error}"),
        })
}

fn run_model(_application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 选择器的数据（当前配置/厂商表）由前端自持镜像渲染，命令只声明
    // 延续意图。
    Ok(CommandOutcome::StartModelSelection)
}

fn run_new(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 纯内存切换：session_id 置 None，首条内容写入时才落盘建会话。
    // 活动 Run/压缩期间拒绝（INV-T3）。视图清空是前端的职责。
    application
        .new_session()
        .map(|_| CommandOutcome::SessionReset)
        .map_err(|error| CommandError::Failed {
            message: error.to_string(),
        })
}

fn run_compact(
    application: &mut TrustedProjectApplication,
) -> Result<CommandOutcome, CommandError> {
    // 异步：立即返回 handle，状态经 CompactionUpdated 事件回流；headless
    // 调用方用 join_report 等结果。
    application
        .compact_session()
        .map(CommandOutcome::StartCompaction)
        .map_err(|error| CommandError::Failed {
            message: format!("compaction unavailable: {error}"),
        })
}

fn run_resume(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    application
        .list_sessions()
        .map(|sessions| CommandOutcome::StartSessionSelection { sessions })
        .map_err(|error| CommandError::Failed {
            message: format!("failed to list conversations: {error}"),
        })
}

fn run_mcp(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 数据来自挂载期的 McpStatus DTO；前端不接触会话/注册表本体，
    // 弹窗内刷新走各自的 refresh 路径。
    Ok(CommandOutcome::ShowMcpStatus(application.mcp_status()))
}

fn run_perm(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 冷切换/降级入口（权限三档）。运行中切档对下一次权限检查生效（P3）。
    Ok(CommandOutcome::StartPermissionModeSelection {
        current: application.permission_mode(),
    })
}

fn run_rename(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 门槛（2026-08-19 放宽）：有活动会话即可改，不再要求 LLM 已起名
    // ——CAS 本就保证改名压制迟到的自动命名。空会话报错。
    match application.current_session_id() {
        Some(_) => Ok(CommandOutcome::StartTitleEdit {
            prefill: application.session_title().unwrap_or_default(),
        }),
        None => Err(CommandError::Failed {
            message: "no active conversation to rename".to_owned(),
        }),
    }
}

fn run_help(application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // INV-C4：帮助载荷与 command_catalog() 同源，新增命令自动进帮助。
    Ok(CommandOutcome::ShowHelp {
        commands: application.command_catalog(),
    })
}

fn run_quit(_application: &mut TrustedProjectApplication) -> Result<CommandOutcome, CommandError> {
    // 应用生命周期是前端概念；headless 下为无操作。
    Ok(CommandOutcome::QuitRequested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandError, CommandRegistryError};
    use crate::plugin::{PluginContext, PluginDescriptor, PluginId, PluginManager, PluginOwner};
    use crate::test_support::{
        SharedEvents, TestBehavior, TestProviderPlugin, configure_test_model, roots,
    };
    use crate::{
        ApplicationRunRequest, BootstrapApplication, PermissionDecision, PermissionRequest, Project,
    };
    use std::sync::{Arc, Mutex};

    fn mount(name: &str) -> (TrustedProjectApplication, std::path::PathBuf) {
        let (storage_root, project_root) = roots(name);
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        (application, storage_root)
    }

    fn cleanup(storage_root: &std::path::Path) {
        std::fs::remove_dir_all(storage_root.parent().unwrap()).ok();
    }

    fn run_once(application: &mut TrustedProjectApplication, prompt: &str) {
        let (completion, receiver) = std::sync::mpsc::channel();
        let handle = application
            .start_run(ApplicationRunRequest {
                message: crate::message::PendingMessage::text(prompt),
                asker: None,
                approver: Arc::new(
                    |_request: PermissionRequest, _cancel: &crate::model::CancelToken| {
                        PermissionDecision::Allow
                    },
                ),
                events: Box::new(SharedEvents(Arc::new(Mutex::new(Vec::new())))),
                completion,
            })
            .unwrap();
        handle.join().unwrap();
        let _ = receiver.recv().unwrap();
    }

    fn run_demo(
        _application: &mut TrustedProjectApplication,
    ) -> Result<CommandOutcome, CommandError> {
        Ok(CommandOutcome::Status("demo".into()))
    }

    /// INV-C2：每条内建命令经真实 Application dispatch 的 outcome 断言
    ///（调用方无关——测试与 TUI/exec 走同一路径）。别名等价
    ///（/clear≡/new、/permission≡/perm、/exit≡/quit）。
    #[test]
    fn dispatch_builtin_outcomes() {
        let (mut application, storage_root) = mount("commands-outcomes");

        assert!(matches!(
            application.dispatch_command("/model"),
            Ok(CommandOutcome::StartModelSelection)
        ));

        // INV-C4：/help 载荷与 command_catalog() 一致。
        let catalog = application.command_catalog();
        match application.dispatch_command("/help") {
            Ok(CommandOutcome::ShowHelp { commands }) => assert_eq!(commands, catalog),
            other => panic!("expected ShowHelp, got {other:?}"),
        }
        let summarize = |info: &crate::command::CommandInfo| {
            let mut names = vec![info.name.clone()];
            names.extend(info.aliases.iter().cloned());
            names
        };
        let catalog_names: Vec<Vec<String>> = catalog.iter().map(summarize).collect();
        for expected in [
            vec!["model"],
            vec!["new", "clear"],
            vec!["compact"],
            vec!["resume"],
            vec!["context"],
            vec!["mcp"],
            vec!["perm", "permission"],
            vec!["plan"],
            vec!["skill", "skills"],
            vec!["mem", "memory"],
            vec!["goal"],
            vec!["sub", "subagents"],
            vec!["rename"],
            vec!["help"],
            vec!["quit", "exit"],
        ] {
            let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
            assert!(
                catalog_names.contains(&expected),
                "catalog missing {expected:?}: {catalog_names:?}"
            );
        }

        assert!(matches!(
            application.dispatch_command("/mcp"),
            Ok(CommandOutcome::ShowMcpStatus(_))
        ));

        // fresh app 无会话：/resume 空列表、/perm 默认档。
        match application.dispatch_command("/resume") {
            Ok(CommandOutcome::StartSessionSelection { sessions }) => {
                assert!(sessions.is_empty())
            }
            other => panic!("expected StartSessionSelection, got {other:?}"),
        }
        match application.dispatch_command("/permission") {
            Ok(CommandOutcome::StartPermissionModeSelection { current }) => {
                assert_eq!(current, crate::permission::PermissionMode::default())
            }
            other => panic!("expected StartPermissionModeSelection, got {other:?}"),
        }

        assert!(matches!(
            application.dispatch_command("/new"),
            Ok(CommandOutcome::SessionReset)
        ));
        assert!(matches!(
            application.dispatch_command("/clear"),
            Ok(CommandOutcome::SessionReset)
        ));
        assert!(matches!(
            application.dispatch_command("/quit"),
            Ok(CommandOutcome::QuitRequested)
        ));
        assert!(matches!(
            application.dispatch_command("/exit"),
            Ok(CommandOutcome::QuitRequested)
        ));

        application.close().unwrap();
        cleanup(&storage_root);
    }

    /// SC-3 判别：真机 catalog 逐行等于权威顺序表（A1 裁定，2026-09-02）
    /// 的十五行——删 `catalog()` 折叠（或抹掉某条 spec 的分组键落位）即
    /// 红。挂载序与展示序解耦（INV-SC-1）靠此测试钉住。
    #[test]
    fn command_catalog_matches_the_authoritative_table() {
        let (application, storage_root) = mount("commands-authoritative-order");
        let catalog = application.command_catalog();
        let rows: Vec<String> = catalog
            .iter()
            .map(|info| {
                let mut names = format!("/{}", info.name);
                for alias in &info.aliases {
                    names.push_str(&format!(", /{alias}"));
                }
                names
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                "/new, /clear",
                "/resume",
                "/rename",
                "/compact",
                "/context",
                "/model",
                "/perm, /permission",
                "/plan",
                "/mcp",
                "/skill, /skills",
                "/mem, /memory",
                "/goal",
                "/sub, /subagents",
                // VP-1（2026-09-03）：custom 一次性视觉探针落 Experiments 组。
                "/vision-probe",
                "/help",
                "/quit, /exit",
            ],
            "the catalog must fold to the authoritative sixteen-row table"
        );
        application.close().unwrap();
        cleanup(&storage_root);
    }

    /// INV-SC-2（A2/A3 判别）：主名调整后，`/memory`、`/subagents` 旧输入
    /// 仍可派发到同一处理器——用户肌肉记忆与既有脚本不破坏。
    #[test]
    fn renamed_primary_names_keep_old_inputs_dispatchable() {
        let (mut application, storage_root) = mount("commands-alias-reachability");
        for input in ["/mem", "/memory"] {
            match application.dispatch_command(input) {
                Ok(CommandOutcome::Status(message)) => assert!(
                    message.contains("memor"),
                    "{input} must reach the memory handler: {message}"
                ),
                other => panic!("{input} must be a Status outcome, got {other:?}"),
            }
        }
        for input in ["/sub", "/subagents"] {
            match application.dispatch_command(input) {
                Ok(CommandOutcome::Status(message)) => assert!(
                    message.contains("subagent experiment"),
                    "{input} must reach the subagent handler: {message}"
                ),
                other => panic!("{input} must be a Status outcome, got {other:?}"),
            }
        }
        application.close().unwrap();
        cleanup(&storage_root);
    }

    /// INV-C6：未知命令、多余参数、非命令输入的结构化错误与文案
    ///（文案保持抽取前 TUI 的提示）。
    #[test]
    fn dispatch_rejects_unknown_and_arguments() {
        let (mut application, storage_root) = mount("commands-errors");
        let error = application.dispatch_command("/nope").unwrap_err();
        assert_eq!(
            error,
            CommandError::NotFound {
                input: "/nope".into()
            }
        );
        assert_eq!(error.to_string(), "unknown command: /nope");
        let error = application.dispatch_command("/model extra").unwrap_err();
        assert_eq!(
            error,
            CommandError::TakesNoArguments {
                name: "model".into()
            }
        );
        assert_eq!(error.to_string(), "/model takes no arguments");
        assert!(matches!(
            application.dispatch_command("hello"),
            Err(CommandError::NotACommand { .. })
        ));
        application.close().unwrap();
        cleanup(&storage_root);
    }

    /// /rename 门控：空会话报错；一轮真实对话后开标题编辑（预填来自
    /// 标题投影）。
    #[test]
    fn dispatch_rename_gates_on_active_session() {
        let (mut application, storage_root) = mount("commands-rename");
        assert_eq!(
            application.dispatch_command("/rename").unwrap_err(),
            CommandError::Failed {
                message: "no active conversation to rename".into()
            }
        );
        configure_test_model(&application);
        run_once(&mut application, "hello there");
        assert!(matches!(
            application.dispatch_command("/rename"),
            Ok(CommandOutcome::StartTitleEdit { .. })
        ));
        application.close().unwrap();
        cleanup(&storage_root);
    }

    /// INV-C7 的可观测面：命令 dispatch 本身不物化任何会话日志
    ///（/new 懒物化；全部命令走完列表仍为空），已有会话上 dispatch
    /// 也不追加任何事件（含 command/\*）。
    #[test]
    fn dispatch_journals_nothing_by_itself() {
        let (mut application, storage_root) = mount("commands-journal-neutral");
        for command in [
            "/model",
            "/help",
            "/mcp",
            "/resume",
            "/perm",
            "/new",
            "/clear",
            "/rename",
            "/quit",
            "/skill",
            "/skill grill-me",
        ] {
            let _ = application.dispatch_command(command);
        }
        assert!(application.list_sessions().unwrap().is_empty());
        // 更强的一侧：一轮真实对话后，dispatch 全部命令不改变 journal。
        configure_test_model(&application);
        run_once(&mut application, "hello there");
        let before = journal_events(&storage_root);
        for command in [
            "/model",
            "/help",
            "/mcp",
            "/resume",
            "/perm",
            "/new",
            "/clear",
            "/rename",
            "/quit",
            "/skill",
            "/skill grill-me",
        ] {
            let _ = application.dispatch_command(command);
        }
        application.close().unwrap();
        let after = journal_events(&storage_root);
        assert_eq!(
            before, after,
            "dispatching commands must not append journal events"
        );
        cleanup(&storage_root);
    }

    /// 读取存储根唯一会话的全部持久事件（对照 application 测试的
    /// load_events）。
    fn journal_events(storage_root: &std::path::Path) -> Vec<String> {
        let backend = crate::session::persistence::JsonlBackend::new(
            storage_root.join("sessions"),
            crate::session::persistence::JsonlCompression::Zstd,
            false,
        );
        let headers = backend.list_headers().unwrap();
        let header = headers.first().expect("one session header").clone();
        let key = crate::session::key::SessionKey {
            project: crate::session::key::ProjectKey::from_cwd(
                &header.cwd.clone().expect("header carries the project cwd"),
            ),
            id: header.id.clone(),
        };
        backend
            .load(&key, false)
            .unwrap()
            .events
            .into_iter()
            .map(|event| event.event_type)
            .collect()
    }

    /// INV-C3：重名（主名与别名）、空名、freeze 后注册失败；冻结不挡
    /// 撤销。
    #[test]
    fn registry_discipline() {
        let registry = Arc::new(CommandRegistry::new());
        let owner = PluginOwner::for_test(PluginId::new("test.command_discipline"));
        let lease = registry
            .register(
                owner,
                spec(
                    crate::command::CommandGroup::Meta,
                    crate::command::COMMAND_ORDER_APPEND,
                    &["demo"],
                    "demo command",
                    run_demo,
                ),
            )
            .unwrap();
        assert!(matches!(
            registry.register(
                owner,
                spec(
                    crate::command::CommandGroup::Meta,
                    crate::command::COMMAND_ORDER_APPEND,
                    &["demo"],
                    "duplicate",
                    run_demo
                )
            ),
            Err(CommandRegistryError::Duplicate { .. })
        ));
        assert!(matches!(
            registry.register(
                owner,
                spec(
                    crate::command::CommandGroup::Meta,
                    crate::command::COMMAND_ORDER_APPEND,
                    &["other", "demo"],
                    "alias duplicate",
                    run_demo
                )
            ),
            Err(CommandRegistryError::Duplicate { .. })
        ));
        assert!(matches!(
            registry.register(
                owner,
                spec(
                    crate::command::CommandGroup::Meta,
                    crate::command::COMMAND_ORDER_APPEND,
                    &[],
                    "no names",
                    run_demo
                )
            ),
            Err(CommandRegistryError::Invalid)
        ));
        assert!(registry.lookup("demo").is_some());
        assert!(!registry.lookup("demo").unwrap().takes_args);
        registry.freeze();
        assert!(matches!(
            registry.register(
                owner,
                spec(
                    crate::command::CommandGroup::Meta,
                    crate::command::COMMAND_ORDER_APPEND,
                    &["late"],
                    "late",
                    run_demo
                )
            ),
            Err(CommandRegistryError::Frozen)
        ));
        lease.revoke().unwrap();
        assert!(registry.lookup("demo").is_none());
        assert!(registry.is_empty());
        assert!(registry.catalog().is_empty());
    }

    /// 扩展插件经 catalog 贡献自定义命令：dispatch 面（lookup/catalog）
    /// 端到端可用，逆序拆解撤销（镜像 plugins/tests.rs 的扩展姿势——
    /// 验收：新增命令不改任何前端代码）。
    struct ExtraCommandPlugin;
    const EXTRA_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
        id: PluginId::new("test.extra_command"),
        scope: crate::plugin::ScopeKind::TrustedProject,
        provides: &[],
        requires: &[COMMAND_SERVICE_ID],
        optional: &[],
    };
    impl Plugin for ExtraCommandPlugin {
        fn descriptor(&self) -> &'static PluginDescriptor {
            &EXTRA_DESCRIPTOR
        }
        fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
            let registry = context
                .require(COMMAND_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?;
            let lease = registry
                .register(
                    context.owner(),
                    spec(
                        crate::command::CommandGroup::Meta,
                        crate::command::COMMAND_ORDER_APPEND,
                        &["extra"],
                        "extra",
                        run_demo,
                    ),
                )
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
            Ok(())
        }
    }

    #[test]
    fn extension_plugin_command_is_dispatchable_and_revoked() {
        let mut manager = PluginManager::root(crate::plugin::ScopeKind::TrustedProject);
        manager
            .mount_all(vec![Arc::new(CommandsPlugin), Arc::new(ExtraCommandPlugin)])
            .unwrap();
        let registry = manager.require(COMMAND_SERVICE).unwrap();
        assert!(registry.lookup("extra").is_some());
        assert!(registry.catalog().iter().any(|info| info.name == "extra"));
        manager.close().unwrap();
        assert!(registry.lookup("extra").is_none());
    }

    /// A4-4（W1-28）：扩展命令 handler 的 panic 不得带崩调用线程/毒化
    /// 核心锁——dispatch 的 catch_unwind 把它降为 `CommandError::Failed`。
    /// pre-fix 红：panic 穿透 dispatch（外层 catch_unwind 收到 payload）。
    struct PanickingCommandPlugin;
    const PANIC_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
        id: PluginId::new("test.panicking_command"),
        scope: crate::plugin::ScopeKind::TrustedProject,
        provides: &[],
        requires: &[COMMAND_SERVICE_ID],
        optional: &[],
    };
    fn run_boom(
        _application: &mut TrustedProjectApplication,
    ) -> Result<CommandOutcome, CommandError> {
        panic!("boom handler");
    }
    impl Plugin for PanickingCommandPlugin {
        fn descriptor(&self) -> &'static PluginDescriptor {
            &PANIC_DESCRIPTOR
        }
        fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
            let registry = context
                .require(COMMAND_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?;
            let lease = registry
                .register(
                    context.owner(),
                    spec(
                        crate::command::CommandGroup::Meta,
                        crate::command::COMMAND_ORDER_APPEND,
                        &["boom"],
                        "boom",
                        run_boom,
                    ),
                )
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
            Ok(())
        }
    }

    #[test]
    fn a_panicking_command_handler_is_contained_as_a_failed_command() {
        let (storage_root, project_root) = roots("commands-panic");
        std::fs::create_dir_all(&project_root).unwrap();
        let project = Project::new(&project_root);
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let mut application = bootstrap
            .authorize_and_mount_with_provider(Arc::new(PanickingCommandPlugin))
            .unwrap();
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            application.dispatch_command("/boom")
        }));
        std::panic::set_hook(previous_hook);
        let dispatched = outcome.expect("dispatch must contain the handler panic");
        match dispatched {
            Err(CommandError::Failed { message }) => {
                assert!(message.contains("panicked"), "names the cause: {message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // dispatch 之后应用仍可用（锁未毒化）：正常命令照常工作。
        assert!(matches!(
            application.dispatch_command("/help"),
            Ok(CommandOutcome::ShowHelp { .. })
        ));
        application.close().unwrap();
        cleanup(&storage_root);
    }
}
