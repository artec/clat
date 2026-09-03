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

use crate::{
    ApplicationEvent, ApplicationRunRequest, BootstrapApplication, CommandOutcome,
    CompactionStatus, EventSink, ModelEvent, PermissionApprover, PermissionDecision,
    PermissionRequest, Project, RunEvent, RunHandle, TrustedProjectApplication, Usage,
};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
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

/// 解析 `clat exec` 之后的参数。用法错误返回 `Err(message)`（退出码 2）。
pub fn parse_exec_args<I>(args: I) -> Result<ExecArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = ExecArgs::default();
    let mut iter = args.into_iter();
    // `--` 之后的 token 全部视为位置参数（prompt 以 `-` 开头时必需）。
    let mut positional_only = false;
    while let Some(arg) = iter.next() {
        let arg = arg.as_str();
        if positional_only {
            if parsed.prompt.is_some() {
                return Err(format!("unexpected extra argument: {arg}"));
            }
            parsed.prompt = Some(arg.to_string());
            continue;
        }
        match arg {
            "--continue" => parsed.continue_session = true,
            "--yes" => parsed.yes = true,
            "--trust" => parsed.trust = true,
            "--quiet" => parsed.quiet = true,
            "--json" => parsed.json = true,
            "--command" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--command requires a /command".to_string())?;
                if !value.starts_with('/') || value.trim().is_empty() {
                    return Err(format!("invalid command: {value} (expected a /command)"));
                }
                parsed.command = Some(value);
            }
            "--session" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--session requires a session id".to_string())?;
                if value.trim().is_empty() {
                    return Err("invalid session id: empty".to_string());
                }
                // DSH SessionId 是开放字符串（CLAT 生成 UUID）；不在此
                // 强加格式，resume 时的存在性校验才是权威。
                parsed.session = Some(value.to_string());
            }
            "--" => positional_only = true,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option: {other}"));
            }
            positional => {
                if parsed.prompt.is_some() {
                    return Err(format!("unexpected extra argument: {positional}"));
                }
                parsed.prompt = Some(positional.to_string());
            }
        }
    }
    if parsed.continue_session && parsed.session.is_some() {
        return Err("--continue and --session are mutually exclusive".into());
    }
    if parsed.command.is_some() && parsed.prompt.is_some() {
        return Err("--command cannot be combined with a prompt".into());
    }
    if parsed.json && parsed.command.is_some() {
        return Err("--json cannot be combined with --command".into());
    }
    Ok(parsed)
}

/// 中断路由器：进程边界（main.rs 的信号处理器）与 exec 内部共享。
/// 信号安装归属进程边界；本类型只做状态与取消，可克隆、可重复使用
/// （HL-05：库内同进程多次 headless run 不互相破坏）。
#[derive(Clone, Default)]
pub struct ExecCancel {
    inner: Arc<ExecCancelInner>,
}

#[derive(Default)]
struct ExecCancelInner {
    slot: Mutex<Option<RunHandle>>,
    handle_ready: Condvar,
    interrupted: AtomicBool,
}

/// `ExecCancel::on_interrupt` 的决策结果；进程语义（如硬退出）归调用方。
pub enum InterruptOutcome {
    /// 已请求活动 run 优雅取消。
    RunCancelled,
    /// run 句柄尚未就位：已记录中断，句柄就位后立即取消（HL-03）。
    PendingRunStart,
    /// 第二次（及以后）中断：调用方应立即以 130 退出。
    MustExit,
}

impl ExecCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn interrupted(&self) -> bool {
        self.inner.interrupted.load(Ordering::SeqCst)
    }

    /// 进程边界收到中断信号时调用。第一次：取消活动 run，或（句柄
    /// 未就位时）记录 pending；第二次起返回 `MustExit`。本方法绝不
    /// 吞掉中断（修复前的缺陷：无句柄的首次信号 cancel 一个 None 后
    /// 被静默忽略，run 照常完成并以 0 退出）。
    pub fn on_interrupt(&self) -> InterruptOutcome {
        let already = self.inner.interrupted.swap(true, Ordering::SeqCst);
        let guard = self.inner.slot.lock();
        match interrupt_action(already, guard.as_ref().is_ok_and(|g| g.is_some())) {
            InterruptAction::CancelRun => {
                if let Ok(guard) = guard
                    && let Some(handle) = guard.as_ref()
                {
                    handle.cancel();
                }
                InterruptOutcome::RunCancelled
            }
            InterruptAction::PendingRunStart => InterruptOutcome::PendingRunStart,
            InterruptAction::ExitHard => InterruptOutcome::MustExit,
        }
    }

    /// 发布 run handle，并兑现句柄就位前已经到达的 Ctrl-C。
    fn attach(&self, handle: RunHandle) {
        if let Ok(mut guard) = self.inner.slot.lock() {
            *guard = Some(handle.clone());
            self.inner.handle_ready.notify_all();
        }
        if self.interrupted() {
            handle.cancel();
        }
    }

    fn detach(&self) {
        if let Ok(mut guard) = self.inner.slot.lock() {
            *guard = None;
        }
    }

    /// stdout 写失败发生在 run worker 内。`start_run` 可能尚未把 handle
    /// 返回给调用线程；此时 sink 必须等待 handle 发布并先 cancel，再
    /// 允许 worker 继续到工具执行阶段（HL-04）。
    fn cancel_active_run_waiting_for_handle(&self) {
        let Ok(mut guard) = self.inner.slot.lock() else {
            return;
        };
        while guard.is_none() {
            let Ok(next) = self.inner.handle_ready.wait(guard) else {
                return;
            };
            guard = next;
        }
        if let Some(handle) = guard.as_ref() {
            handle.cancel();
        }
    }
}

/// Ctrl-C 决策（纯函数，供单测穷举）。
enum InterruptAction {
    CancelRun,
    PendingRunStart,
    ExitHard,
}

