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

use clat::client_ports::presentation::{
    LocaleContext, UiMessage, leading_language, terminal_language,
};
#[path = "cli/language.rs"]
mod language_cli;

const NAME: &str = "clat";

#[path = "cli/host.rs"]
mod host_cli;
use host_cli::run_host_command;
#[path = "cli/upgrade.rs"]
mod upgrade_cli;
use upgrade_cli::run_upgrade;

fn main() -> ExitCode {
    #[cfg(windows)]
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--internal-windows-sandbox"))
    {
        // Preserve the complete Windows child exit code instead of truncating
        // through ExitCode's byte constructor. Dispatch before any host setup.
        std::process::exit(
            clat::run_windows_sandbox_internal(env::args_os().skip(2).collect()) as i32,
        );
    }
    run(env::args().skip(1))
}

fn run<I>(args: I) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let (explicit, args) = match leading_language(args.collect()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{}", message.english());
            return ExitCode::from(2);
        }
    };
    let language = match terminal_language(explicit.as_deref()) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{}", message.english());
            return ExitCode::from(2);
        }
    };
    for warning in language.warnings {
        eprintln!("{}", language.context.render(&warning));
    }
    dispatch(args.into_iter(), language.context, language.preference)
}
fn dispatch<I>(
    mut args: I,
    locale: LocaleContext,
    preference: clat::client_ports::presentation::LanguagePreference,
) -> ExitCode
where
    I: Iterator<Item = String>,
{
    match args.next().as_deref() {
        None => host_cli::run_default_tui(locale),
        Some("language") => language_cli::run(args, locale, preference),
        Some("standalone" | "--standalone") => {
            if args.next().is_some() {
                eprintln!("{}", locale.text("cli.standalone_args"));
                ExitCode::from(2)
            } else {
                run_tui(locale)
            }
        }
        Some("host") => run_host_command(args, locale),
        Some("demo") => run_demo(locale),
        Some("exec") => run_exec_command(args, locale),
        Some("dsh") => run_dsh_command(args, locale),
        Some("attach") => run_attach_command(args, locale),
        Some("serve") => run_serve_command(args, locale),
        Some("wechat") => run_wechat_command(args, locale),
        Some("plugin") => run_plugin_command(args, locale),
        Some("upgrade") => run_upgrade(args.next().as_deref() == Some("--check"), locale),
        Some("-V" | "--version") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("-h" | "--help") => {
            print_help(locale);
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("cli.unknown_command").arg("command", command))
            );
            eprintln!("{}", locale.text("cli.help_hint"));
            ExitCode::from(2)
        }
    }
}

fn run_dsh_command<I>(args: I, locale: LocaleContext) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let args: Vec<String> = args.collect();
    // D-2：dsh 态走 CLAT TUI 本体（同一 App/事件循环/弹框），仅数据源
    // 与命令后端换成宿主。
    match clat::tui::run_dsh_mode_with_locale(&args, locale) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: dsh: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_attach_command(args: impl Iterator<Item = String>, locale: LocaleContext) -> ExitCode {
    let mut args = args;
    let mut port = 2691;
    let mut trust = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trust" => trust = true,
            "--port" => match args.next().and_then(|value| value.parse::<u16>().ok()) {
                Some(value) if value != 0 => port = value,
                _ => {
                    eprintln!("{}", locale.text("cli.attach_port"));
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("{}", locale.text("cli.attach_args"));
                return ExitCode::from(2);
            }
        }
    }
    let result = Project::current()
        .and_then(|project| clat::tui::run_host_mode_with_locale(project, port, trust, locale));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clat: attach: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help(locale: LocaleContext) {
    print!("{}", locale.text("cli.help"));
}

fn run_wechat_command<I>(args: I, locale: LocaleContext) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!(
                "{}",
                locale
                    .render(&UiMessage::new("cli.project_error").arg("detail", error.to_string()))
            );
            return ExitCode::FAILURE;
        }
    };
    ExitCode::from(clat::serve::run_wechat_command_with_locale(
        project, args, locale,
    ))
}

fn run_plugin_command<I>(args: I, locale: LocaleContext) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let outcome = clat::plugin_cli::run(args).localize(locale);
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    ExitCode::from(outcome.exit_code)
}

/// `clat serve [--no-open] [--port <n>] [--token <t> | --rotate-token]`：本地
/// HTTP+SSE 前端。
/// 信号处理器在进程边界安装一次：Ctrl-C 以及 Unix SIGTERM/SIGHUP
/// 第一次触发优雅关停（accept 循环完整走关停序列），第二次强退。
fn run_serve_command<I>(args: I, locale: LocaleContext) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let parsed = match clat::serve::parse_serve_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("clat: {message}");
            eprintln!("{}", locale.text("cli.help_hint"));
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
        eprintln!("{}", locale.text("cli.signal_serve"));
    }
    match clat::serve::run_serve_with_shutdown(parsed, shutdown) {
        0 => ExitCode::SUCCESS,
        1 => ExitCode::FAILURE,
        _ => ExitCode::from(130),
    }
}

