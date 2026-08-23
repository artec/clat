use clat::exec::{
    ExecArgs, ExecCancel, ExecIo, ExecOutcome, ExecPermissionAnswer, ExecPermissionInput,
    InterruptOutcome,
};
use clat::{EventSink, ModelEvent, Project, RunEvent};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const NAME: &str = "clat";
const TAGLINE: &str = "command-line agent";

fn main() -> ExitCode {
    run(env::args().skip(1))
}

fn run<I>(mut args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    match args.next().as_deref() {
        None => run_tui(),
        Some("demo") => run_demo(),
        Some("exec") => run_exec_command(args),
        Some("dsh") => run_dsh_command(args),
        Some("serve") => run_serve_command(args),
        Some("upgrade") => run_upgrade(args.next().as_deref() == Some("--check")),
        Some("-V" | "--version") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("clat: unknown command or argument: {command}");
            eprintln!("Run `clat --help` for usage.");
            ExitCode::from(2)
        }
    }
}

fn run_dsh_command<I>(args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let args: Vec<String> = args.collect();
    let code = clat::dsh::run_dsh(&args);
    if code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code as u8)
    }
}

fn print_help() {
    println!("{NAME} — {TAGLINE}");
    println!();
    println!("Usage: clat [COMMAND]");
    println!();
    println!("Running `clat` with no command opens the interactive TUI.");
    println!("Inside the TUI, use `/model` to configure model parameters and credentials.");
    println!();
    println!("Commands:");
    println!("  exec [PROMPT]     Run one agent turn headlessly and print the reply on stdout");
    println!("  dsh              Open the TUI as a client of a local DSH web host");
    println!("  serve             Serve the local HTTP+SSE API on 127.0.0.1");
    println!("  demo             Run the deterministic model → tool → model loop");
    println!("  upgrade          Upgrade to the latest GitHub release");
    println!();
    println!("Options:");
    println!("  -h, --help       Print help");
    println!("  -V, --version    Print version");
    println!();
    println!("Serve options:");
    println!("  --port <n>       Port to bind on 127.0.0.1 (default 0 = pick a free port)");
    println!("  --token <t>      Explicit auth token (default: generate one per run)");
    println!();
    println!("Exec options:");
    println!("  --continue       Continue the project's most recent session");
    println!("  --session <id>   Continue a specific session");
    println!("  --command <cmd>  Run one /command headlessly (e.g. --command /compact)");
    println!("  --trust          Trust the current project without the TUI prompt");
    println!("  --yes            Approve every side-effecting tool call (dangerous)");
    println!("  --quiet          Suppress stderr status output (assistant text only)");
    println!("  --json           Print the RunEvent stream as NDJSON on stdout instead of text");
    println!("  Piped stdin is used as the prompt, or as context when PROMPT is also present.");
}

/// `clat serve [--port <n>] [--token <t>]`：本地 HTTP+SSE 前端。
/// Ctrl-C 处理器在进程边界安装一次：第一次优雅关停（accept 循环
/// 完整走关停序列），第二次强退（serve 同款纪律）。
fn run_serve_command<I>(args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let parsed = match clat::serve::parse_serve_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("clat: {message}");
            eprintln!("Run `clat --help` for usage.");
            return ExitCode::from(2);
        }
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    let pressed = Arc::new(AtomicBool::new(false));
    if ctrlc::set_handler(move || {
        if pressed.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        flag.store(true, Ordering::SeqCst);
    })
    .is_err()
    {
        eprintln!("clat: warning: Ctrl-C will not stop serve gracefully on this host");
    }
    match clat::serve::run_serve_with_shutdown(parsed, shutdown) {
        0 => ExitCode::SUCCESS,
        1 => ExitCode::FAILURE,
        _ => ExitCode::from(130),
    }
}

fn run_tui() -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!("clat: could not determine current project: {error}");
            return ExitCode::FAILURE;
        }
    };

    match clat::tui::run(project) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: TUI failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `clat exec [OPTIONS] [PROMPT]`：headless 一次执行。stdout 只承载