fn interrupt_action(already_interrupted: bool, has_handle: bool) -> InterruptAction {
    match (already_interrupted, has_handle) {
        (false, true) => InterruptAction::CancelRun,
        // 尚未起 run：记录 pending，绝不升级成硬退（HL-03：文档承诺
        // 第一次优雅取消；硬退只属于第二次信号）。
        (false, false) => InterruptAction::PendingRunStart,
        (true, _) => InterruptAction::ExitHard,
    }
}

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
        }
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
    Failure(String),
    /// 用法错误（退出码 2）。
    UsageError(String),
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
            return ExecOutcome::Failure(format!("could not determine current project: {error}"));
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
    ) -> Self {
        let join = std::thread::spawn(move || {
            while let Ok(event) = events.recv() {
                if let ApplicationEvent::CompactionUpdated(CompactionStatus::Finished {
                    note,
                    ..
                }) = event
                {
                    write_status(&error, &io_state, format_args!("● {note}\n"));
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

    let bootstrap = match storage_root {
        Some(root) => BootstrapApplication::open(project.clone(), root),
        None => BootstrapApplication::open_default(project.clone()),
    };
    let bootstrap = match bootstrap {
        Ok(bootstrap) => bootstrap,
        Err(error) => return ExecOutcome::Failure(error.to_string()),
    };
    // INV-2：未信任 → 只读检查即退出；--trust 是唯一非交互通路，经
    // 一次性 ProjectAuthorization + authorize_and_mount（无独立
    // trust 写入口，plan §16 阶段5）。
    let mut application = match bootstrap.is_trusted() {
        Ok(true) => match into_trusted(bootstrap) {
            Ok(application) => application,
            Err(error) => return ExecOutcome::Failure(error.to_string()),
        },
        Ok(false) if args.trust => {
            match bootstrap.authorize_and_mount(crate::ProjectAuthorization::grant()) {
                Ok(application) => application,
                Err(error) => return ExecOutcome::Failure(error.to_string()),
            }
        }
        Ok(false) => {
            return ExecOutcome::Failure(
                "project is not trusted — open `clat` once to review the trust prompt, \
                 or pass --trust"
                    .into(),
            );
        }
        Err(error) => return ExecOutcome::Failure(error.to_string()),
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
    if let Some(command) = args.command.clone() {
        return run_headless_command(application, &command, &args, &io, &io_state);
    }

    let (config, _credentials) = match application.model_state() {
        Ok(state) => state,
        Err(error) => {
            return close_and_finalize(
                application,
                ExecOutcome::Failure(error.to_string()),
                &args,
                &io,
                &io_state,
            );
        }
    };
    if !config.is_configured() {
        return close_and_finalize(
            application,
            ExecOutcome::Failure(
                "model is not configured — run `clat` and use /model to configure a model".into(),
            ),
            &args,
            &io,
            &io_state,
        );
    }

    // INV-6：prompt 在信任门/模型检查之后才读 stdin（无效调用不吞输入）。
    let prompt = match resolve_prompt(&args, &io, MAX_STDIN_BYTES) {
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
        ));
    }

    let mode = if args.yes {
        PermissionMode::AllowAll
    } else if io.interactive_stdin {
        PermissionMode::Interactive
    } else {
        PermissionMode::NonInteractive
    };
    let approver: Arc<dyn PermissionApprover> = Arc::new(ExecApprover {
        mode,
        input: io.permission_input.clone(),
        error: Arc::clone(&io.error),
        io_state: Arc::clone(&io_state),
        interrupt: cancel.clone(),
    });
    let sink = ExecEventSink {
        output: Arc::clone(&io.output),
        error: Arc::clone(&io.error),
        quiet: args.quiet,
        json: args.json,
        stdout_is_terminal: io.stdout_is_terminal,
        io_state: Arc::clone(&io_state),
        cancel: cancel.clone(),
    };

    let (completion_tx, completion_rx) = mpsc::channel();
    // MM-1A：typed 初始消息（headless 不接受附件——首期边界，方案
    // §范围裁定；无客户端幂等键）。
    let handle = match application.start_run(ApplicationRunRequest {
        message: crate::message::PendingMessage::text(prompt),
        asker: None,
        approver,
        events: Box::new(sink),
        completion: completion_tx,
    }) {
        Ok(handle) => handle,
        Err(error) => {
            return close_and_finalize(
                application,
                ExecOutcome::Failure(error.to_string()),
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
            ExecOutcome::Failure(error.to_string()),
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
                    format_args!("[cancelled after {} turns]\n", done.turns),
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
                        "[{} turns, {} input tokens, {} output tokens]\n",
                        done.turns, done.usage.input_tokens, done.usage.output_tokens
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
        Err(failure) => ExecOutcome::Failure(failure.error),
    };

    // HL-04：输出写失败不得伪装成功。取消（下游已关）+ 成功都改判失败；
    // 已有失败保留原错误并追加 I/O 错误。
    let io_failure = io_state.take_first_error();
    let outcome = match (io_failure, outcome) {
        (Some(error), ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. }) => {
            ExecOutcome::Failure(error)
        }
        (Some(error), ExecOutcome::Failure(message)) => {
            ExecOutcome::Failure(format!("{message}; {error}"))
        }
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
fn run_headless_command(
    mut application: TrustedProjectApplication,
    command: &str,
    args: &ExecArgs,
    io: &ExecIo,
    io_state: &SharedIoState,
) -> ExecOutcome {
    // 与 run 路径同一可见性通道：CompactionUpdated 的 stderr 通知。
    let mut forwarder = None;
    if !args.quiet {
        let (events_tx, events_rx) = mpsc::channel::<ApplicationEvent>();
        application.subscribe(events_tx);
        forwarder = Some(ApplicationEventForwarder::spawn(
            events_rx,
            Arc::clone(&io.error),
            Arc::clone(io_state),
        ));
    }
    let goal_words = command.split_whitespace().collect::<Vec<_>>();
    let interactive_goal_run = goal_words.first().copied() == Some("/goal")
        && (goal_words.get(1).copied() == Some("run")
            || (goal_words.get(1).copied() == Some("create") && goal_words.contains(&"--run")));
    let outcome = if interactive_goal_run {
        ExecOutcome::UsageError(format!(
            "{command} requires an interactive frontend — run `clat` or `clat serve`"
        ))
    } else {
        match application.dispatch_command(command) {
            Ok(CommandOutcome::StartCompaction(handle)) => match handle.join_report() {
                // 外层 Err = join 失败（worker 崩溃）；内层 Err = 压缩自身的
                // 失败报告。
                Ok(Ok(report)) => {
                    if !args.quiet {
                        write_status(
                            &io.error,
                            io_state,
                            format_args!("● {}\n", report.status_text()),
                        );
                    }
                    ExecOutcome::Success {
                        output: String::new(),
                        turns: 0,
                        usage: Usage::default(),
                    }
                }
                Ok(Err(message)) => ExecOutcome::Failure(message),
                Err(error) => ExecOutcome::Failure(error.to_string()),
            },
            Ok(CommandOutcome::StartVisionProbe(handle)) => match handle.join_report() {
                // VP-1：headless 腿同步等判定；报告一行式打印。
                Ok(report) => {
                    if !args.quiet {
                        write_status(
                            &io.error,
                            io_state,
                            format_args!("● {}\n", report.status_text()),
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
                            ExecOutcome::Failure(report.status_text())
                        }
                        crate::application::VisionProbeOutcome::Inconclusive => {
                            ExecOutcome::Failure(report.status_text())
                        }
                    }
                }
                Err(error) => ExecOutcome::Failure(error.to_string()),
            },
            Ok(CommandOutcome::ShowHelp { commands }) => {
                let mut text = String::new();
                for info in &commands {
                    let mut names = format!("/{}", info.name);
                    for alias in &info.aliases {
                        names.push_str(&format!(", /{alias}"));
                    }
                    text.push_str(&format!("{names} — {}\n", info.description));
                }
                let write = stream_write(&io.output, format_args!("{text}"))
                    .and_then(|()| stream_flush(&io.output));
                if let Err(error) = write {
                    io_state.note("stdout", error);
                }
                ExecOutcome::Success {
                    output: text,
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::ShowMcpStatus(status)) => {
                let connecting = if status.connecting > 0 {
                    format!(" · {} connecting", status.connecting)
                } else {
                    String::new()
                };
                let mut text = format!(
                    "mcp: {}/{} connected{connecting}\n",
                    status.connected, status.configured
                );
                for server in &status.servers {
                    text.push_str(&format!(
                        "● {}  {} · {} · {} tools\n",
                        server.name, server.transport, server.protocol_version, server.tools
                    ));
                }
                for failure in &status.failures {
                    text.push_str(&format!("! {failure}\n"));
                }
                let write = stream_write(&io.output, format_args!("{text}"))
                    .and_then(|()| stream_flush(&io.output));
                if let Err(error) = write {
                    io_state.note("stdout", error);
                }
                ExecOutcome::Success {
                    output: text,
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::ShowSkills(overview)) => {
                let mut text = format!("skills: {}\n", overview.entries.len());
                for entry in &overview.entries {
                    let execution = if entry.requires_execution {
                        ", requires-execution"
                    } else {
                        ""
                    };
                    text.push_str(&format!(
                        "● {}  {}{} — {}\n",
                        entry.name, entry.source, execution, entry.description
                    ));
                }
                for diagnostic in &overview.diagnostics {
                    let name = diagnostic.name.as_deref().unwrap_or("-");
                    text.push_str(&format!(
                        "! {} / {} / {}: {}\n",
                        diagnostic.source, name, diagnostic.kind, diagnostic.message
                    ));
                }
                let write = stream_write(&io.output, format_args!("{text}"))
                    .and_then(|()| stream_flush(&io.output));
                if let Err(error) = write {
                    io_state.note("stdout", error);
                }
                ExecOutcome::Success {
                    output: text,
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::ShowContext(snapshot)) => {
                let mut text = format!(
                    "context estimate ({})\nbase prompt: {}\nproject instructions: {}\nplan policy: {}\nskill catalog: {}\ngoal policy: {}\nmemory injection: {}/{} bytes\ntool schemas: {}\nhistory/compaction view: {}\noutput reserve: {}\ninput estimate: {}\ntotal estimate: {}\n",
                    snapshot.unit,
                    snapshot.base_prompt_estimate,
                    snapshot.project_instructions_estimate,
                    snapshot.plan_policy_estimate,
                    snapshot.skill_catalog_estimate,
                    snapshot.goal_policy_estimate,
                    snapshot.memory_estimate,
                    snapshot.memory_budget_bytes,
                    snapshot.tool_schemas_estimate,
                    snapshot.history_estimate,
                    snapshot.output_reserve_estimate,
                    snapshot.input_estimate,
                    snapshot.total_estimate,
                );
                text.push_str(&format!("tools: {}\n", snapshot.tool_names.join(", ")));
                text.push_str(&format!("skills: {}\n", snapshot.skill_names.join(", ")));
                if snapshot.skill_diagnostics.is_empty() {
                    text.push_str("skill diagnostics: none\n");
                } else {
                    text.push_str("skill diagnostics:\n");
                    for diagnostic in &snapshot.skill_diagnostics {
                        let name = diagnostic.name.as_deref().unwrap_or("-");
                        text.push_str(&format!(
                            "! {} / {} / {}: {}\n",
                            diagnostic.source, name, diagnostic.kind, diagnostic.message
                        ));
                    }
                }
                let write = stream_write(&io.output, format_args!("{text}"))
                    .and_then(|()| stream_flush(&io.output));
                if let Err(error) = write {
                    io_state.note("stdout", error);
                }
                ExecOutcome::Success {
                    output: text,
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::Status(message)) => {
                if !args.quiet {
                    write_status(&io.error, io_state, format_args!("{message}\n"));
                }
                ExecOutcome::Success {
                    output: String::new(),
                    turns: 0,
                    usage: Usage::default(),
                }
            }
            Ok(CommandOutcome::SessionReset) => {
                if !args.quiet {
                    write_status(&io.error, io_state, format_args!("new conversation\n"));
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
                CommandOutcome::StartModelSelection
                | CommandOutcome::StartSessionSelection { .. }
                | CommandOutcome::StartPermissionModeSelection { .. }
                | CommandOutcome::StartTitleEdit { .. }
                | CommandOutcome::StartGoalRun,
            ) => ExecOutcome::UsageError(format!(
                "{command} requires an interactive frontend — run `clat` and use it there"
            )),
            Err(error) => ExecOutcome::Failure(error.to_string()),
        }
    };
    // HL-04 同款：stdout 写失败不得伪装成功。
    let outcome = match (io_state.take_first_error(), outcome) {
        (Some(error), ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. }) => {
            ExecOutcome::Failure(error)
        }
        (Some(error), ExecOutcome::Failure(message)) => {
            ExecOutcome::Failure(format!("{message}; {error}"))
        }
        (Some(_), outcome @ ExecOutcome::UsageError(_)) => outcome,
        (None, outcome) => outcome,
    };
    let closed = close_application(application, outcome);
    if let Some(forwarder) = forwarder.as_mut() {
        forwarder.join();
    }
    closed
}

/// 观察每个退出路径上的 application.close()（审计 P2-01）：早退不再吞掉
/// flush/join 失败；已有失败时追加而不是覆盖，用户能同时看到主错误与
/// 持久化收尾错误。
fn close_application(application: TrustedProjectApplication, outcome: ExecOutcome) -> ExecOutcome {
    match application.close() {
        Ok(()) => outcome,
        Err(error) => match outcome {
            ExecOutcome::Failure(message) => {
                ExecOutcome::Failure(format!("{message}; application close failed: {error}"))
            }
            // UsageError keeps its exit-code contract, but the close
            // failure still reaches the user (never swallowed).
            ExecOutcome::UsageError(message) => {
                ExecOutcome::UsageError(format!("{message}; application close failed: {error}"))
            }
            ExecOutcome::Success { .. } | ExecOutcome::Cancelled { .. } => {
                ExecOutcome::Failure(format!("application close failed: {error}"))
            }
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
                ExecOutcome::Failure(message) => {
                    ExecOutcome::Failure(format!("{message}; stdout write failed"))
                }
                ExecOutcome::UsageError(message) => {
                    ExecOutcome::UsageError(format!("{message}; stdout write failed"))
                }
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
            return Err(ExecOutcome::UsageError("prompt must not be empty".into()));
        }
        instruction => instruction,
    };
    if io.interactive_stdin {
        return match instruction {
            Some(instruction) => Ok(instruction.to_string()),
            None => Err(ExecOutcome::UsageError(
                "missing prompt — pass it as an argument or pipe it on stdin".into(),
            )),
        };
    }
    let piped = read_stdin_budgeted(io, limit)?;
    if piped.trim().is_empty() {
        return match instruction {
            Some(instruction) => Ok(instruction.to_string()),
            None => Err(ExecOutcome::UsageError(
                "no prompt provided on stdin".into(),
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
        reader
            .read_to_string(&mut buffer)
            .map_err(|error| ExecOutcome::Failure(format!("could not read piped input: {error}")))?
    };
    if count > limit {
        return Err(ExecOutcome::UsageError(format!(
            "piped input exceeds the {limit}-byte limit; trim the input or summarize it first"
        )));
    }
    Ok(buffer)
}

fn apply_session_policy(
    application: &mut TrustedProjectApplication,
    args: &ExecArgs,
) -> Result<(), String> {
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
                        format!("could not switch to session {display_id}: {error}")
                    })
            }
            None => Err("no session to continue".into()),
        };
    }
    if let Some(id) = args.session.clone() {
        let session_id = crate::SessionId::new(id.clone());
        return application
            .switch_session(session_id)
            .map(|_| ())
            .map_err(|error| format!("could not switch to session {id}: {error}"));
    }
    application
        .new_session()
        .map_err(|error| format!("could not start a new session: {error}"))
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
    mode: PermissionMode,
    input: Option<Arc<dyn ExecPermissionInput>>,
    error: SharedWriter,
    io_state: SharedIoState,
    interrupt: ExecCancel,
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
            PermissionMode::Interactive => {
                write_status(
                    &self.error,
                    &self.io_state,
                    format_args!(
                        "permission requested: {} ({:?}) — {}\n",
                        request.tool, request.effect, request.reason
                    ),
                );
                write_status(
                    &self.error,
                    &self.io_state,
                    format_args!("arguments: {}\n", request.arguments),
                );
                write_status(&self.error, &self.io_state, format_args!("allow? [y/N] "));
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
    output: SharedWriter,
    error: SharedWriter,
    quiet: bool,
    json: bool,
    stdout_is_terminal: bool,
    io_state: SharedIoState,
    cancel: ExecCancel,
}

impl EventSink for ExecEventSink {
    fn emit(&mut self, event: RunEvent) {
        if self.json {
            // FP-10 由 wire 层承担：serde 转义 C0 + 补转义 DEL，结构性
            // 字节全为可打印 ASCII，无需 TTY 消毒（INV-J3）。
            let line = crate::wire::envelope_line(&event);
            let result = stream_write(&self.output, format_args!("{line}"))
                .and_then(|()| stream_flush(&self.output));
            if let Err(error) = result {
                self.io_state.note("stdout", error);
                self.cancel.cancel_active_run_waiting_for_handle();
            }
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
                // FP-10（双契约）：TTY 显示路径对 C0/DEL 做可见转义——
                // 远端诱导的 OSC 52（剪贴板改写）/OSC 8/CSI 序列失去
                // 效力；管道路径字节原样（assistant-text 契约不破）。
                let text = if self.stdout_is_terminal {
                    sanitize_tty_text(&delta)
                } else {
                    delta
                };
                let result = stream_write(&self.output, format_args!("{text}"))
                    .and_then(|()| stream_flush(&self.output));
                if let Err(error) = result {
                    self.io_state.note("stdout", error);
                    self.cancel.cancel_active_run_waiting_for_handle();
                }
            }
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => {
                if !self.quiet {
                    write_status(
                        &self.error,
                        &self.io_state,
                        format_args!("● {provider}/{model} turn {turn}\n"),
                    );
                }
            }
            RunEvent::ToolStarted { tool, .. } => {
                if !self.quiet {
                    write_status(&self.error, &self.io_state, format_args!("● tool {tool}\n"));
                }
            }
            RunEvent::PermissionDenied { tool, reason } => {
                if !self.quiet {
                    write_status(
                        &self.error,
                        &self.io_state,
                        format_args!("● permission denied {tool}: {reason}\n"),
                    );
                }
            }
            RunEvent::ToolFinished { result } if !self.quiet => {
                let mark = if result.is_error { "✗" } else { "✓" };
                write_status(
                    &self.error,
                    &self.io_state,
                    format_args!("● tool {mark} {}\n", result.tool_name),
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestBehavior, TestProviderPlugin, roots};
    use crate::{BootstrapApplication, ToolEffect};
    use std::fs;
    use std::time::Instant;

    fn args(prompt: Option<&str>) -> ExecArgs {
        ExecArgs {
            prompt: prompt.map(str::to_string),
            ..ExecArgs::default()
        }
    }

    /// 信任临时项目并预配置脚本模型，使后续 exec 注入的 provider
    /// 能命中已配置的 model_state。
    fn prepare_storage(project: &Project, storage_root: &std::path::Path, behavior: TestBehavior) {
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
        let application = bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
            .unwrap();
        crate::test_support::configure_test_model(&application);
        application.close().unwrap();
    }

    fn exec(
        project: &Project,
        storage_root: &std::path::Path,
        behavior: TestBehavior,
        args: ExecArgs,
        io: ExecIo,
    ) -> ExecOutcome {
        exec_with_cancel(
            project,
            storage_root,
            behavior,
            args,
            io,
            &ExecCancel::new(),
        )
    }

    fn exec_with_cancel(
        project: &Project,
        storage_root: &std::path::Path,
        behavior: TestBehavior,
        args: ExecArgs,
        io: ExecIo,
        cancel: &ExecCancel,
    ) -> ExecOutcome {
        exec_with(
            project.clone(),
            Some(storage_root.to_path_buf()),
            args,
            io,
            |bootstrap| {
                bootstrap
                    .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin { behavior }))
            },
            cancel,
        )
    }

    fn setup(name: &str) -> (PathBuf, PathBuf, Project) {
        let (storage_root, project_root) = roots(name);
        fs::create_dir_all(&project_root).expect("project");
        let project = Project::new(&project_root);
        (storage_root, project_root, project)
    }

    /// 重开应用读取最近会话的首条 user 消息（持久化断言用）。
    fn persisted_user_message(
        project: &Project,
        storage_root: &std::path::Path,
    ) -> (crate::SessionId, String) {
        let bootstrap =
            BootstrapApplication::open(project.clone(), storage_root.to_path_buf()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        let id = application.current_session_id().expect("session");
        let transcript = application.snapshot().unwrap().transcript;
        application.close().unwrap();
        let first_user = transcript
            .iter()
            .find(|line| line.kind == "user")
            .expect("a user line");
        (id, first_user.text.clone())
    }

    // ---- 参数解析 ----

    #[test]
    fn parse_accepts_flags_and_prompt() {
        let parsed = parse_exec_args([
            "--quiet".to_string(),
            "do work".to_string(),
            "--yes".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("do work"));
        assert!(parsed.quiet);
        assert!(parsed.yes);
        assert!(!parsed.trust);
        assert!(!parsed.continue_session);
        assert_eq!(parsed.session, None);
    }

    #[test]
    fn parse_rejects_unknown_flag_extra_argument_and_bad_session() {
        assert!(parse_exec_args(["--yolo".into()]).is_err());
        assert!(parse_exec_args(["a".into(), "b".into()]).is_err());
        assert!(parse_exec_args(["--session".into()]).is_err());
        assert!(parse_exec_args(["--session".into(), " ".into()]).is_err());
        assert!(parse_exec_args(["--continue".into(), "--session".into(), "1".into()]).is_err());
    }

    #[test]
    fn parse_session_id_is_an_opaque_string() {
        let parsed = parse_exec_args(["--session".into(), "0f8c2a4e-uuid-like-id".into()]).unwrap();
        assert_eq!(parsed.session.as_deref(), Some("0f8c2a4e-uuid-like-id"));
    }

    #[test]
    fn double_dash_makes_remaining_tokens_positional() {
        let parsed = parse_exec_args(["--".into(), "--yes".into()]).unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("--yes"));
        assert!(!parsed.yes, "token after -- must not be parsed as a flag");
        let parsed = parse_exec_args(["--quiet".into(), "--".into(), "-x".into()]).unwrap();
        assert!(parsed.quiet);
        assert_eq!(parsed.prompt.as_deref(), Some("-x"));
        // `--` 之后仍只允许一个位置参数。
        assert!(
            parse_exec_args(["--".into(), "a".into(), "b".into()]).is_err(),
            "second positional after -- must still be a usage error"
        );
    }

    // ---- --command（core.commands 注册表的 headless 形态）----

    #[test]
    fn parse_accepts_command_flag_and_rejects_misuse() {
        let parsed = parse_exec_args([
            "--continue".to_string(),
            "--command".to_string(),
            "/compact".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.command.as_deref(), Some("/compact"));
        assert!(parsed.continue_session);
        assert_eq!(parsed.prompt, None);
        // 值必须以 `/` 起头。
        assert!(parse_exec_args(["--command".into(), "compact".into()]).is_err());
        // 缺值。
        assert!(parse_exec_args(["--command".into()]).is_err());
        // 与位置参数互斥。
        assert!(parse_exec_args(["--command".into(), "/quit".into(), "prompt".into()]).is_err());
    }

    /// /help 的 stdout 是本次调用的产品（无模型流，不违反 INV-3 的
    /// 混流关切）；目录来自 core.commands 注册表。
    #[test]
    fn command_help_lists_registry_catalog_on_stdout() {
        let (storage_root, project_root, project) = setup("exec-command-help");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut options = args(None);
        options.command = Some("/help".into());
        let (io, captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        match outcome {
            ExecOutcome::Success { output, turns, .. } => {
                assert_eq!(turns, 0);
                assert!(
                    output.contains("/model — configure the active model/provider"),
                    "{output}"
                );
                assert_eq!(captured.output_string(), output);
            }
            other => panic!("expected success, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// SC-2 双入口等价：`--command /skill` 在 headless 输出列表文本（与
    /// TUI 弹窗同一 `ShowSkills` DTO 的另一呈现）；`/skill <name>` 武装
    /// 经 Status 提示确认。快照含 bundled 层标记与 grill-me（SC-1）。
    #[test]
    fn command_skill_lists_catalog_and_arms_via_status_headless() {
        let (storage_root, project_root, project) = setup("exec-command-skill");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut options = args(None);
        options.command = Some("/skill".into());
        let (io, captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        match outcome {
            ExecOutcome::Success { output, turns, .. } => {
                assert_eq!(turns, 0);
                assert!(output.contains("● grill-me  bundled"), "{output}");
                assert!(output.contains("● code-review  bundled"), "{output}");
                assert_eq!(captured.output_string(), output);
            }
            other => panic!("expected success, got {other:?}"),
        }
        let mut options = args(None);
        options.command = Some("/skill grill-me".into());
        let (io, _captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        match outcome {
            ExecOutcome::Success { .. } => {}
            other => panic!("arming via /skill <name> must succeed headless, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// 失败路径：默认会话策略是新会话（懒物化、无活动 id），/compact
    /// 必须以 core 的结构化错误干净失败（不 panic、不落任何日志）。
    #[test]
    fn command_compact_on_empty_session_fails_cleanly() {
        let (storage_root, project_root, project) = setup("exec-command-compact-empty");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut options = args(None);
        options.command = Some("/compact".into());
        let (io, _captured) = ExecIo::capture(&[]);
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        match outcome {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("no conversation to compact"), "{message}");
            }
            other => panic!("expected failure, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// 交互类命令在 headless 无呈现：UsageError（退出码 2，脚本可判定）。
    ///（/rename 不在此列：它先过会话门控，空会话以 `Failed` 干净失败。）
    #[test]
    fn command_interactive_selections_are_usage_errors_headless() {
        let (storage_root, project_root, project) = setup("exec-command-interactive");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        for command in ["/model", "/resume", "/perm"] {
            let mut options = args(None);
            options.command = Some(command.into());
            let (io, _captured) = ExecIo::capture(&[]);
            let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
            match outcome {
                ExecOutcome::UsageError(message) => {
                    assert!(message.contains("interactive"), "{command}: {message}");
                }
                other => panic!("{command}: expected usage error, got {other:?}"),
            }
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn headless_goal_continuation_rejection_commits_no_goal_mutation() {
        let (storage_root, project_root, project) = setup("exec-goal-run-no-mutation");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, _) = ExecIo::capture(&[]);
        assert!(matches!(
            exec(
                &project,
                &storage_root,
                TestBehavior::Success,
                args(Some("seed session")),
                io,
            ),
            ExecOutcome::Success { .. }
        ));

        let mut options = args(None);
        options.continue_session = true;
        options.command = Some("/goal create must-not-commit --run".into());
        let (io, _) = ExecIo::capture(&[]);
        assert!(matches!(
            exec(&project, &storage_root, TestBehavior::Success, options, io,),
            ExecOutcome::UsageError(_)
        ));

        let application = BootstrapApplication::open(project.clone(), storage_root.clone())
            .unwrap()
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        assert!(application.current_session_id().is_some());
        assert!(application.goal().unwrap().is_none());
        application.close().unwrap();
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    // ---- 中断决策（HL-03：pending 而非硬退；中断永不吞掉）----

    #[test]
    fn interrupt_dispatch_covers_all_combinations() {
        // 首次 + 有句柄 → 优雅取消。
        assert!(matches!(
            interrupt_action(false, true),
            InterruptAction::CancelRun
        ));
        // 首次 + 无句柄 → pending（句柄就位后立即取消），绝不硬退
        // （修复前两版分别实现成"吞掉"和"硬退"，都与文档契约冲突）。
        assert!(matches!(
            interrupt_action(false, false),
            InterruptAction::PendingRunStart
        ));
        // 第二次 → 硬退。
        assert!(matches!(
            interrupt_action(true, true),
            InterruptAction::ExitHard
        ));
        assert!(matches!(
            interrupt_action(true, false),
            InterruptAction::ExitHard
        ));
    }

    #[test]
    fn pending_interrupt_cancels_the_run_as_soon_as_its_handle_exists() {
        let (storage_root, project_root, project) = setup("exec-pending-interrupt");
        prepare_storage(&project, &storage_root, TestBehavior::Cancel);
        let cancel = ExecCancel::new();
        // 第一次中断发生在句柄就位前：记录 pending，不退出进程。
        assert!(matches!(
            cancel.on_interrupt(),
            InterruptOutcome::PendingRunStart
        ));
        let (io, _) = ExecIo::capture(b"");
        let outcome = exec_with_cancel(
            &project,
            &storage_root,
            TestBehavior::Cancel,
            args(Some("slow work")),
            io,
            &cancel,
        );
        // Cancel 行为在 token 置位前不会返回；pending 取消必须生效。
        assert!(
            matches!(outcome, ExecOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn second_interrupt_reports_must_exit_without_exiting_the_process() {
        let cancel = ExecCancel::new();
        let _ = cancel.on_interrupt();
        assert!(matches!(cancel.on_interrupt(), InterruptOutcome::MustExit));
        // 库不做进程决策：第三次调用仍返回 MustExit，由宿主决定退出。
        assert!(matches!(cancel.on_interrupt(), InterruptOutcome::MustExit));
    }

    // ---- stderr 不可被终端转义注入（依赖 serde_json 转义，钉住不变量）----

    /// FP-10（双契约）：同一条 delta 含 OSC 52（剪贴板改写序列）——
    /// TTY 旗标开 → 输出无裸 ESC、含可见转义；管道（默认）→ 字节
    /// 完全一致。删除 sanitize 调用 → TTY 腿红（判别力）。
    #[test]
    fn tty_deltas_escape_control_sequences_pipes_stay_verbatim() {
        let hostile = "before\u{1b}]52;c;aGVsbG8=\u{07}after\n";
        assert_eq!(
            sanitize_tty_text(hostile),
            "before\\x1b]52;c;aGVsbG8=\\x07after\n",
            "the escape introducer is defused visibly"
        );
        assert_eq!(sanitize_tty_text("plain text\ttab\n"), "plain text\ttab\n");
        assert_eq!(sanitize_tty_text("中文保持不变"), "中文保持不变");

        // sink 级双契约：同 delta 两种旗标。
        fn sink_with(flag: bool) -> (ExecEventSink, CapturedOutput) {
            let output = Arc::new(Mutex::new(Vec::new()));
            let error = Arc::new(Mutex::new(Vec::new()));
            let sink = ExecEventSink {
                output: Arc::new(Mutex::new(
                    Box::new(CapturedWriter(Arc::clone(&output))) as Box<dyn Write + Send>
                )),
                error: Arc::new(Mutex::new(
                    Box::new(CapturedWriter(Arc::clone(&error))) as Box<dyn Write + Send>
                )),
                quiet: false,
                json: false,
                stdout_is_terminal: flag,
                io_state: Arc::new(ExecIoState::default()),
                cancel: ExecCancel::new(),
            };
            (sink, CapturedOutput { output, error })
        }
        let (mut tty_sink, tty_captured) = sink_with(true);
        tty_sink.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: hostile.into(),
            },
        });
        let tty_out = tty_captured.output_string();
        assert!(
            !tty_out.contains('\u{1b}'),
            "no raw ESC reaches the TTY: {tty_out:?}"
        );
        assert!(
            tty_out.contains("\\x1b"),
            "visible escape present: {tty_out:?}"
        );

        let (mut pipe_sink, pipe_captured) = sink_with(false);
        pipe_sink.emit(RunEvent::ModelStream {
            turn: 1,
            event: ModelEvent::TextDelta {
                delta: hostile.into(),
            },
        });
        assert_eq!(
            pipe_captured.output_string(),
            hostile,
            "pipe output stays byte-faithful"
        );
    }

    // ---- --json：NDJSON 事件流（PWA-1，INV-J1…J6）----

    fn json_args(prompt: Option<&str>) -> ExecArgs {
        ExecArgs {
            prompt: prompt.map(str::to_string),
            json: true,
            ..ExecArgs::default()
        }
    }

    /// INV-J1 断言基础：stdout 必须整体是可逐行解析的 NDJSON（任何一行
    /// 坏、任何 JSON 外字节都在这里红）。
    fn parse_json_stream(stdout: &str) -> Vec<crate::wire::WireEvent> {
        stdout
            .lines()
            .map(|line| {
                crate::wire::parse_envelope_line(line)
                    .unwrap_or_else(|error| panic!("invalid NDJSON line ({error:?}): {line}"))
            })
            .collect()
    }

    fn json_type_tags(events: &[crate::wire::WireEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(crate::wire::wire_event_type_tag)
            .collect()
    }

    /// Run 层终态计数；exec 层终态另行断言——两层不能混算（PWA1-01：
    /// run 终态 ≠ invocation 终态）。
    fn run_terminal_count(events: &[crate::wire::WireEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    crate::wire::WireEvent::Run(
                        RunEvent::RunCompleted { .. }
                            | RunEvent::RunCancelled { .. }
                            | RunEvent::RunFailed { .. },
                    )
                )
            })
            .count()
    }

    fn exec_final_count(events: &[crate::wire::WireEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    crate::wire::WireEvent::ExecCompleted { .. }
                        | crate::wire::WireEvent::ExecFailed { .. }
                )
            })
            .count()
    }

    /// PWA1-01 核心：流最后一行必须是 exec 终态，其 exit_code 与最终
    /// outcome 的退出语义一致。旧实现（无 exec 终态行）在这里 panic
    /// （判别力）。
    fn last_line_exit_code(stdout: &str) -> u64 {
        match parse_json_stream(stdout).last() {
            Some(crate::wire::WireEvent::ExecCompleted { exit_code }) => *exit_code,
            Some(crate::wire::WireEvent::ExecFailed { exit_code, .. }) => *exit_code,
            other => panic!("stream must end with an exec final, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_flag_and_its_command_conflict() {
        let parsed = parse_exec_args(["--json".into(), "hi".into()]).unwrap();
        assert!(parsed.json);
        assert!(!parse_exec_args(["hi".into()]).unwrap().json);
        assert_eq!(
            parse_exec_args(["--json".into(), "--command".into(), "/help".into()]).unwrap_err(),
            "--json cannot be combined with --command"
        );
    }

    /// INV-J1 + 验收①：`--json` 下 stdout 单一契约是 NDJSON——无 JSON 外
    /// 字节、发射序即行序、助手文本只在事件载荷里；stderr 状态面原样。
    /// 判别力：sink 不走 JSON 分支（或仍写纯文本 delta）→ 行解析红。
    #[test]
    fn json_stdout_is_pure_ndjson_and_keeps_stderr_status() {
        let (storage_root, project_root, project) = setup("exec-json-purity");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            json_args(Some("hi")),
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let stdout = captured.output_string();
        assert!(stdout.ends_with('\n'), "stream ends newline-terminated");
        for line in stdout.lines() {
            assert!(
                line.starts_with(r#"{"v":1,"event":{"type":""#),
                "no bytes outside the NDJSON contract: {line:?}"
            );
        }
        let events = parse_json_stream(&stdout);
        let tags = json_type_tags(&events);
        assert_eq!(tags.first(), Some(&"run_started"), "{tags:?}");
        // 助手文本在事件载荷里，不再以纯文本落 stdout。
        let streamed_text: String = events
            .iter()
            .filter_map(|event| match event {
                crate::wire::WireEvent::Run(RunEvent::ModelStream {
                    event: ModelEvent::TextDelta { delta },
                    ..
                }) => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed_text, "done");
        assert!(!stdout.starts_with("done"), "no plain-text prefix");
        // PWA1-01：流以 invocation 终态收尾（exit 0），run 终态在其前。
        assert_eq!(exec_final_count(&events), 1, "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
        assert_eq!(tags[tags.len() - 2], "run_completed", "{tags:?}");
        assert_eq!(last_line_exit_code(&stdout), 0);
        // stderr 状态面不变（未传 --quiet）。
        let stderr = captured.error_string();
        assert!(stderr.contains("deterministic"), "{stderr}");
        assert!(stderr.contains("turn"), "{stderr}");
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// INV-J4 + 验收④ + PWA1-01：两层各自恰一终态——Run 层
    /// （run_completed/run_cancelled）恰一、exec 层（exec_completed/
    /// exec_failed）恰一且恒为最后一行、其 exit_code 与进程退出语义
    /// 一致。消费者不靠 EOF 猜完成，也不把 run 终态当进程结果。
    #[test]
    fn json_stream_self_terminates_with_exactly_one_terminal_event() {
        let (storage_root, project_root, project) = setup("exec-json-terminal-success");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            json_args(Some("hi")),
            io,
        );
        assert!(matches!(outcome, ExecOutcome::Success { .. }));
        let events = parse_json_stream(&captured.output_string());
        let tags = json_type_tags(&events);
        assert_eq!(run_terminal_count(&events), 1, "{tags:?}");
        assert_eq!(exec_final_count(&events), 1, "{tags:?}");
        assert_eq!(tags[tags.len() - 2], "run_completed", "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
        assert_eq!(last_line_exit_code(&captured.output_string()), 0);
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();

        // 取消腿：pending 中断在句柄就位后立即取消（复用 Cancel 行为）。
        // run 层 run_cancelled；invocation 层 exec_failed 携 exit 130——
        // 用户中断对 CI 是非零退出，不是成功。
        let (storage_root, project_root, project) = setup("exec-json-terminal-cancel");
        prepare_storage(&project, &storage_root, TestBehavior::Cancel);
        let cancel = ExecCancel::new();
        assert!(matches!(
            cancel.on_interrupt(),
            InterruptOutcome::PendingRunStart
        ));
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec_with_cancel(
            &project,
            &storage_root,
            TestBehavior::Cancel,
            json_args(Some("slow work")),
            io,
            &cancel,
        );
        assert!(
            matches!(outcome, ExecOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        let events = parse_json_stream(&captured.output_string());
        let tags = json_type_tags(&events);
        assert_eq!(run_terminal_count(&events), 1, "{tags:?}");
        assert_eq!(exec_final_count(&events), 1, "{tags:?}");
        assert_eq!(tags[tags.len() - 2], "run_cancelled", "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
        assert_eq!(last_line_exit_code(&captured.output_string()), 130);
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// INV-J5 + 验收⑤：权限不混流——决策是事件，提示照旧走
    /// stderr+stdin；管道 NonInteractive 拒绝与 `--yes` 放行两腿的
    /// 事件面与退出语义都锁定。permission-request 事件留给 Phase 2。
    #[test]
    fn json_stream_carries_the_permission_surface_without_prompt_events() {
        // 管道默认：NonInteractive → Unavailable 拒绝，工具报结构化错误，
        // run 仍完成（退出语义与无 --json 时一致）。
        let (storage_root, project_root, project) = setup("exec-json-perm-deny");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            json_args(Some("write")),
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let events = parse_json_stream(&captured.output_string());
        let tags = json_type_tags(&events);
        // run 完成（拒绝是结构化工具错误，agent 继续到完成）→ invocation
        // 成功：最后一行 exec_completed、exit 0。两层语义在此分野。
        assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
        assert_eq!(last_line_exit_code(&captured.output_string()), 0);
        let decision = events
            .iter()
            .find_map(|event| match event {
                crate::wire::WireEvent::Run(RunEvent::PermissionChecked { decision, .. }) => {
                    Some(decision.clone())
                }
                _ => None,
            })
            .expect("permission_checked event");
        assert!(
            matches!(decision, PermissionDecision::Unavailable { .. }),
            "{decision:?}"
        );
        assert!(tags.contains(&"permission_denied"), "{tags:?}");
        // 被拒调用从不执行：RunEvent 流里没有 tool_started/tool_finished
        //（拒绝即 continue；error-only tool/result 是 journal 侧的映射，
        // 不在 RunEvent 词汇里）。工具结果不存在正是「未执行」的证据。
        assert!(
            !tags.contains(&"tool_started") && !tags.contains(&"tool_finished"),
            "{tags:?}"
        );
        assert!(
            !project_root.join("generated.txt").exists(),
            "denied write_file must not touch the project"
        );
        // stderr 状态面照旧承载拒绝提示。
        assert!(
            captured.error_string().contains("permission denied"),
            "denial must stay visible on stderr"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();

        // --yes 腿：Allow，工具执行，文件落地。
        let (storage_root, project_root, project) = setup("exec-json-perm-yes");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        let mut options = json_args(Some("write"));
        options.yes = true;
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            options,
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let events = parse_json_stream(&captured.output_string());
        let tags = json_type_tags(&events);
        let decision = events
            .iter()
            .find_map(|event| match event {
                crate::wire::WireEvent::Run(RunEvent::PermissionChecked { decision, .. }) => {
                    Some(decision.clone())
                }
                _ => None,
            })
            .expect("permission_checked event");
        assert!(
            matches!(decision, PermissionDecision::Allow),
            "{decision:?}"
        );
        assert!(tags.contains(&"tool_started"), "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_completed"), "{tags:?}");
        let written = fs::read_to_string(project_root.join("generated.txt")).unwrap();
        assert_eq!(written, "from headless test");
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// INV-J6：`--json` 下 stdout 写失败（断管）仍必须取消 run 并以失败
    /// 退出，绝不伪装成功。判别力：JSON 分支不接 io_state/cancel 管道
    /// → Success + 吞错误 → 红。
    #[test]
    fn json_broken_pipe_fails_the_run() {
        let (storage_root, project_root, project) = setup("exec-json-broken-pipe");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let error_capture = Arc::new(Mutex::new(Vec::new()));
        let io = ExecIo::new(
            Box::new(std::io::Cursor::new(Vec::new())),
            Box::new(BrokenPipeWriter),
            Box::new(CapturedWriter(Arc::clone(&error_capture))),
            false,
        );
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            json_args(Some("hi")),
            io,
        );
        match outcome {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("stdout"), "{message}");
            }
            other => panic!("expected failure, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    /// PWA1-01（审计 §3.6）：invocation 终态行与进程结果不得矛盾——
    /// 三条失败向的真实腿。修复前（无 exec 终态行）本测试红：流以
    /// run_failed 甚至无终态收尾，消费者无从得知进程将非零退出。
    #[test]
    fn json_final_line_never_contradicts_the_process_result() {
        // 腿 1：模型失败 → run_failed（Run 层）+ exec_failed exit 1。
        let (storage_root, project_root, project) = setup("exec-json-final-failure");
        prepare_storage(&project, &storage_root, TestBehavior::Failure);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Failure,
            json_args(Some("hi")),
            io,
        );
        assert!(matches!(outcome, ExecOutcome::Failure(_)), "{outcome:?}");
        let stdout = captured.output_string();
        let events = parse_json_stream(&stdout);
        let tags = json_type_tags(&events);
        assert!(tags.contains(&"run_failed"), "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
        assert_eq!(last_line_exit_code(&stdout), 1);
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();

        // 腿 2：run worker 在流中途崩溃（部分 delta 已出、无 Run 终态）
        // ——exec 终态仍收尾整流，exit 1。这正是「Run 终态 ≠ invocation
        // 终态」必须分层的极端证据：任何 Run 终态缺失/异常都不该让
        // 消费者失去权威结果。
        let (storage_root, project_root, project) = setup("exec-json-final-panic");
        prepare_storage(&project, &storage_root, TestBehavior::Panic);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Panic,
            json_args(Some("hi")),
            io,
        );
        assert!(matches!(outcome, ExecOutcome::Failure(_)), "{outcome:?}");
        let stdout = captured.output_string();
        let events = parse_json_stream(&stdout);
        let tags = json_type_tags(&events);
        assert!(
            stdout.contains("partial-panic"),
            "stream carried the partial delta: {stdout:?}"
        );
        assert_eq!(run_terminal_count(&events), 0, "{tags:?}");
        assert_eq!(exec_final_count(&events), 1, "{tags:?}");
        assert_eq!(tags.last(), Some(&"exec_failed"), "{tags:?}");
        assert_eq!(last_line_exit_code(&stdout), 1);
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();

        // 腿 3：用法错误（管道空、无 prompt）→ 未起 run，stdout 恰一行
        // exec_failed exit 2，退出码契约不变。
        let (storage_root, project_root, project) = setup("exec-json-final-usage");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            json_args(None),
            io,
        );
        assert!(matches!(outcome, ExecOutcome::UsageError(_)), "{outcome:?}");
        let stdout = captured.output_string();
        let events = parse_json_stream(&stdout);
        assert_eq!(
            events.len(),
            1,
            "no run started, one final line: {stdout:?}"
        );
        assert_eq!(last_line_exit_code(&stdout), 2);
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn permission_arguments_render_escaped_so_ansi_cannot_reach_the_terminal() {
        let hostile = serde_json::json!({"content": "\u{1b}[31mfake prompt\u{1b}[0m"});
        let rendered = hostile.to_string();
        assert!(
            !rendered.contains('\u{1b}'),
            "raw ESC must never be printed"
        );
        assert!(
            rendered.contains("\\u001b"),
            "expected JSON escaping: {rendered}"
        );
    }

    // ---- 信任门（INV-2）----

    #[test]
    fn untrusted_project_fails_closed_without_trust_flag() {
        let (storage_root, project_root, project) = setup("exec-untrusted");
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("hi")),
            io,
        );
        match &outcome {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("--trust"), "message: {message}");
            }
            other => panic!("expected failure, got {other:?}"),
        }
        // 信任门失败 → 没有会话被创建。
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let application = bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        assert!(application.current_session_id().is_none());
        application.close().unwrap();
        assert!(captured.output_string().is_empty());
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn trust_flag_admits_first_run() {
        let (storage_root, project_root, project) = setup("exec-trust-flag");
        // 预置信任 + 模型，再撤销信任：--trust 必须走真正的重信任路径。
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        // 撤销信任经控制面 remove_trust；TrustedProjectApplication 不再
        // 暴露写入口（untrust 是 Ready 控制面上的独立命令）。
        drop(bootstrap);
        let mut options = args(Some("hi"));
        options.trust = true;
        let (io, _captured) = ExecIo::capture(b"");
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    // ---- stdout 纯净（INV-3）与成功路径 ----

    #[test]
    fn stdout_carries_only_assistant_text() {
        let (storage_root, project_root, project) = setup("exec-stdout-purity");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("hi")),
            io,
        );
        let ExecOutcome::Success { output, turns, .. } = outcome else {
            panic!("expected success");
        };
        assert_eq!(output, "done");
        assert_eq!(turns, 1);
        // INV-3：stdout 只有模型文本（含结尾换行）；状态行全在 stderr。
        assert_eq!(captured.output_string(), "done\n");
        let error = captured.error_string();
        assert!(error.contains("turn"), "status line missing: {error}");
        assert!(error.contains("input tokens"), "summary missing: {error}");
        assert!(!error.contains("done"), "assistant text leaked to stderr");
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn quiet_suppresses_status_but_not_output() {
        let (storage_root, project_root, project) = setup("exec-quiet");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut options = args(Some("hi"));
        options.quiet = true;
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        assert!(matches!(outcome, ExecOutcome::Success { .. }));
        assert_eq!(captured.output_string(), "done\n");
        assert!(
            captured.error_string().trim().is_empty(),
            "quiet must suppress stderr chatter"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    // ---- I/O 写失败不得伪装成功（HL-04）----

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "consumer closed the pipe",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "consumer closed the pipe",
            ))
        }
    }

    #[test]
    fn stdout_write_failure_fails_the_run_instead_of_reporting_success() {
        let (storage_root, project_root, project) = setup("exec-broken-pipe");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let error_capture = Arc::new(Mutex::new(Vec::new()));
        let io = ExecIo::new(
            Box::new(std::io::Cursor::new(Vec::new())),
            Box::new(BrokenPipeWriter),
            Box::new(CapturedWriter(Arc::clone(&error_capture))),
            false,
        );
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("hi")),
            io,
        );
        // 修复前：run 成功 + 写错误被吞 → Success + 退出码 0。
        match outcome {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("stdout"), "{message}");
                assert!(
                    message.contains("BrokenPipe") || message.contains("pipe"),
                    "{message}"
                );
            }
            other => panic!("expected failure, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn stdout_failure_before_handle_publication_cancels_before_side_effects() {
        let (storage_root, project_root, project) = setup("exec-early-broken-pipe");
        prepare_storage(&project, &storage_root, TestBehavior::DeltaThenWrite);
        let error_capture = Arc::new(Mutex::new(Vec::new()));
        let io = ExecIo::new(
            Box::new(std::io::Cursor::new(Vec::new())),
            Box::new(BrokenPipeWriter),
            Box::new(CapturedWriter(Arc::clone(&error_capture))),
            false,
        );
        let mut options = args(Some("write after streaming"));
        options.yes = true;
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::DeltaThenWrite,
            options,
            io,
        );
        assert!(
            matches!(&outcome, ExecOutcome::Failure(message) if message.contains("stdout")),
            "{outcome:?}"
        );
        assert!(
            !project_root.join("generated.txt").exists(),
            "broken stdout must cancel before a later --yes tool executes"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn application_event_forwarder_drop_drains_queued_events_and_joins() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer: SharedWriter =
            Arc::new(Mutex::new(Box::new(CapturedWriter(Arc::clone(&output)))));
        let state = Arc::new(ExecIoState::default());
        let (sender, receiver) = mpsc::channel();
        let forwarder = ApplicationEventForwarder::spawn(receiver, writer, state);
        sender
            .send(ApplicationEvent::CompactionUpdated(
                CompactionStatus::Finished {
                    note: "queued tail".into(),
                    succeeded: true,
                },
            ))
            .unwrap();
        drop(sender);
        // Drop 是错误早退路径的兜底：必须等待 receiver 排空后才返回。
        drop(forwarder);
        assert_eq!(
            String::from_utf8_lossy(&output.lock().unwrap()),
            "● queued tail\n"
        );
    }

    // ---- stdin 双输入协议（INV-6 / HL-01）与预算（HL-06）----

    #[test]
    fn positional_instruction_and_piped_stdin_are_combined_into_the_prompt() {
        let (storage_root, project_root, project) = setup("exec-dual-input");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        // 文档 canonical 用法：位置指令 + 管道上下文。
        let (io, _) = ExecIo::capture(b"UNIQUE_DIFF_SENTINEL\n");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("review this diff")),
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let (_, message) = persisted_user_message(&project, &storage_root);
        assert!(
            message.contains("review this diff") && message.contains("UNIQUE_DIFF_SENTINEL"),
            "instruction and piped context must both reach the model: {message}"
        );
        assert!(
            message.contains("piped input follows"),
            "context must carry an explicit boundary: {message}"
        );

        // 空管道 + 指令 → 指令原样，无上下文块（续接同一会话，
        // 断言最后一条 user 消息）。
        let mut options = args(Some("plain instruction"));
        options.continue_session = true;
        let (io, _) = ExecIo::capture(b"");
        let outcome = exec(&project, &storage_root, TestBehavior::Success, options, io);
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        let transcript = application.snapshot().unwrap().transcript;
        application.close().unwrap();
        let user_lines: Vec<&str> = transcript
            .iter()
            .filter(|line| line.kind == "user")
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(user_lines.len(), 2, "second run must append to the session");
        assert!(user_lines[1].contains("plain instruction"));
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn stdin_budget_is_enforced_at_the_byte_boundary() {
        // 恰好等于预算 → 通过。
        let (io, _) = ExecIo::capture(b"abcdef");
        assert_eq!(resolve_prompt(&args(None), &io, 6).unwrap(), "abcdef");
        // 超过一个字节 → 用法错误（无论是否带位置指令）。
        let (io, _) = ExecIo::capture(b"abcdef");
        assert!(matches!(
            resolve_prompt(&args(None), &io, 5),
            Err(ExecOutcome::UsageError(_))
        ));
        let (io, _) = ExecIo::capture(b"abcdef");
        assert!(matches!(
            resolve_prompt(&args(Some("x")), &io, 5),
            Err(ExecOutcome::UsageError(_))
        ));
        // 终端 stdin：预算不参与，指令直接返回。
        let (io, _) = ExecIo::capture_interactive(b"");
        assert_eq!(resolve_prompt(&args(Some("hi")), &io, 0).unwrap(), "hi");
    }

    #[test]
    fn missing_prompt_on_tty_is_usage_error_and_piped_stdin_becomes_prompt() {
        let (storage_root, project_root, project) = setup("exec-stdin");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        // 终端 stdin 且无位置参数 → 用法错误。
        let (io, _) = ExecIo::capture_interactive(b"");
        match exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(None),
            io,
        ) {
            ExecOutcome::UsageError(message) => assert!(message.contains("prompt"), "{message}"),
            other => panic!("expected usage error, got {other:?}"),
        }
        // 管道 stdin（无指令）→ 全文作为 prompt。
        let (io, captured) = ExecIo::capture(b"from stdin");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(None),
            io,
        );
        assert!(matches!(outcome, ExecOutcome::Success { .. }));
        assert_eq!(captured.output_string(), "done\n");
        // 空管道 + 无指令 → 用法错误。
        let (io, _) = ExecIo::capture(b"   ");
        match exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(None),
            io,
        ) {
            ExecOutcome::UsageError(_) => {}
            other => panic!("expected usage error, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    // ---- 权限（INV-2 / HL-02）----

    struct WaitForInterruptInput;

    impl ExecPermissionInput for WaitForInterruptInput {
        fn read_answer(&self, cancel: &ExecCancel) -> ExecPermissionAnswer {
            while !cancel.interrupted() {
                std::thread::sleep(Duration::from_millis(5));
            }
            ExecPermissionAnswer::Interrupted
        }
    }

    #[test]
    fn non_interactive_run_denies_side_effects_by_default() {
        let (storage_root, project_root, project) = setup("exec-deny-default");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        let (io, captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            args(Some("write")),
            io,
        );
        // 拒绝是结构化工具错误，agent 收到后完成运行：进程退出码仍为 0。
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert!(
            !project_root.join("generated.txt").exists(),
            "denied write_file must not touch the project"
        );
        assert!(
            captured.error_string().contains("permission denied"),
            "denial must be visible on stderr"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn yes_flag_allows_side_effects() {
        let (storage_root, project_root, project) = setup("exec-yes-allow");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        let mut options = args(Some("write"));
        options.yes = true;
        let (io, _captured) = ExecIo::capture(b"");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            options,
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        let written = fs::read_to_string(project_root.join("generated.txt")).unwrap();
        assert_eq!(written, "from headless test");
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn interactive_stdin_answers_permission_prompts() {
        let (storage_root, project_root, project) = setup("exec-interactive-ask");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        // 回答 y → 允许。
        let (io, captured) = ExecIo::capture_interactive(b"y\n");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            args(Some("write")),
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert!(project_root.join("generated.txt").exists());
        let error = captured.error_string();
        assert!(error.contains("permission requested"), "{error}");
        assert!(
            error.contains("arguments"),
            "full arguments must be shown: {error}"
        );
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();

        // 回答空行 → 拒绝，项目不被写入。
        let (storage_root, project_root, project) = setup("exec-interactive-deny");
        prepare_storage(&project, &storage_root, TestBehavior::WriteFile);
        let (io, captured) = ExecIo::capture_interactive(b"\n");
        let outcome = exec(
            &project,
            &storage_root,
            TestBehavior::WriteFile,
            args(Some("write")),
            io,
        );
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "{outcome:?}"
        );
        assert!(!project_root.join("generated.txt").exists());
        assert!(captured.error_string().contains("permission denied"));
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn interrupted_permission_wait_denies_instead_of_blocking_forever() {
        // HL-02：交互询问必须可被中断解除。修复前 approver 阻塞在
        // 不可取消的 stdin read 上，第一次 Ctrl-C 无法唤醒它。
        let cancel = ExecCancel::new();
        let (io, _captured) = ExecIo::capture(b"");
        let approver = ExecApprover {
            mode: PermissionMode::Interactive,
            input: Some(Arc::new(WaitForInterruptInput)),
            error: Arc::clone(&io.error),
            io_state: Arc::new(ExecIoState::default()),
            interrupt: cancel.clone(),
        };
        // 模拟第一次 Ctrl-C（run 句柄尚未就位 → pending，标志已置位）。
        let _ = cancel.on_interrupt();
        let started = Instant::now();
        let decision = approver.decide(
            PermissionRequest {
                tool: "write_file".into(),
                effect: ToolEffect::Write,
                reason: "test".into(),
                arguments: serde_json::json!({}),
                call_id: "call-1".into(),
            },
            &crate::model::CancelToken::new(),
        );
        assert!(
            matches!(&decision, PermissionDecision::Deny { reason } if reason.contains("interrupted")),
            "unexpected decision: {decision:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "interrupted wait must return promptly"
        );
    }

    // ---- 持久化对等（INV-4）、会话策略与可重复性（HL-05）----

    #[test]
    fn default_run_persists_session_and_continue_appends_to_it() {
        let (storage_root, project_root, project) = setup("exec-persist-parity");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let (io, _) = ExecIo::capture(b"");
        exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("first")),
            io,
        );

        // 与 TUI 相同的重开路径：最近会话即 exec 创建的会话，含两轮消息。
        let (session_id, message) = persisted_user_message(&project, &storage_root);
        assert_eq!(message, "first");
        {
            let bootstrap =
                BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
            let mut application = bootstrap
                .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                    behavior: TestBehavior::Success,
                }))
                .unwrap();
            assert_eq!(application.snapshot().unwrap().transcript.len(), 2);
            application.close().unwrap();
        }

        // --continue 续接同一会话。
        let mut options = args(Some("second"));
        options.continue_session = true;
        let (io, _) = ExecIo::capture(b"");
        exec(&project, &storage_root, TestBehavior::Success, options, io);
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        assert_eq!(application.current_session_id(), Some(session_id.clone()));
        assert_eq!(application.snapshot().unwrap().transcript.len(), 4);
        application.close().unwrap();

        // 默认（无 --continue）→ 新会话，不污染旧会话。
        let (io, _) = ExecIo::capture(b"third-from-stdin");
        exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(None),
            io,
        );
        let bootstrap = BootstrapApplication::open(project, storage_root.clone()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        let fresh_id = application.current_session_id().expect("fresh session");
        assert_ne!(fresh_id, session_id);
        assert_eq!(application.snapshot().unwrap().transcript.len(), 2);
        application.close().unwrap();
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn continue_without_session_and_unknown_session_fail() {
        let (storage_root, project_root, project) = setup("exec-session-errors");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut options = args(Some("hi"));
        options.continue_session = true;
        let (io, _) = ExecIo::capture(b"");
        match exec(&project, &storage_root, TestBehavior::Success, options, io) {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("no session"), "{message}")
            }
            other => panic!("expected failure, got {other:?}"),
        }
        let mut options = args(Some("hi"));
        options.session = Some("9999".into());
        let (io, _) = ExecIo::capture(b"");
        match exec(&project, &storage_root, TestBehavior::Success, options, io) {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("session 9999"), "{message}")
            }
            other => panic!("expected failure, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    #[test]
    fn exec_runner_is_repeatable_within_one_process() {
        // HL-05：库入口不再安装进程级信号处理器，同进程连续调用
        // 互不干扰（信号安装只在 main.rs 进程边界发生一次）。
        let (storage_root, project_root, project) = setup("exec-repeatable");
        prepare_storage(&project, &storage_root, TestBehavior::Success);
        let mut first = args(Some("first run"));
        first.trust = true; // 存储已信任；flag 幂等，仅演示进程内重复调用
        let (io, _) = ExecIo::capture(b"");
        let outcome = exec(&project, &storage_root, TestBehavior::Success, first, io);
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "first run failed: {outcome:?}"
        );
        let mut second = args(Some("second run"));
        second.continue_session = true;
        let (io, _) = ExecIo::capture(b"");
        let outcome = exec(&project, &storage_root, TestBehavior::Success, second, io);
        assert!(
            matches!(outcome, ExecOutcome::Success { .. }),
            "second run failed: {outcome:?}"
        );
        // 续接后最后一条 user 消息是第二轮的。
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        let mut application = bootstrap
            .into_trusted_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap();
        let transcript = application.snapshot().unwrap().transcript;
        application.close().unwrap();
        let user_lines: Vec<&str> = transcript
            .iter()
            .filter(|line| line.kind == "user")
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(user_lines.len(), 2);
        assert_eq!(user_lines[1], "second run");
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }

    // ---- 未配置模型 ----

    #[test]
    fn unconfigured_model_fails_with_pointer_to_model_command() {
        let (storage_root, project_root, project) = setup("exec-unconfigured");
        // 只信任，不配置模型。
        let bootstrap = BootstrapApplication::open(project.clone(), storage_root.clone()).unwrap();
        bootstrap
            .authorize_and_mount_with_provider(Arc::new(TestProviderPlugin {
                behavior: TestBehavior::Success,
            }))
            .unwrap()
            .close()
            .unwrap();
        let (io, _) = ExecIo::capture(b"");
        match exec(
            &project,
            &storage_root,
            TestBehavior::Success,
            args(Some("hi")),
            io,
        ) {
            ExecOutcome::Failure(message) => {
                assert!(message.contains("model is not configured"), "{message}")
            }
            other => panic!("expected failure, got {other:?}"),
        }
        fs::remove_dir_all(storage_root).ok();
        fs::remove_dir_all(project_root).ok();
    }
}
