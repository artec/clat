//! `clat exec`：headless 前端。一次调用完成 提问 → agent 循环 → 退出。
//!
//! 不变量（实现与测试都必须从这里推导）：
//!
//! - INV-1 门面唯一：只经 `BootstrapApplication` / `TrustedProjectApplication`
//!   公有方法，绝不直连 agent/provider 服务（审计 CB1 约束）。
//! - INV-2 失败关闭：未信任项目除 trust 查询外不产生任何副作用；
//!   非交互（stdin 非终端）且未传 `--yes` 时，副作用工具一律拒绝。
//! - INV-3 stdout 纯净与管道语义：stdout 只承载模型文本流（可安全
//!   管道）；**写失败（如下游 broken pipe）必须取消 run 并以失败
//!   退出，绝不伪装成功**。状态信息只进 stderr。
//! - INV-4 持久化对等：落盘全部由 core 完成，exec 与等价 TUI 运行
//!   产生完全相同的持久状态；exec 自身不写任何存储路径。
//! - INV-5 干净退出：退出前 join run worker、`close()`，并 join 本
//!   模块产生的全部可 join 线程（状态转发线程有界关闭）。
//! - INV-6 双输入协议：stdin 非 TTY 时，位置参数是指令、stdin 是
//!   上下文，二者合并进同一 prompt，绝不静默丢弃任何一侧；stdin
//!   读取有显式字节预算。
//!
//! 本模块不安装任何进程级信号处理器（HL-05）：中断由进程边界
//! （main.rs）经 [`ExecCancel`] 注入；库形态可重复调用。

use crate::i18n::{LocaleContext, PresentedText, UiMessage};
use crate::{
    ApplicationEvent, ApplicationRunRequest, BootstrapApplication, CommandOutcome,
    CompactionStatus, EventSink, ModelEvent, PermissionApprover, PermissionDecision,
    PermissionRequest, Project, RunEvent, TrustedProjectApplication, Usage,
};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
#[cfg(test)]
use std::time::Duration;

/// stdin（管道 prompt 或管道上下文）的字节预算。超出按用法错误拒绝，
/// 不无界占用内存（HL-06）。
pub const MAX_STDIN_BYTES: usize = 8 * 1024 * 1024;

/// `clat exec` 的已解析参数。
#[derive(Clone, Debug, Default)]
pub struct ExecArgs {
    /// 位置参数：对本次运行的指令。`None` 且 stdin 为管道时，整个
    /// stdin 作为 prompt；与管道 stdin 同时存在时二者按 INV-6 合并。
    pub prompt: Option<String>,
    /// `--command /xxx`：headless 运行一条 `core.commands` 注册表命令
    /// （与 TUI 同一 dispatch 路径），不跑模型、不读 stdin。与位置
    /// 参数互斥。
    pub command: Option<String>,
    pub continue_session: bool,
    pub session: Option<String>,
    pub yes: bool,
    pub trust: bool,
    pub quiet: bool,
    /// `--json`：stdout 改为 NDJSON 事件流——一行一个 RunEvent 信封
    /// `{"v":1,"event":{…}}`（PWA-1，INV-J1）。与 `--command` 互斥（那是
    /// 注册表命令的纯文本面，不在本契约内）。stderr 状态面与 `--quiet`
    /// 语义不变。
    pub json: bool,
}

mod args;
pub use args::{parse_exec_args, parse_exec_args_presented};

mod cancel;
pub use cancel::{ExecCancel, InterruptOutcome};
#[cfg(test)]
use cancel::{InterruptAction, interrupt_action};

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type SharedReader = Arc<Mutex<Box<dyn Read + Send>>>;

/// 一次权限请求对应的一次输入结果。终端实现必须在请求提示已经展示后
/// 才开始接收，并丢弃请求前积压的按键，避免旧的 `y` 批准未来调用。
pub enum ExecPermissionAnswer {
    Answer(String),
    Interrupted,
    Closed,
    Error(String),
}

/// 可取消、请求作用域内的权限输入端口。终端细节由进程前端实现；exec
/// runner 不创建后台 stdin reader，也不持有无法回收的输入线程。
pub trait ExecPermissionInput: Send + Sync {
    fn read_answer(&self, cancel: &ExecCancel) -> ExecPermissionAnswer;
}

/// exec 前端的输入输出端口。生产端由 main.rs 用真实 std 流构造；
/// 测试端注入缓冲区。`interactive_stdin` 决定权限询问能否发生
/// （stdin 为管道时询问无处发生，必须失败关闭——INV-2）。
pub struct ExecIo {
    input: SharedReader,
    output: SharedWriter,
    error: SharedWriter,
    interactive_stdin: bool,
    /// FP-10：stdout 是否为 TTY——TTY 显示路径对模型 delta 做控制
    /// 字符可见转义（OSC/CSI 注入失效）；管道路径保持字节保真契约。
    stdout_is_terminal: bool,
    permission_input: Option<Arc<dyn ExecPermissionInput>>,
    locale: LocaleContext,
}

impl ExecIo {
    pub fn new(
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        error: Box<dyn Write + Send>,
        interactive_stdin: bool,
    ) -> Self {
        Self {
            input: Arc::new(Mutex::new(input)),
            output: Arc::new(Mutex::new(output)),
            error: Arc::new(Mutex::new(error)),
            interactive_stdin,
            stdout_is_terminal: false,
            permission_input: None,
            locale: LocaleContext::default(),
        }
    }

    pub fn with_locale(mut self, locale: LocaleContext) -> Self {
        self.locale = locale;
        self
    }

    /// FP-10：标记 stdout 为 TTY（main 以 `io::stdout().is_terminal()`
    /// 注入；测试用内存缓冲默认管道语义）。
    pub fn with_stdout_terminal(mut self, is_terminal: bool) -> Self {
        self.stdout_is_terminal = is_terminal;
        self
    }