fn run_tui(locale: LocaleContext) -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!(
                "{}",
                locale
                    .render(&UiMessage::new("cli.project_error").arg("detail", error.to_string()))
            );
            return ExitCode::FAILURE;
        }
    };

    match clat::tui::run_with_locale(project, locale) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "{}",
                locale.render(&UiMessage::new("cli.tui_error").arg("detail", error.to_string()))
            );
            ExitCode::FAILURE
        }
    }
}

/// `clat exec [OPTIONS] [PROMPT]`：headless 一次执行。stdout 只承载
/// 助手文本，状态走 stderr。退出码：0 成功、1 失败、2 用法错误、
/// 130 用户中断。Ctrl-C 处理器在进程边界安装一次（HL-05）：第一次
/// 优雅取消，第二次强退。
fn run_exec_command<I>(args: I, locale: LocaleContext) -> ExitCode
where
    I: Iterator<Item = String>,
{
    let parsed: ExecArgs = match clat::exec::parse_exec_args_presented(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("clat: {}", locale.render(&message));
            eprintln!("{}", locale.text("cli.help_hint"));
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
        eprintln!("{}", locale.text("cli.signal_exec"));
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
    exec_io = exec_io
        .with_locale(locale)
        .with_stdout_terminal(io::stdout().is_terminal());
    match clat::exec::run_exec_with_cancel(parsed, exec_io, &cancel) {
        ExecOutcome::Success { .. } => ExitCode::SUCCESS,
        ExecOutcome::Cancelled { .. } => ExitCode::from(130),
        ExecOutcome::Failure(message) => {
            eprintln!("clat: {}", message.render(locale));
            ExitCode::FAILURE
        }
        ExecOutcome::UsageError(message) => {
            eprintln!("clat: {}", message.render(locale));
            eprintln!("{}", locale.text("cli.help_hint"));
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

fn run_demo(locale: LocaleContext) -> ExitCode {
    let project = match Project::current() {
        Ok(project) => project,
        Err(error) => {
            eprintln!(
                "{}",
                locale
                    .render(&UiMessage::new("cli.project_error").arg("detail", error.to_string()))
            );
            return ExitCode::FAILURE;
        }
    };
    let result = clat::demo::run_demo(
        project,
        "prove the agent loop works",
        Box::new(DemoEventSink { locale }),
    );
    match result {
        Ok(output) => {
            println!();
            eprintln!(
                "{}",
                locale.render(
                    &UiMessage::new("cli.demo_summary")
                        .arg("turns", output.turns)
                        .arg("input", output.usage.input_tokens)
                        .arg("output", output.usage.output_tokens)
                )
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("clat: {error}");
            ExitCode::FAILURE
        }
    }
}

struct DemoEventSink {
    locale: LocaleContext,
}
impl DemoEventSink {
    fn notice(&self, message: UiMessage) {
        eprintln!("{}", self.locale.render(&message));
    }
}

impl EventSink for DemoEventSink {
    fn emit(&mut self, event: RunEvent) {
        match event {
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
            RunEvent::ToolRequested { call } => self.notice(
                UiMessage::new("cli.demo_tool")
                    .arg("name", call.name)
                    .arg("arguments", call.arguments.to_string()),
            ),
            RunEvent::PermissionChecked { decision, .. } => self.notice(
                UiMessage::new("cli.demo_permission").arg("decision", format!("{decision:?}")),
            ),
            RunEvent::PermissionDenied { tool, reason } => self.notice(
                UiMessage::new("cli.demo_denied")
                    .arg("tool", tool)
                    .arg("reason", reason),
            ),
            RunEvent::ToolFinished { result } => {
                if result.is_error {
                    self.notice(
                        UiMessage::new("cli.demo_error").arg("output", result.output.to_string()),
                    );
                } else {
                    self.notice(
                        UiMessage::new("cli.demo_result").arg("output", result.output.to_string()),
                    );
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
    fn host_and_standalone_usage_errors_do_not_start_a_frontend() {
        for args in [
            vec!["host"],
            vec!["host", "stop", "--force"],
            vec!["host", "status", "--port", "0"],
            vec!["standalone", "--unexpected"],
            vec!["--standalone", "--unexpected"],
        ] {
            assert_eq!(run(args.into_iter().map(String::from)), ExitCode::from(2));
        }
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