/// 助手文本，状态走 stderr。退出码：0 成功、1 失败、2 用法错误、
/// 130 用户中断。Ctrl-C 处理器在进程边界安装一次（HL-05）：第一次
/// 优雅取消，第二次强退。
fn run_exec_command<I>(args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let parsed: ExecArgs = match clat::exec::parse_exec_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("clat: {message}");
            eprintln!("Run `clat --help` for usage.");
            return ExitCode::from(2);
        }
    };
    let cancel = ExecCancel::new();
    let signal_cancel = cancel.clone();
    // 进程边界拥有信号：lib 可重复调用、不接管宿主 handler（HL-05）。
    // 安装失败（宿主已有 handler 等）不阻塞执行，只是失去优雅取消。
    if ctrlc::set_handler(move || {
        if let InterruptOutcome::MustExit = signal_cancel.on_interrupt() {
            std::process::exit(130);
        }
    })
    .is_err()
    {
        eprintln!("clat: warning: Ctrl-C will not cancel gracefully in this host");
    }
    let interactive_stdin = io::stdin().is_terminal();
    let mut exec_io = ExecIo::new(
        Box::new(io::stdin()),
        Box::new(io::stdout()),
        Box::new(io::stderr()),
        interactive_stdin,
    );
    if interactive_stdin {
        exec_io = exec_io.with_permission_input(Arc::new(TerminalPermissionInput));
    }
    // FP-10：TTY 双契约——stdout 是终端时模型 delta 过可见转义。
    exec_io = exec_io.with_stdout_terminal(io::stdout().is_terminal());
    match clat::exec::run_exec_with_cancel(parsed, exec_io, &cancel) {
        ExecOutcome::Success { .. } => ExitCode::SUCCESS,
        ExecOutcome::Cancelled { .. } => ExitCode::from(130),
        ExecOutcome::Failure(message) => {
            eprintln!("clat: {message}");
            ExitCode::FAILURE
        }
        ExecOutcome::UsageError(message) => {
            eprintln!("clat: {message}");
            eprintln!("Run `clat --help` for usage.");
            ExitCode::from(2)
        }
    }
}

/// 真实终端的 request-scoped 权限输入。仅在权限详情已经写出并 flush 后
/// 进入 raw mode；先清掉请求前积压的按键，再读取本次 y/N。这样既能
/// 轮询取消，也不会把用户早先输入的 `y` 当成本次授权。
struct TerminalPermissionInput;

impl ExecPermissionInput for TerminalPermissionInput {
    fn read_answer(&self, cancel: &ExecCancel) -> ExecPermissionAnswer {
        if cancel.interrupted() {
            return ExecPermissionAnswer::Interrupted;
        }
        let _raw = match RawModeGuard::enter() {
            Ok(guard) => guard,
            Err(error) => return ExecPermissionAnswer::Error(error.to_string()),
        };
        read_permission_events(cancel, &mut CrosstermPermissionEvents)
    }
}

trait PermissionEventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read(&mut self) -> io::Result<Event>;
    fn echo(&mut self, text: &str);
}

struct CrosstermPermissionEvents;

impl PermissionEventSource for CrosstermPermissionEvents {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        event::read()
    }

    fn echo(&mut self, text: &str) {
        eprint!("{text}");
        let _ = io::stderr().flush();
    }
}

fn read_permission_events(
    cancel: &ExecCancel,
    events: &mut dyn PermissionEventSource,
) -> ExecPermissionAnswer {
    // raw mode 会把 canonical 缓冲中尚未提交的旧字符也变为可读；在
    // request 已展示后统一清空。Ctrl-C 始终视为当前中断而非旧输入。
    loop {
        match events.poll(Duration::ZERO) {
            Ok(true) => match events.read() {
                Ok(Event::Key(key)) if is_ctrl_c(key) => return interrupt_permission(cancel),
                Ok(_) => {}
                Err(error) => return ExecPermissionAnswer::Error(error.to_string()),
            },
            Ok(false) => break,
            Err(error) => return ExecPermissionAnswer::Error(error.to_string()),
        }
    }

    let mut answer = String::new();
    loop {
        if cancel.interrupted() {
            return ExecPermissionAnswer::Interrupted;
        }
        match events.poll(Duration::from_millis(100)) {
            Ok(false) => continue,
            Err(error) => return ExecPermissionAnswer::Error(error.to_string()),
            Ok(true) => {}
        }
        let event = match events.read() {
            Ok(event) => event,
            Err(error) => return ExecPermissionAnswer::Error(error.to_string()),
        };
        match event {
            Event::Key(key) if is_ctrl_c(key) => return interrupt_permission(cancel),
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            }) => return ExecPermissionAnswer::Answer(answer),
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press,
                ..
            }) => return ExecPermissionAnswer::Answer(String::new()),
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                kind: KeyEventKind::Press,
                ..
            }) => {
                if answer.pop().is_some() {
                    events.echo("\x08 \x08");
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char(character),
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && answer.chars().count() < 16 =>
            {
                answer.push(character);
                events.echo(&character.to_string());
            }
            _ => {}
        }
    }
}