    /// 注入交互式权限输入端口。生产 CLI 使用 main.rs 的终端事件实现；
    /// desktop/IDE 可提供自己的 request-scoped 对话端口。
    pub fn with_permission_input(mut self, input: Arc<dyn ExecPermissionInput>) -> Self {
        self.permission_input = Some(input);
        self
    }
}

/// 测试用内存捕获 writer。
#[cfg(test)]
struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

#[cfg(test)]
impl Write for CapturedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("captured writer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
struct ScriptedPermissionInput {
    input: SharedReader,
}

#[cfg(test)]
impl ExecPermissionInput for ScriptedPermissionInput {
    fn read_answer(&self, cancel: &ExecCancel) -> ExecPermissionAnswer {
        if cancel.interrupted() {
            return ExecPermissionAnswer::Interrupted;
        }
        match read_line(&self.input) {
            Some(_) if cancel.interrupted() => ExecPermissionAnswer::Interrupted,
            Some(answer) => ExecPermissionAnswer::Answer(answer),
            None => ExecPermissionAnswer::Closed,
        }
    }
}

/// exec 测试对 output/error 缓冲的读取句柄。
#[cfg(test)]
pub struct CapturedOutput {
    output: Arc<Mutex<Vec<u8>>>,
    error: Arc<Mutex<Vec<u8>>>,
}

#[cfg(test)]
impl CapturedOutput {
    pub fn output_string(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().expect("captured output")).into_owned()
    }
    pub fn error_string(&self) -> String {
        String::from_utf8_lossy(&self.error.lock().expect("captured error")).into_owned()
    }
}

#[cfg(test)]
impl ExecIo {
    /// 构造非交互（管道 stdin）的内存 IO，并返回捕获句柄。
    fn capture(input: &[u8]) -> (Self, CapturedOutput) {
        let output = Arc::new(Mutex::new(Vec::new()));
        let error = Arc::new(Mutex::new(Vec::new()));
        let io = Self::new(
            Box::new(std::io::Cursor::new(input.to_vec())),
            Box::new(CapturedWriter(Arc::clone(&output))),
            Box::new(CapturedWriter(Arc::clone(&error))),
            false,
        );
        (io, CapturedOutput { output, error })
    }

    /// 构造交互 stdin 的内存 IO，input 为用户对权限询问的按键行。
    fn capture_interactive(input: &[u8]) -> (Self, CapturedOutput) {
        let (mut io, captured) = Self::capture(input);
        io.interactive_stdin = true;
        io.permission_input = Some(Arc::new(ScriptedPermissionInput {
            input: Arc::clone(&io.input),
        }));
        (io, captured)
    }
}

/// exec 的最终结果。退出码翻译归 main.rs；lib 不决定进程语义。
#[derive(Clone, Debug)]
pub enum ExecOutcome {
    Success {
        output: String,
        turns: usize,
        usage: Usage,
    },
    /// 运行前/运行中错误（退出码 1）。I/O 写失败也归入此类（HL-04）。
    Failure(PresentedText),
    /// 用法错误（退出码 2）。
    UsageError(PresentedText),
    /// 用户取消（退出码 130）。
    Cancelled { turns: usize, usage: Usage },
}

/// `clat exec [OPTIONS] [PROMPT]` 的库入口：当前目录项目、`~/.clat`
/// 存储、自带一个内部 `ExecCancel`（无信号安装）。进程边界请用
/// [`run_exec_with_cancel`] 注入信号路由。
pub fn run_exec(args: ExecArgs, io: ExecIo) -> ExecOutcome {
    let cancel = ExecCancel::new();
    run_exec_with_cancel(args, io, &cancel)
}

/// 与 [`run_exec`] 相同，但中断路由由调用方拥有（进程边界把 Ctrl-C
/// 接到 `cancel.on_interrupt()`）。同进程可重复调用（HL-05）。
pub fn run_exec_with_cancel(args: ExecArgs, io: ExecIo, cancel: &ExecCancel) -> ExecOutcome {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            return ExecOutcome::Failure(
                UiMessage::new("cli.exec_current_project")
                    .arg("error", error.to_string())
                    .into(),
            );
        }
    };
    exec_with(
        project,
        None,
        args,
        io,
        |bootstrap| bootstrap.into_trusted(),
        cancel,
    )
}

/// ApplicationEvent 转发线程的 RAII 所有者。receiver 只以 channel
/// disconnect 作为终止条件，因此 Application drop 后会先排空已排队
/// 事件；任何错误早退也会经 Drop join（HL-07）。
struct ApplicationEventForwarder {
    join: Option<std::thread::JoinHandle<()>>,
}

impl ApplicationEventForwarder {
    fn spawn(
        events: mpsc::Receiver<ApplicationEvent>,
        error: SharedWriter,
        io_state: SharedIoState,
        locale: LocaleContext,
    ) -> Self {
        let join = std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                if let ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    note,
                    ..
                }) = event
                {
                    write_status(
                        &error,
                        &io_state,
                        format_args!("● {}\n", note.render(locale)),
                    );
                }
            }
        });
        Self { join: Some(join) }
    }

    fn join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ApplicationEventForwarder {
    fn drop(&mut self) {
        self.join();
    }
}

/// 共享实现：`into_trusted` 由调用方注入（生产走真实 catalog，
/// 测试注入脚本 provider）。
fn exec_with<F>(
    project: Project,
    storage_root: Option<PathBuf>,
    args: ExecArgs,
    io: ExecIo,
    into_trusted: F,
    cancel: &ExecCancel,
) -> ExecOutcome
where
    F: FnOnce(BootstrapApplication) -> Result<TrustedProjectApplication, crate::ApplicationError>,
{
    let io_state: SharedIoState = Arc::new(ExecIoState::default());

    let mut application =
        match open_exec_application(&project, storage_root, args.trust, into_trusted) {
            Ok(application) => application,
            Err(outcome) => return outcome,
        };

    // 会话策略：默认新会话（脚本可预测）；--continue / --session 显式续接。
    if let Err(error) = apply_session_policy(&mut application, &args) {
        return close_and_finalize(
            application,
            ExecOutcome::Failure(error),
            &args,
            &io,
            &io_state,
        );
    }

    // `--command`：core.commands 注册表的 headless 形态（与 TUI 同一
    // dispatch 路径，INV-C2）。不跑模型、不读 stdin；命令各自的失败
    // （如未配置模型时的 /compact）由 dispatch 以结构化错误返回。
    let task_resume = match prepare_task_command(&mut application, args.command.as_deref()) {
        Ok(resume) => resume,
        Err(error) => {
            return close_and_finalize(
                application,
                ExecOutcome::Failure((error).into()),
                &args,
                &io,
                &io_state,
            );
        }
    };
    if let Some(command) = args.command.clone().filter(|_| task_resume.is_none()) {
        return run_headless_command(application, &command, &args, &io, &io_state);
    }

    let (config, _credentials) = match application.model_state() {
        Ok(state) => state,
        Err(error) => {
            return close_and_finalize(
                application,
                ExecOutcome::Failure((error.to_string()).into()),
                &args,
                &io,
                &io_state,
            );
        }
    };
    if !config.is_configured() {
        return close_and_finalize(
            application,
            ExecOutcome::Failure(UiMessage::new("cli.exec_model_missing").into()),
            &args,
            &io,
            &io_state,
        );
    }

    // INV-6：prompt 在信任门/模型检查之后才读 stdin（无效调用不吞输入）。
    let prompt = match task_resume.as_ref().map_or_else(
        || resolve_prompt(&args, &io, MAX_STDIN_BYTES),
        |_| Ok(String::new()),
    ) {
        Ok(prompt) => prompt,
        Err(outcome) => return close_and_finalize(application, outcome, &args, &io, &io_state),
    };

    // 压缩状态可见性（ApplicationEvent 只是展示通道，不进入退出语义）。
    // RAII forwarder 在所有 return 路径 join；sender 断开前会排空队列。
    let mut forwarder = None;
    if !args.quiet {
        let (events_tx, events_rx) = mpsc::channel::<ApplicationEvent>();
        application.subscribe(events_tx);
        forwarder = Some(ApplicationEventForwarder::spawn(
            events_rx,
            Arc::clone(&io.error),
            Arc::clone(&io_state),
            io.locale,
        ));
    }

    let (completion_tx, completion_rx) = mpsc::channel();
    let request = exec_request((&args, &io, &io_state, cancel), prompt, completion_tx);
    let started = match task_resume {
        Some(resume) => application
            .start_task_run(resume, request)
            .map(|(handle, _)| handle),
        None => application.start_run(request),
    };
    let handle = match started {
        Ok(handle) => handle,
        Err(error) => {
            return close_and_finalize(
                application,
                ExecOutcome::Failure((error.to_string()).into()),
                &args,
                &io,
                &io_state,
            );
        }
    };
    // 发布 handle 会兑现句柄就位前的 pending Ctrl-C；stdout failure
    // 若先发生，sink 会阻塞到这里并在返回前 cancel（HL-03/HL-04）。
    cancel.attach(handle.clone());

    // 完成消息在持久化与 run scope 清理之后到达（Application 契约）。
    let result = match completion_rx.recv() {
        Ok(result) => result,
        Err(_) => {
            let _ = handle.join();
            cancel.detach();
            return close_and_finalize(
                application,
                ExecOutcome::Failure("run worker exited without a result".into()),
                &args,
                &io,
                &io_state,
            );
        }
    };
    if let Err(error) = handle.join() {
        cancel.detach();
        return close_and_finalize(
            application,
            ExecOutcome::Failure((error.to_string()).into()),
            &args,
            &io,
            &io_state,
        );
    }
    cancel.detach();

    let outcome = match result {
        Ok(done) if done.cancelled => {
            if !args.quiet {
                write_status(
                    &io.error,
                    &io_state,
                    format_args!(
                        "{}",
                        io.locale
                            .render(&UiMessage::new("cli.exec_cancelled").arg("turns", done.turns))
                    ),
                );
            }
            ExecOutcome::Cancelled {
                turns: done.turns,
                usage: done.usage.clone(),
            }
        }
        Ok(done) => {
            if !args.quiet {
                write_status(
                    &io.error,
                    &io_state,
                    format_args!(
                        "{}",
                        io.locale.render(
                            &UiMessage::new("cli.exec_summary")
                                .arg("turns", done.turns)
                                .arg("input", done.usage.input_tokens)
                                .arg("output", done.usage.output_tokens)
                        )
                    ),
                );
            }
            // 流式文本以换行收尾：stdout 可直接进管道，不与 shell 提示符
            // 粘连（仍是助手文本的呈现，INV-3 契约随测试固化）。
            // `--json` 不补：每行信封自带换行，stdout 是 NDJSON 单一契约。
            if !args.json && !done.output.is_empty() {
                let result = stream_write(&io.output, format_args!("\n"))
                    .and_then(|()| stream_flush(&io.output));
                if let Err(error) = result {
                    io_state.note("stdout", error);
                }
            }
            ExecOutcome::Success {
                output: done.output,
                turns: done.turns,
                usage: done.usage,
            }
        }
        Err(failure) => ExecOutcome::Failure((failure.error).into()),
    };

    // HL-04：输出写失败不得伪装成功。取消（下游已关）+ 成功都改判失败；
    // 已有失败保留原错误并追加 I/O 错误。
    let io_failure = io_state.take_first_error();
    let outcome = match (io_failure, outcome) {
        (Some(error), ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. }) => {
            ExecOutcome::Failure((error).into())
        }
        (Some(error), ExecOutcome::Failure(message)) => ExecOutcome::Failure(
            UiMessage::new("cli.exec_failure_combined")
                .arg_message("message", message.message())
                .arg("error", error)
                .into(),
        ),
        // UsageError 路径早于 sink 存在，不会携带 I/O 错误。
        (Some(_), outcome @ ExecOutcome::UsageError(_)) => outcome,
        (None, outcome) => outcome,
    };

    // INV-5：干净退出。run 失败也要走 close（join 后台 worker、flush
    // 会话与 checkpoint）；close 必须先于 forwarder.join——monitor 的
    // sender 在 close 内释放，通道关闭后转发线程才能排空退出。
    // PWA1-01：`--json` 的 invocation 终态行在 close **之后**由最终
    // outcome 派生（close_and_finalize），run 终态不再是进程级承诺。
    let closed_outcome = close_and_finalize(application, outcome, &args, &io, &io_state);
    if let Some(forwarder) = forwarder.as_mut() {
        forwarder.join();
    }
    closed_outcome
}