fn is_ctrl_c(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('c')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn interrupt_permission(cancel: &ExecCancel) -> ExecPermissionAnswer {
    if matches!(cancel.on_interrupt(), InterruptOutcome::MustExit) {
        std::process::exit(130);
    }
    ExecPermissionAnswer::Interrupted
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn run_demo() -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!("clat: could not determine current project: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = clat::demo::run_demo(
        project,
        "prove the agent loop works",
        Box::new(DemoEventSink),
    );
    match result {
        Ok(output) => {
            println!();
            eprintln!(
                "[{} turns, {} input tokens, {} output tokens]",
                output.turns, output.usage.input_tokens, output.usage.output_tokens
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `clat upgrade [--check]`：检查并安装 GitHub 最新 release。
/// `--check` 只报告不安装；已是最新输出提示并以 0 退出。
fn run_upgrade(check_only: bool) -> ExitCode {
    use clat::upgrade::UpgradeOutcome;
    match clat::upgrade::upgrade(check_only) {
        Ok(UpgradeOutcome::UpToDate { latest }) => {
            println!(
                "{NAME} {} is up to date (latest release {latest})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Ok(UpgradeOutcome::Available { tag }) => {
            println!("{NAME} {} → {tag} available", env!("CARGO_PKG_VERSION"));
            println!("Run `clat upgrade` to install {tag}.");
            ExitCode::SUCCESS
        }
        Ok(UpgradeOutcome::Installed { tag }) => {
            println!("{NAME} upgraded to {tag}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: upgrade failed: {error}");
            ExitCode::FAILURE
        }
    }
}

struct DemoEventSink;

impl EventSink for DemoEventSink {
    fn emit(&mut self, event: RunEvent) {
        match event {
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => eprintln!("● {provider}/{model} turn {turn}"),
            RunEvent::ModelStream {
                event: ModelEvent::TextDelta { delta },
                ..
            }
            | RunEvent::ModelStream {
                event: ModelEvent::RefusalDelta { delta },
                ..
            } => {
                print!("{delta}");
                let _ = io::stdout().flush();
            }
            RunEvent::ToolRequested { call } => {
                eprintln!("\n● tool {} {}", call.name, call.arguments)
            }
            RunEvent::PermissionChecked { decision, .. } => eprintln!("● permission {decision:?}"),
            RunEvent::PermissionDenied { tool, reason } => {
                eprintln!("● permission denied {tool}: {reason}")
            }
            RunEvent::ToolFinished { result } => {
                if result.is_error {
                    eprintln!("● tool error {}", result.output);
                } else {
                    eprintln!("● tool result {}", result.output);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakePermissionEvents {
        stale: VecDeque<Event>,
        current: VecDeque<Event>,
    }

    impl PermissionEventSource for FakePermissionEvents {
        fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
            if timeout == Duration::ZERO {
                Ok(!self.stale.is_empty())
            } else {
                Ok(!self.current.is_empty())
            }
        }

        fn read(&mut self) -> io::Result<Event> {
            self.stale
                .pop_front()
                .or_else(|| self.current.pop_front())
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no fake event"))
        }

        fn echo(&mut self, _text: &str) {}
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn unknown_argument_fails() {
        let code = run(["unknown".to_owned()].into_iter());
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn help_succeeds() {
        let code = run(["--help".to_owned()].into_iter());
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn demo_command_executes_agent_loop() {
        let code = run(["demo".to_owned()].into_iter());
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn exec_usage_errors_exit_2() {
        let code = run(["exec".to_owned(), "--bogus".to_owned()].into_iter());
        assert_eq!(code, ExitCode::from(2));
        let code = run(["exec".to_owned(), "--session".to_owned()].into_iter());
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn permission_input_discards_pre_request_yes_before_reading_current_answer() {
        let mut events = FakePermissionEvents {
            stale: VecDeque::from([
                key(KeyCode::Char('y'), KeyModifiers::NONE),
                key(KeyCode::Enter, KeyModifiers::NONE),
            ]),
            current: VecDeque::from([
                key(KeyCode::Char('n'), KeyModifiers::NONE),
                key(KeyCode::Enter, KeyModifiers::NONE),
            ]),
        };
        let answer = read_permission_events(&ExecCancel::new(), &mut events);
        assert!(
            matches!(answer, ExecPermissionAnswer::Answer(value) if value == "n"),
            "stale y must be drained before this request reads its answer"
        );
    }

    #[test]
    fn permission_input_treats_ctrl_c_as_run_cancellation() {
        let cancel = ExecCancel::new();
        let mut events = FakePermissionEvents {
            stale: VecDeque::new(),
            current: VecDeque::from([key(KeyCode::Char('c'), KeyModifiers::CONTROL)]),
        };
        assert!(matches!(
            read_permission_events(&cancel, &mut events),
            ExecPermissionAnswer::Interrupted
        ));
        assert!(cancel.interrupted());
    }
}