/// `--command` 的 headless 渲染：outcome → 文本 + 退出码。查询类结果
/// （/help、/mcp）是本次调用的**产品**，写 stdout（该路径没有模型文本
/// 流，不存在 INV-3 的混流问题）；状态类走 stderr。交互类命令在
/// headless 没有对应呈现，按用法错误拒绝（脚本可判定）。压缩经
/// `join_report` 等待结果（压缩 worker 有总 deadline 与尝试上限，join
/// 有界）。
fn headless_command_forwarder(
    application: &TrustedProjectApplication,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> Option<ApplicationEventForwarder> {
    let mut forwarder = None;
    if !args.quiet {
        let (events_tx, events_rx) = mpsc::channel::<ApplicationEvent>();
        application.subscribe(events_tx);
        forwarder = Some(ApplicationEventForwarder::spawn(
            events_rx,
            Arc::clone(&io.error),
            Arc::clone(io_state),
            io.locale,
        ));
    }
    forwarder
}

fn run_headless_command(
    mut application: TrustedProjectApplication,
    command: &str,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    // 与 run 路径同一可见性通道：CompactionUpdated 的 stderr 通知。
    let mut forwarder = headless_command_forwarder(&application, args, io, io_state);
    let goal_words = command.split_whitespace().collect::<Vec<_>>();
    let interactive_goal_run = goal_words.first().copied() == Some("/goal")
        && (goal_words.get(1).copied() == Some("run")
            || (goal_words.get(1).copied() == Some("create") && goal_words.contains(&"--run")));
    let outcome = if interactive_goal_run {
        if let Err(error) = application.check_goal_activation() {
            ExecOutcome::Failure((error.to_string()).into())
        } else {
            ExecOutcome::UsageError(
                UiMessage::new("cli.headless_goal")
                    .arg("command", command)
                    .into(),
            )
        }
    } else {
        match application.dispatch_command(command) {
            Ok(CommandOutcome::StartCompaction(handle)) => {
                finish_command_compaction(handle, args, io, io_state)
            }
            Ok(CommandOutcome::StartVisionProbe(handle)) => {
                finish_headless_probe(handle, args, io, io_state)
            }
            Ok(CommandOutcome::ShowHelp { commands }) => write_help_view(&commands, io, io_state),
            Ok(CommandOutcome::ShowMcpStatus(status)) => write_mcp_view(status, io, io_state),
            Ok(CommandOutcome::ShowMemory(view)) => {
                write_content_view(view.localized_text(io.locale), io, io_state)
            }
            Ok(CommandOutcome::QueryHistory(request)) => match application
                .history_reader()
                .execute(request, &crate::CancelToken::new())
            {
                Ok(view) => write_content_view(
                    serde_json::to_string_pretty(&view).unwrap_or_default(),
                    io,
                    io_state,
                ),
                Err(error) => ExecOutcome::Failure(error.presentation().into()),
            },
            Ok(CommandOutcome::ShowTask(text) | CommandOutcome::ShowCapabilities(text)) => {
                write_content_view(text, io, io_state)
            }
            Ok(CommandOutcome::ShowJobs(value)) => write_content_view(
                crate::background_jobs::command::display(&value, io.locale),
                io,
                io_state,
            ),
            Ok(CommandOutcome::ShowSchedules(value)) => write_content_view(
                crate::scheduler::command::display(&value, io.locale),
                io,
                io_state,
            ),
            Ok(CommandOutcome::StartTaskRun(_)) => {
                ExecOutcome::Failure("task continuation was not routed to execution".into())
            }
            Ok(CommandOutcome::ShowGoal(view)) => {
                write_content_view(view.localized_text(io.locale), io, io_state)
            }
            Ok(CommandOutcome::ShowSubagentStatus(view)) => {
                write_content_view(view.localized_text(io.locale), io, io_state)
            }
            Ok(CommandOutcome::ShowSkills(view)) => {
                write_content_view(view.localized_text(io.locale), io, io_state)
            }
            Ok(CommandOutcome::ShowContext(view)) => {
                write_content_view(view.localized_text(io.locale), io, io_state)
            }
            Ok(CommandOutcome::Status(message)) => {
                if !args.quiet {
                    write_status(
                        &io.error,
                        io_state,
                        format_args!("{}\n", message.render(io.locale)),
                    );
                }
                ExecOutcome::Success {
                    output: String::new(),
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::SessionReset) => {
                if !args.quiet {
                    write_status(
                        &io.error,
                        io_state,
                        format_args!("{}\n", io.locale.text("tui.new_conversation")),
                    );
                }
                ExecOutcome::Success {
                    output: String::new(),
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            // headless 无应用生命周期概念：/quit 等价无操作（正常退出）。
            Ok(CommandOutcome::QuitRequested) => ExecOutcome::Success {
                output: String::new(),
                turns: 0,
                usage: Usage::default(),
            },
            Ok(
                CommandOutcome::StartPluginManagement
                | CommandOutcome::StartModelSelection
                | CommandOutcome::StartSessionSelection { .. }
                | CommandOutcome::StartPermissionModeSelection { .. }
                | CommandOutcome::StartTitleEdit { .. }
                | CommandOutcome::StartGoalRun { .. },
            ) => ExecOutcome::UsageError(
                UiMessage::new("cli.headless_interactive")
                    .arg("command", command)
                    .into(),
            ),
            Err(error) => ExecOutcome::Failure(error.presentation().into()),
        }
    };
    // HL-04 同款：stdout 写失败不得伪装成功。
    let outcome = match (io_state.take_first_error(), outcome) {
        (Some(error), ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. }) => {
            ExecOutcome::Failure((error).into())
        }
        (Some(error), ExecOutcome::Failure(message)) => ExecOutcome::Failure(
            UiMessage::new("cli.exec_failure_combined")
                .arg_message("message", message.message())
                .arg("error", error)
                .into(),
        ),
        (Some(_), outcome @ ExecOutcome::UsageError(_)) => outcome,
        (None, outcome) => outcome,
    };
    let closed = close_application(application, outcome);
    if let Some(forwarder) = forwarder.as_mut() {
        forwarder.join();
    }
    closed
}

fn write_mcp_view(
    status: crate::McpStatusDto,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    write_content_view(status.localized_text(io.locale), io, io_state)
}

fn write_content_view(text: String, io: &ExecIo, io_state: &SharedIoState) -> ExecOutcome {
    let output = format!("{text}\n");
    if let Err(error) =
        stream_write(&io.output, format_args!("{output}")).and_then(|()| stream_flush(&io.output))
    {
        io_state.note("stdout", error);
    }
    ExecOutcome::Success {
        output,
        turns: 0,
        usage: Usage::default(),
    }
}

/// 观察每个退出路径上的 application.close()（审计 P2-01）：早退不再吞掉
/// flush/join 失败；已有失败时追加而不是覆盖，用户能同时看到主错误与
/// 持久化收尾错误。
fn close_application(application: TrustedProjectApplication, outcome: ExecOutcome) -> ExecOutcome {
    match application.close() {
        Ok(()) => outcome,
        Err(error) => match outcome {
            ExecOutcome::Failure(message) => ExecOutcome::Failure(
                UiMessage::new("cli.exec_close_combined")
                    .arg_message("message", message.message())
                    .arg("error", error.to_string())
                    .into(),
            ),
            // UsageError keeps its exit-code contract, but the close
            // failure still reaches the user (never swallowed).
            ExecOutcome::UsageError(message) => ExecOutcome::UsageError(
                UiMessage::new("cli.exec_close_combined")
                    .arg_message("message", message.message())
                    .arg("error", error.to_string())
                    .into(),
            ),
            ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. } => ExecOutcome::Failure(
                UiMessage::new("cli.exec_close_failure")
                    .arg("error", error.to_string())
                    .into(),
            ),
        },
    }
}

/// exec_with 的统一收尾（PWA1-01）：先关 application（close 失败可把
/// Success/Cancelled 改判 Failure——这正是不变量所在），**然后**才从
/// 最终 outcome 派生 invocation 终态行。`--json` 下 exec 终态与进程
/// 退出码出自同一份数据，二者不可能矛盾；它恒为 stdout 最后一行
/// （run worker 已 join，不会再有 RunEvent 行与之竞争次序）。
/// 非 json 路径行为与直接调 close_application 字节同。
///
/// 终态行写失败（消费者断管）与文本路径的收尾换行同纪律：改判失败，
/// 绝不伪装成功（INV-J6）。
fn close_and_finalize(
    application: TrustedProjectApplication,
    outcome: ExecOutcome,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    let closed = close_application(application, outcome);
    if !args.json {
        return closed;
    }
    let line = match &closed {
        ExecOutcome::Success { .. } => crate::wire::exec_completed_line(0),
        ExecOutcome::Failure(message) => crate::wire::exec_failed_line(1, message),
        // UsageError 保留退出码 2 契约（与 close_application 同款）。
        ExecOutcome::UsageError(message) => crate::wire::exec_failed_line(2, message),
        ExecOutcome::Cancelled { turns, .. } => {
            crate::wire::exec_failed_line(130, &format!("cancelled after {turns} turns"))
        }
    };
    let result =
        stream_write(&io.output, format_args!("{line}")).and_then(|()| stream_flush(&io.output));
    match result {
        Ok(()) => closed,
        Err(error) => {
            io_state.note("stdout", error);
            match closed {
                ExecOutcome::Failure(message) => ExecOutcome::Failure(
                    UiMessage::new("cli.exec_write_combined")
                        .arg_message("message", message.message())
                        .into(),
                ),
                ExecOutcome::UsageError(message) => ExecOutcome::UsageError(
                    UiMessage::new("cli.exec_write_combined")
                        .arg_message("message", message.message())
                        .into(),
                ),
                ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. } => {
                    ExecOutcome::Failure("stdout write failed".into())
                }
            }
        }
    }
}

/// prompt 双输入协议（INV-6 / HL-01）：
/// - 终端 stdin：只有位置指令（或用法错误）。
/// - 管道 stdin：`指令 + 上下文` 合并；只有管道时全文即 prompt；
///   管道为空则退回纯指令。任何一侧都不被静默丢弃。
fn resolve_prompt(args: &ExecArgs, io: &ExecIo, limit: usize) -> Result<String, ExecOutcome> {
    let instruction = match args.prompt.as_deref() {
        Some(instruction) if instruction.trim().is_empty() => {
            return Err(ExecOutcome::UsageError(
                UiMessage::new("cli.exec_empty_prompt").into(),
            ));
        }
        instruction => instruction,
    };
    if io.interactive_stdin {
        return match instruction {
            Some(instruction) => Ok(instruction.to_string()),
            None => Err(ExecOutcome::UsageError(
                UiMessage::new("cli.exec_missing_prompt").into(),
            )),
        };
    }
    let piped = read_stdin_budgeted(io, limit)?;
    if piped.trim().is_empty() {
        return match instruction {
            Some(instruction) => Ok(instruction.to_string()),
            None => Err(ExecOutcome::UsageError(
                UiMessage::new("cli.exec_empty_stdin").into(),
            )),
        };
    }
    match instruction {
        Some(instruction) => Ok(compose_prompt(instruction, &piped)),
        None => Ok(piped),
    }
}

fn compose_prompt(instruction: &str, context: &str) -> String {
    let mut context = context.to_string();
    if !context.ends_with('\n') {
        context.push('\n');
    }
    format!("{instruction}\n\n--- piped input follows ---\n{context}--- end of piped input ---")
}

fn read_stdin_budgeted(io: &ExecIo, limit: usize) -> Result<String, ExecOutcome> {
    let mut buffer = String::new();
    let count = {
        let mut input = io.input.lock().expect("stdin");
        let mut reader = (&mut *input).take((limit + 1) as u64);
        reader.read_to_string(&mut buffer).map_err(|error| {
            ExecOutcome::Failure(
                UiMessage::new("cli.exec_stdin_read")
                    .arg("error", error.to_string())
                    .into(),
            )
        })?
    };
    if count > limit {
        return Err(ExecOutcome::UsageError(
            UiMessage::new("cli.exec_stdin_limit")
                .arg("limit", limit)
                .into(),
        ));
    }
    Ok(buffer)
}

fn apply_session_policy(
    application: &mut TrustedProjectApplication,
    args: &ExecArgs,
) -> Result<(), PresentedText> {
    if args.continue_session {
        // 优先 workspace 选择；缺失时回退该项目最近会话（plan §13.1
        // 的回退语义，不再复刻旧 SQLite 秒级排序细节）。
        if application.current_session_id().is_some() {
            return Ok(());
        }
        let sessions = application
            .list_sessions()
            .map_err(|error| error.to_string())?;
        let most_recent = sessions.first().cloned();
        return match most_recent {
            Some(session) => {
                let display_id = session.id.to_string();
                application
                    .switch_session(session.id)
                    .map(|_| ())
                    .map_err(move |error| {
                        UiMessage::new("cli.exec_session_switch")
                            .arg("id", display_id)
                            .arg("error", error.to_string())
                            .into()
                    })
            }
            None => Err(UiMessage::new("cli.exec_no_session").into()),
        };
    }
    if let Some(id) = args.session.clone() {
        let session_id = crate::SessionId::new(id.clone());
        return application
            .switch_session(session_id)
            .map(|_| ())
            .map_err(|error| {
                UiMessage::new("cli.exec_session_switch")
                    .arg("id", id)
                    .arg("error", error.to_string())
                    .into()
            });
    }
    application.new_session().map_err(|error| {
        UiMessage::new("cli.exec_new_session")
            .arg("error", error.to_string())
            .into()
    })
}

// ---- I/O 错误跟踪（HL-04）----

/// 记录首个输出写错误；sink、状态行与转发线程共享。
#[derive(Default)]
struct ExecIoState {
    first_error: Mutex<Option<String>>,
}

type SharedIoState = Arc<ExecIoState>;

impl ExecIoState {
    fn note(&self, stream: &str, error: std::io::Error) {
        let mut slot = self.first_error.lock().expect("io state");
        if slot.is_none() {
            *slot = Some(format!("{stream} write failed: {error}"));
        }
    }

    fn take_first_error(&self) -> Option<String> {
        self.first_error.lock().expect("io state").take()
    }
}

/// FP-10：TTY 显示路径的控制字符可见转义。保留 \n/\t/\r 与全部可
/// 打印/多字节字符；其余 C0 与 DEL 转为 `\xNN` 字面形式——转义掉
/// ESC 引入符即废掉整个 OSC/CSI 序列（其余字节只是无害文本）。
fn sanitize_tty_text(delta: &str) -> String {
    let mut out = String::with_capacity(delta.len());
    for character in delta.chars() {
        match character {
            '\n' | '\t' | '\r' => out.push(character),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn stream_write(writer: &SharedWriter, args: std::fmt::Arguments<'_>) -> std::io::Result<()> {
    writer.lock().expect("exec writer").write_fmt(args)
}

fn stream_flush(writer: &SharedWriter) -> std::io::Result<()> {
    writer.lock().expect("exec writer").flush()
}

/// 状态行写入（stderr / 摘要行）：记录错误但不取消 run——诊断流失败
/// 不是数据契约破裂。
fn write_status(writer: &SharedWriter, state: &SharedIoState, args: std::fmt::Arguments<'_>) {
    if let Err(error) = stream_write(writer, args) {
        state.note("stderr", error);
    }
}

enum PermissionMode {
    AllowAll,
    Interactive,
    NonInteractive,
}

/// headless 权限审批（INV-2）：`--yes` 全批；终端输入仅在具体请求已
/// 展示后读取一次，输入端口负责丢弃请求前积压按键并支持取消
/// （HL-02/08/09）；非交互一律拒绝并说明原因。
struct ExecApprover {
    locale: LocaleContext,
    mode: PermissionMode,
    input: Option<Arc<dyn ExecPermissionInput>>,
    error: SharedWriter,
    io_state: SharedIoState,
    interrupt: ExecCancel,
}

impl ExecApprover {
    fn interactive(&self, request: PermissionRequest) -> PermissionDecision {
        write_status(
            &self.error,
            &self.io_state,
            format_args!(
                "{}",
                self.locale.render(
                    &UiMessage::new("cli.exec_permission")
                        .arg("tool", request.tool.clone())
                        .arg("effect", format!("{:?}", request.effect))
                        .arg("reason", request.reason.clone())
                )
            ),
        );
        write_status(
            &self.error,
            &self.io_state,
            format_args!(
                "{}",
                self.locale.render(
                    &UiMessage::new("cli.exec_arguments")
                        .arg("arguments", request.arguments.clone())
                )
            ),
        );
        write_status(
            &self.error,
            &self.io_state,
            format_args!("{}", self.locale.text("cli.exec_allow")),
        );
        if let Err(error) = stream_flush(&self.error) {
            self.io_state.note("stderr", error);
        }
        let Some(input) = &self.input else {
            return PermissionDecision::Unavailable {
                reason: "interactive input is unavailable".into(),
            };
        };
        let answer = input.read_answer(&self.interrupt);
        write_status(&self.error, &self.io_state, format_args!("\n"));
        match answer {
            ExecPermissionAnswer::Answer(answer)
                if answer.trim().eq_ignore_ascii_case("y")
                    || answer.trim().eq_ignore_ascii_case("yes") =>
            {
                PermissionDecision::Allow
            }
            ExecPermissionAnswer::Answer(_) => PermissionDecision::Deny {
                reason: "denied by user".into(),
            },
            ExecPermissionAnswer::Interrupted => PermissionDecision::Deny {
                reason: "interrupted by user".into(),
            },
            ExecPermissionAnswer::Closed => PermissionDecision::Unavailable {
                reason: "no answer available (stdin closed)".into(),
            },
            ExecPermissionAnswer::Error(error) => PermissionDecision::Unavailable {
                reason: format!("permission input failed: {error}"),
            },
        }
    }
}
impl PermissionApprover for ExecApprover {
    fn decide(
        &self,
        request: PermissionRequest,
        cancel: &crate::model::CancelToken,
    ) -> PermissionDecision {
        // W1-17/A1：run 已取消（如断管触发的取消）时不进入交互等待，
        // 直接以中断语义拒绝。
        if cancel.is_cancelled() {
            return PermissionDecision::Deny {
                reason: "interrupted by run cancellation".into(),
            };
        }
        match self.mode {
            PermissionMode::AllowAll => PermissionDecision::Allow,
            PermissionMode::NonInteractive => PermissionDecision::Unavailable {
                reason: format!(
                    "non-interactive run denied `{}`; pass --yes to allow side effects",
                    request.tool
                ),
            },
            PermissionMode::Interactive => self.interactive(request),
        }
    }
}

/// 读一行（不含换行）。`None` 表示 EOF：与空行（用户直接回车=拒绝）
/// 显式区分。
#[cfg(test)]
fn read_line(input: &SharedReader) -> Option<String> {
    let mut buffer = Vec::new();
    let mut guard = input.lock().expect("stdin");
    // TTY 上阻塞等待逐字节到达；EOF 或读错误视为输入结束。
    let mut byte = [0u8; 1];
    loop {
        match guard.read(&mut byte) {
            Ok(0) => {
                if buffer.is_empty() {
                    drop(guard);
                    return None;
                }
                break;
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => buffer.push(byte[0]),
            Err(_) => break,
        }
    }
    drop(guard);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// 事件分流（INV-3）：模型文本进 output（stdout），写/flush 失败即
/// 记录并取消 run（下游已关闭，继续执行只是浪费模型与副作用，HL-04）；
/// 状态进 error（stderr），`--quiet` 只保留权限询问（那是交互，不是
/// 噪音）。
///
/// `--json`（INV-J1）：stdout 单一契约改为 NDJSON——每个事件一行
/// `{"v":1,"event":{…}}` 信封（发射序即行序），模型文本不再走纯文本
/// 路径（文本在 `model_stream` 事件载荷里）；stderr 状态面原样。
/// 写失败沿 INV-3 同一管道（记错 + 取消），退出语义不变（INV-J6）。
struct ExecEventSink {
    locale: LocaleContext,
    output: SharedWriter,
    error: SharedWriter,
    quiet: bool,
    json: bool,
    stdout_is_terminal: bool,
    io_state: SharedIoState,
    cancel: ExecCancel,
}

impl ExecEventSink {
    fn output(&self, text: &str) {
        let result = stream_write(&self.output, format_args!("{text}"))
            .and_then(|()| stream_flush(&self.output));
        if let Err(error) = result {
            self.io_state.note("stdout", error);
            self.cancel.cancel_active_run_waiting_for_handle();
        }
    }
    fn notice(&self, message: UiMessage) {
        if !self.quiet {
            write_status(
                &self.error,
                &self.io_state,
                format_args!("{}", self.locale.render(&message)),
            );
        }
    }
}
impl EventSink for ExecEventSink {
    fn emit(&mut self, event: RunEvent) {
        if self.json {
            self.output(&crate::wire::envelope_line(&event));
        }
        match event {
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { delta },
                ..
            }
            | RunEvent::ModelStream {
                event: ModelEvent::RefusalDelta { delta },
                ..
            } if !self.json => {
                let text = if self.stdout_is_terminal {
                    sanitize_tty_text(&delta)
                } else {
                    delta
                };
                self.output(&text);
            }
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => self.notice(
                UiMessage::new("cli.exec_turn")
                    .arg("provider", provider)
                    .arg("model", model)
                    .arg("turn", turn),
            ),
            RunEvent::ToolStarted { tool, .. } => {
                self.notice(UiMessage::new("cli.exec_tool").arg("tool", tool))
            }
            RunEvent::PermissionDenied { tool, reason } => self.notice(
                UiMessage::new("cli.exec_denied")
                    .arg("tool", tool)
                    .arg("reason", reason),
            ),
            RunEvent::ToolFinished { result } => self.notice(
                UiMessage::new("cli.exec_finished")
                    .arg("mark", if result.is_error { "✗" } else { "✓" })
                    .arg("tool", result.tool_name),
            ),
            _ => {}
        }
    }
}

fn prepare_task_command(
    app: &mut TrustedProjectApplication,
    command: Option<&str>,
) -> Result<Option<crate::task::TaskResume>, String> {
    let Some(command) = command else {
        return Ok(None);
    };
    let words: Vec<_> = command.split_whitespace().take(2).collect();
    if words != ["/task", "continue"] {
        return Ok(None);
    }
    match app.dispatch_command(command).map_err(|e| e.to_string())? {
        CommandOutcome::StartTaskRun(resume) => Ok(Some(resume)),
        _ => Err("unexpected task continuation outcome".into()),
    }
}

fn write_help_view(
    commands: &[crate::CommandInfo],
    io: &ExecIo,
    state: &SharedIoState,
) -> ExecOutcome {
    let mut text = String::new();
    for info in commands {
        let mut names = format!("/{}", info.name);
        for alias in &info.aliases {
            names.push_str(&format!(", /{alias}"));
        }
        let description = crate::i18n::command_presentation(&info.name, &info.description)
            .map(|m| io.locale.render(&m))
            .unwrap_or_else(|| info.description.clone());
        text.push_str(&format!("{names} — {description}\n"));
    }
    if let Err(error) =
        stream_write(&io.output, format_args!("{text}")).and_then(|()| stream_flush(&io.output))
    {
        state.note("stdout", error);
    }
    ExecOutcome::Success {
        output: text,
        turns: 0,
        usage: Usage::default(),
    }
}

fn exec_request(
    (args, io, state, cancel): (&ExecArgs, &ExecIo, &SharedIoState, &ExecCancel),
    prompt: String,
    completion: mpsc::Sender<
        Result<crate::application::ApplicationRunDone, crate::application::ApplicationRunFailure>,
    >,
) -> ApplicationRunRequest {
    let mode = if args.yes {
        PermissionMode::AllowAll
    } else if io.interactive_stdin {
        PermissionMode::Interactive
    } else {
        PermissionMode::NonInteractive
    };
    ApplicationRunRequest {
        message: crate::message::PendingMessage::text(prompt),
        asker: None,
        completion,
        approver: Arc::new(ExecApprover {
            locale: io.locale,
            mode,
            input: io.permission_input.clone(),
            error: Arc::clone(&io.error),
            io_state: Arc::clone(state),
            interrupt: cancel.clone(),
        }),
        events: Box::new(ExecEventSink {
            locale: io.locale,
            output: Arc::clone(&io.output),
            error: Arc::clone(&io.error),
            quiet: args.quiet,
            json: args.json,
            stdout_is_terminal: io.stdout_is_terminal,
            io_state: Arc::clone(state),
            cancel: cancel.clone(),
        }),
    }
}

fn open_exec_application<F>(
    project: &Project,
    storage_root: Option<PathBuf>,
    trust: bool,
    into_trusted: F,
) -> Result<TrustedProjectApplication, ExecOutcome>
where
    F: FnOnce(BootstrapApplication) -> Result<TrustedProjectApplication, crate::ApplicationError>,
{
    let bootstrap = match storage_root {
        Some(root) => BootstrapApplication::open(project.clone(), root),
        None => BootstrapApplication::open_default(project.clone()),
    };
    let bootstrap = match bootstrap {
        Ok(bootstrap) => bootstrap,
        Err(error) => return Err(ExecOutcome::Failure((error.to_string()).into())),
    };
    // INV-2：未信任 → 只读检查即退出；--trust 是唯一非交互通路，经
    // 一次性 ProjectAuthorization + authorize_and_mount（无独立
    // trust 写入口，plan §16 阶段5）。
    let application = match bootstrap.is_trusted() {
        Ok(true) => match into_trusted(bootstrap) {
            Ok(application) => application,
            Err(error) => return Err(ExecOutcome::Failure((error.to_string()).into())),
        },
        Ok(false) if trust => {
            match bootstrap.authorize_and_mount(crate::ProjectAuthorization::grant()) {
                Ok(application) => application,
                Err(error) => return Err(ExecOutcome::Failure((error.to_string()).into())),
            }
        }
        Ok(false) => {
            return Err(ExecOutcome::Failure(
                UiMessage::new("cli.exec_untrusted").into(),
            ));
        }
        Err(error) => return Err(ExecOutcome::Failure((error.to_string()).into())),
    };

    Ok(application)
}

#[cfg(test)]
mod tests;

fn finish_command_compaction(
    handle: crate::CompactHandle,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    match handle.join_report() {
        // 外层 Err = join 失败（worker 崩溃）；内层 Err = 压缩自身的
        // 失败报告。
        Ok(Ok(report)) => {
            if !args.quiet {
                write_status(
                    &io.error,
                    io_state,
                    format_args!("● {}\n", io.locale.render(&report.status_presentation())),
                );
            }
            ExecOutcome::Success {
                output: String::new(),
                turns: 0,
                usage: Usage::default(),
            }
        }
        Ok(Err(message)) => ExecOutcome::Failure(message),
        Err(error) => ExecOutcome::Failure((error.to_string()).into()),
    }
}

fn finish_headless_probe(
    handle: crate::application::VisionProbeHandle,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    match handle.join_report() {
        // VP-1：headless 腿同步等判定；报告一行式打印。
        Ok(report) => {
            if !args.quiet {
                write_status(
                    &io.error,
                    io_state,
                    format_args!("● {}\n", io.locale.render(&report.status_presentation())),
                );
            }
            match report.outcome {
                crate::application::VisionProbeOutcome::Pass => ExecOutcome::Success {
                    output: String::new(),
                    turns: 0,
                    usage: Usage::default(),
                },
                crate::application::VisionProbeOutcome::Rejected
                | crate::application::VisionProbeOutcome::SilentDrop => {
                    ExecOutcome::Failure(report.status_presentation().into())
                }
                crate::application::VisionProbeOutcome::Inconclusive => {
                    ExecOutcome::Failure(report.status_presentation().into())
                }
            }
        }
        Err(error) => ExecOutcome::Failure((error.to_string()).into()),
    }
}
