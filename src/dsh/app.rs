//! `clat dsh` 的终端面（D-1 §5）：自有 App/事件循环（终端输入 + 双 WS
//! 下行 + HTTP worker 三路合流）。渲染复用 `ConversationModel`（INV-D6）；
//! 命令面按设计 §5 映射；审批/问答弹卡经 `/api/respond` 回填。

use crate::dsh::client::DshClient;
use crate::dsh::connect::{ConnectFailure, Online, ensure_online};
use crate::dsh::files::{self, DshSessionRow};
use crate::dsh::frames::{DshFrame, SessionEventNotice, event_vocabulary_violation, parse_frame};
use crate::dsh::transcript::DshTranscript;
use crate::dsh::ws::{self, WsMessage};
use crate::session::event::SessionEvent;
use crate::tui::conversation::ToolCardVisibility;
use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::{Value, json};
use std::io::Stdout;
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// `clat dsh` 入口（main.rs 分发）。返回进程退出码。
pub fn run_dsh(args: &[String]) -> i32 {
    let port = parse_port(args).unwrap_or(crate::dsh::connect::DEFAULT_PORT);
    let mut terminal = match setup_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("clat: dsh: cannot enter the terminal: {error}");
            return 1;
        }
    };
    let outcome: Result<i32, String> = (|| {
        // 连接屏（阻塞期保持可见反馈）。
        draw_notice(&mut terminal, "probing dsh web on 127.0.0.1 …")
            .map_err(|error| error.to_string())?;
        let online = match ensure_online(port, "dsh", files::dsh_home().as_deref()) {
            Ok(online) => online,
            Err(ConnectFailure::NotInstalled) => {
                restore_terminal(&mut terminal);
                eprintln!("clat: dsh: dsh is not installed (no dsh executable and no ~/.dsh)");
                return Ok(1);
            }
            Err(ConnectFailure::Failed(message)) => {
                restore_terminal(&mut terminal);
                eprintln!("clat: dsh: {message}");
                return Ok(1);
            }
        };
        match DshApp::run(terminal, online) {
            Ok(()) => Ok(0),
            Err(message) => {
                // terminal 已由 run 内恢复。
                eprintln!("clat: dsh: {message}");
                Ok(1)
            }
        }
    })();
    match outcome {
        Ok(code) => code,
        Err(error) => {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
            eprintln!("clat: dsh: {error}");
            1
        }
    }
}

fn parse_port(args: &[String]) -> Option<u16> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--port" {
            return iter.next().and_then(|value| value.parse().ok());
        }
    }
    None
}

fn setup_terminal() -> std::io::Result<Terminal<ratatui::backend::CrosstermBackend<Stdout>>> {
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
}

fn restore_terminal(_terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>) {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
}

fn draw_notice(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    message: &str,
) -> std::io::Result<()> {
    terminal
        .draw(|frame| {
            frame.render_widget(
                Paragraph::new(format!(" clat dsh — {message}"))
                    .style(Style::default().fg(Color::Gray)),
                frame.area(),
            );
        })
        .map(|_| ())
}

// ---- UI 事件与 HTTP 任务 ----

enum UiEvent {
    Terminal(TerminalEvent),
    Mux(DshFrame),
    Host(DshFrame),
    LinkDown(String),
    Worker(WorkerReply),
}

enum WorkerTask {
    Restore,
    Prompt {
        session: String,
        steer: bool,
        text: String,
    },
    Cancel {
        session: String,
    },
    Create {
        cwd: Option<String>,
    },
    History {
        session: String,
    },
    Models {
        session: Option<String>,
    },
    Select {
        session: String,
        provider: String,
        model: String,
    },
    Rename {
        session: String,
        title: String,
    },
    Respond {
        rpc_id: String,
        result: Value,
    },
    Reconnect,
}

enum WorkerReply {
    Restored {
        session: Option<String>,
    },
    History {
        session: String,
        events: Vec<SessionEvent>,
    },
    Status(String),
    Created(String),
    Models(Vec<(String, String, String)>),
    Failed(String),
    Reconnected(u16),
}

struct PendingApproval {
    rpc_id: String,
    session_id: String,
    approval_id: String,
    tool_name: String,
    reason: Option<String>,
}

struct PendingQuestion {
    rpc_id: String,
    session_id: String,
    questions: Vec<Value>,
    index: usize,
    answers: Vec<Value>,
    input: String,
}

enum Picker {
    Sessions {
        rows: Vec<DshSessionRow>,
        selected: usize,
    },
    Models {
        entries: Vec<(String, String, String)>,
        selected: usize,
    },
}

struct DshApp {
    ui: Sender<UiEvent>,
    client: DshClient,
    port: u16,
    describe: Value,
    transcript: DshTranscript,
    current_session: Option<String>,
    running: bool,
    connected: bool,
    status: Option<(String, Instant)>,
    banner: Option<String>,
    notices: Vec<SessionEventNotice>,
    input: String,
    scroll_bottom: bool,
    scroll_offset: usize,
    approval: Option<PendingApproval>,
    question: Option<PendingQuestion>,
    picker: Option<Picker>,
    quit: bool,
}

impl DshApp {
    fn run(
        mut terminal: Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
        online: Online,
    ) -> Result<(), String> {
        let (ui_tx, ui_rx) = channel::<UiEvent>();
        // 终端输入转发线程（tui::run 同款）。
        {
            let sender = ui_tx.clone();
            std::thread::spawn(move || {
                loop {
                    match crossterm::event::read() {
                        Ok(event) => {
                            if sender.send(UiEvent::Terminal(event)).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }
        let mut app = Self::open(online, ui_tx.clone())?;
        // HTTP worker。
        let (task_tx, task_rx) = channel::<WorkerTask>();
        spawn_worker(app.client.clone(), app.port, task_rx, ui_tx);
        task_tx.send(WorkerTask::Restore).ok();

        while !app.quit {
            let event = ui_rx
                .recv_timeout(Duration::from_millis(120))
                .unwrap_or_else(|_| UiEvent::Terminal(TerminalEvent::FocusGained));
            if !matches!(event, UiEvent::Terminal(TerminalEvent::FocusGained)) {
                app.handle(event, &task_tx);
            }
            app.draw(&mut terminal).map_err(|e| e.to_string())?;
        }
        restore_terminal(&mut terminal);
        Ok(())
    }

    fn open(online: Online, ui_tx: Sender<UiEvent>) -> Result<Self, String> {
        let client = DshClient::new(online.port);
        open_downlink(online.port, "/api/events.mux", &ui_tx)?;
        open_downlink(online.port, "/api/events.host", &ui_tx)?;
        Ok(Self {
            ui: ui_tx,
            client,
            port: online.port,
            describe: online.describe,
            transcript: DshTranscript::new(),
            current_session: None,
            running: false,
            connected: true,
            status: None,
            banner: None,
            notices: Vec::new(),
            input: String::new(),
            scroll_bottom: true,
            scroll_offset: 0,
            approval: None,
            question: None,
            picker: None,
            quit: false,
        })
    }

    // ---- 事件处理 ----

    fn handle(&mut self, event: UiEvent, tasks: &Sender<WorkerTask>) {
        match event {
            UiEvent::Terminal(event) => self.handle_terminal(event, tasks),
            UiEvent::Mux(frame) => self.handle_mux(frame, tasks),
            UiEvent::Host(frame) => self.handle_host(frame),
            UiEvent::LinkDown(reason) => {
                if self.connected {
                    self.connected = false;
                    self.running = false;
                    self.banner = Some(format!("disconnected ({reason}) — /reconnect to retry"));
                }
            }
            UiEvent::Worker(reply) => self.handle_reply(reply, tasks),
        }
    }

    fn handle_terminal(&mut self, event: TerminalEvent, tasks: &Sender<WorkerTask>) {
        let TerminalEvent::Key(key) = event else {
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }
        // 对话框优先接管输入。
        if self.approval.is_some() {
            self.handle_approval_key(key, tasks);
            return;
        }
        if self.question.is_some() {
            self.handle_question_key(key, tasks);
            return;
        }
        if self.picker.is_some() {
            self.handle_picker_key(key, tasks);
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
            (KeyCode::Esc, _) => {
                if let Some(session) = self.current_session.clone()
                    && self.running
                {
                    tasks.send(WorkerTask::Cancel { session }).ok();
                    self.flash("cancelling…");
                }
            }
            (KeyCode::Enter, _) => self.submit(tasks),
            (KeyCode::Backspace, _) => {
                self.input.pop();
            }
            (KeyCode::PageUp, _) => {
                self.scroll_bottom = false;
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            (KeyCode::PageDown, _) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                if self.scroll_offset == 0 {
                    self.scroll_bottom = true;
                }
            }
            (KeyCode::Char(character), modifiers) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(character);
            }
            _ => {}
        }
    }

    fn handle_approval_key(&mut self, key: KeyEvent, tasks: &Sender<WorkerTask>) {
        let Some(pending) = self.approval.take() else {
            return;
        };
        let outcome = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Some("allowed-once"),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some("rejected"),
            _ => None,
        };
        match outcome {
            Some(outcome) => {
                tasks
                    .send(WorkerTask::Respond {
                        rpc_id: pending.rpc_id,
                        result: json!({
                            "sessionId": pending.session_id,
                            "approvalId": pending.approval_id,
                            "outcome": outcome,
                        }),
                    })
                    .ok();
                self.flash(format!("approval {outcome}"));
            }
            None => self.approval = Some(pending),
        }
    }

    fn handle_question_key(&mut self, key: KeyEvent, tasks: &Sender<WorkerTask>) {
        let Some(pending) = self.question.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                let pending = self.question.take().expect("checked above");
                tasks
                    .send(WorkerTask::Respond {
                        rpc_id: pending.rpc_id,
                        result: json!({
                            "type": "client-response-error",
                            "result": {"ok": false, "error": {"code": "cancelled", "message": "cancelled", "details": {}}},
                        }),
                    })
                    .ok();
                self.flash("question cancelled");
            }
            KeyCode::Enter => {
                // 选项题：输入序号（1-9）；自由题：输入文本。
                let question = &pending.questions[pending.index.min(pending.questions.len() - 1)];
                let options = question.get("options").and_then(Value::as_array);
                let answer = match options {
                    Some(options) => {
                        let selected = pending
                            .input
                            .chars()
                            .filter_map(|character| character.to_digit(10))
                            .filter_map(|digit| {
                                options
                                    .get(digit.saturating_sub(1) as usize)
                                    .and_then(|option| option.get("label"))
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "id": question.get("id").cloned().unwrap_or(Value::Null),
                            "selected": selected,
                        })
                    }
                    None => json!({
                        "id": question.get("id").cloned().unwrap_or(Value::Null),
                        "selected": [],
                        "custom": pending.input.clone(),
                    }),
                };
                pending.answers.push(answer);
                pending.input.clear();
                if pending.index + 1 < pending.questions.len() {
                    pending.index += 1;
                } else {
                    let done = self.question.take().expect("checked above");
                    tasks
                        .send(WorkerTask::Respond {
                            rpc_id: done.rpc_id,
                            result: json!({
                                "sessionId": done.session_id,
                                "answer": {"answers": done.answers},
                            }),
                        })
                        .ok();
                    self.flash("question answered");
                }
            }
            KeyCode::Backspace => {
                pending.input.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                pending.input.push(character);
            }
            _ => {}
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent, tasks: &Sender<WorkerTask>) {
        enum Choice {
            Session(String),
            Model(String, String),
            Pending,
            Cancelled,
        }
        let choice = match key.code {
            KeyCode::Up => {
                if let Some(picker) = self.picker.as_mut() {
                    match picker {
                        Picker::Sessions { selected, .. } | Picker::Models { selected, .. } => {
                            *selected = selected.saturating_sub(1);
                        }
                    }
                }
                Choice::Pending
            }
            KeyCode::Down => {
                if let Some(picker) = self.picker.as_mut() {
                    let (selected, limit) = match picker {
                        Picker::Sessions { selected, rows } => (selected, rows.len()),
                        Picker::Models { selected, entries } => (selected, entries.len()),
                    };
                    if *selected + 1 < limit {
                        *selected += 1;
                    }
                }
                Choice::Pending
            }
            KeyCode::Esc => Choice::Cancelled,
            KeyCode::Enter => match self.picker.as_ref() {
                Some(Picker::Sessions { rows, selected }) => rows
                    .get(*selected)
                    .map(|row| Choice::Session(row.session_id.clone()))
                    .unwrap_or(Choice::Cancelled),
                Some(Picker::Models { entries, selected }) => entries
                    .get(*selected)
                    .map(|(_, provider, model)| Choice::Model(provider.clone(), model.clone()))
                    .unwrap_or(Choice::Cancelled),
                None => Choice::Cancelled,
            },
            _ => Choice::Pending,
        };
        match choice {
            Choice::Pending => {}
            Choice::Cancelled => self.picker = None,
            Choice::Session(session) => {
                self.picker = None;
                self.switch_to(session, tasks);
            }
            Choice::Model(provider, model) => {
                self.picker = None;
                if let Some(session) = self.current_session.clone() {
                    tasks
                        .send(WorkerTask::Select {
                            session,
                            provider,
                            model,
                        })
                        .ok();
                    self.flash("selecting model…");
                }
            }
        }
    }

    // ---- 提交与命令 ----

    fn submit(&mut self, tasks: &Sender<WorkerTask>) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.dispatch_command(&text, tasks);
            return;
        }
        let Some(session) = self.current_session.clone() else {
            self.flash("no session — start one with /new");
            return;
        };
        let steer = self.running;
        tasks
            .send(WorkerTask::Prompt {
                session,
                steer,
                text,
            })
            .ok();
        self.flash(if steer { "steering…" } else { "sending…" });
    }

    /// 命令面（设计 §5）：可用集映射 API；CLAT 自有命令软置灰。
    fn dispatch_command(&mut self, input: &str, tasks: &Sender<WorkerTask>) {
        let (name, args) = match input.split_once(' ') {
            Some((name, args)) => (name, args.trim()),
            None => (input, ""),
        };
        match name {
            "/quit" | "/exit" => self.quit = true,
            "/help" => self.flash(
                "/new /resume /model /rename <title> /reconnect /quit — other CLAT \
                 commands are not available in clat dsh mode",
            ),
            "/new" => {
                let cwd = self
                    .describe
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                tasks.send(WorkerTask::Create { cwd }).ok();
                self.flash("creating session…");
            }
            "/resume" => match files::read_sessions(&files::dsh_home().unwrap_or_default()) {
                Some(rows) if !rows.is_empty() => {
                    self.picker = Some(Picker::Sessions { rows, selected: 0 });
                }
                // 数据面缺席（INV-D6 fail-soft）→ API 列表兜底。
                _ => {
                    tasks.send(WorkerTask::Restore).ok();
                    self.flash("listing sessions…");
                }
            },
            "/model" => {
                tasks
                    .send(WorkerTask::Models {
                        session: self.current_session.clone(),
                    })
                    .ok();
                self.flash("loading models…");
            }
            "/rename" => {
                if args.is_empty() {
                    self.flash("usage: /rename <title>");
                } else if let Some(session) = self.current_session.clone() {
                    tasks
                        .send(WorkerTask::Rename {
                            session,
                            title: args.to_owned(),
                        })
                        .ok();
                    self.flash("renaming…");
                } else {
                    self.flash("no active session");
                }
            }
            "/reconnect" => {
                tasks.send(WorkerTask::Reconnect).ok();
                self.flash("reconnecting…");
            }
            other => {
                // 软置灰：可发现、不可用。
                self.flash(format!("{other} is not available in clat dsh mode"));
            }
        }
    }

    fn switch_to(&mut self, session: String, tasks: &Sender<WorkerTask>) {
        self.current_session = Some(session.clone());
        self.transcript = DshTranscript::new();
        self.notices.clear();
        self.running = false;
        tasks.send(WorkerTask::History { session }).ok();
        self.flash("loading history…");
    }

    // ---- 帧处理 ----

    fn handle_mux(&mut self, frame: DshFrame, tasks: &Sender<WorkerTask>) {
        match frame {
            DshFrame::Subscribed {
                session_id,
                last_seq,
            } => {
                if self.current_session.as_deref() == Some(session_id.as_str()) {
                    self.transcript.baseline(last_seq);
                }
            }
            DshFrame::SessionEvent { session_id, event } => {
                if self.current_session.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                if let Some(notice) = event_vocabulary_violation(&session_id, &event) {
                    self.notices.push(notice);
                }
                if let Some(from) = self.transcript.gap_before(&event) {
                    // INV-D5：帧丢失 → 拉历史补（worker 过滤未见过的事件）。
                    let _ = from;
                    let session = session_id.clone();
                    tasks.send(WorkerTask::History { session }).ok();
                }
                self.transcript.apply(&event);
                if event.event_type == "turn/start" {
                    self.running = true;
                } else if event.event_type == "turn/end" {
                    self.running = false;
                }
            }
            DshFrame::ApprovalRequested {
                rpc_id,
                session_id,
                approval_id,
                tool_name,
                reason,
                ..
            } => {
                if self.approval.is_none() {
                    self.approval = Some(PendingApproval {
                        rpc_id,
                        session_id,
                        approval_id,
                        tool_name,
                        reason,
                    });
                }
            }
            DshFrame::ApprovalResolved { .. } => {
                self.approval = None;
            }
            DshFrame::QuestionRequested {
                rpc_id,
                session_id,
                questions,
            } => {
                let count = questions.as_array().map(Vec::len).unwrap_or(0);
                if count > 0 && self.question.is_none() {
                    self.question = Some(PendingQuestion {
                        rpc_id,
                        session_id,
                        questions: questions.as_array().cloned().unwrap_or_default(),
                        index: 0,
                        answers: Vec::new(),
                        input: String::new(),
                    });
                }
            }
            DshFrame::QuestionResolved { rpc_id, .. } => {
                if self
                    .question
                    .as_ref()
                    .is_some_and(|pending| pending.rpc_id == rpc_id)
                {
                    self.question = None;
                }
            }
            DshFrame::Queue { .. } | DshFrame::SessionStatus { .. } => {}
            DshFrame::SessionAdded { .. } | DshFrame::SessionRemoved { .. } => {}
            DshFrame::StreamError { message } => {
                if self.connected {
                    self.connected = false;
                    self.banner = Some(format!("stream error ({message}) — /reconnect to retry"));
                }
            }
            DshFrame::Unknown { .. } => {}
        }
    }

    fn handle_host(&mut self, frame: DshFrame) {
        if let DshFrame::SessionStatus {
            session_id,
            running,
        } = frame
            && self.current_session.as_deref() == Some(session_id.as_str())
        {
            self.running = running;
        }
    }

    fn handle_reply(&mut self, reply: WorkerReply, tasks: &Sender<WorkerTask>) {
        match reply {
            WorkerReply::Restored { session } => match session {
                Some(session) => self.switch_to(session, tasks),
                None => self.flash("no previous session — /new to start one"),
            },
            WorkerReply::History { session, events } => {
                if self.current_session.as_deref() != Some(session.as_str()) {
                    return;
                }
                // 首装整页重建；间隙补齐只追加未见事件。
                let fresh = self.transcript_fresh();
                if fresh {
                    self.transcript.load_history(&events);
                } else {
                    for event in &events {
                        if self.transcript.gap_before(event).is_some() {
                            self.transcript.apply(event);
                        }
                    }
                }
            }
            WorkerReply::Created(session) => {
                self.current_session = Some(session.clone());
                self.transcript = DshTranscript::new();
                self.notices.clear();
                self.flash("session created");
            }
            WorkerReply::Models(entries) => {
                if entries.is_empty() {
                    self.flash("no models available");
                } else {
                    self.picker = Some(Picker::Models {
                        entries,
                        selected: 0,
                    });
                }
            }
            WorkerReply::Status(message) => self.flash(message),
            WorkerReply::Failed(message) => self.flash(format!("error: {message}")),
            WorkerReply::Reconnected(port) => {
                self.port = port;
                self.client = DshClient::new(port);
                match open_downlink(port, "/api/events.mux", &self.ui_sender())
                    .and_then(|()| open_downlink(port, "/api/events.host", &self.ui_sender()))
                {
                    Ok(()) => {
                        self.connected = true;
                        self.banner = None;
                        self.flash("reconnected");
                    }
                    Err(message) => {
                        self.banner = Some(format!("reconnect failed ({message})"));
                    }
                }
            }
        }
    }

    fn transcript_fresh(&self) -> bool {
        self.transcript.model.is_empty()
    }

    fn ui_sender(&self) -> Sender<UiEvent> {
        self.ui.clone()
    }

    fn flash(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), Instant::now()));
    }

    // ---- 渲染 ----

    fn draw(
        &mut self,
        terminal: &mut Terminal<ratatui::backend::CrosstermBackend<Stdout>>,
    ) -> std::io::Result<()> {
        let connected = self.connected;
        let running = self.running;
        let status = self
            .status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(4))
            .map(|(message, _)| message.clone());
        let banner = self.banner.clone();
        let notices = self.notices.len();
        let model_label = format!(
            "{} · {}",
            self.describe
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            self.describe
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("?")
        );
        let session_label = self
            .current_session
            .as_deref()
            .map(|session| &session[session.len().saturating_sub(12)..])
            .unwrap_or("—");
        let input = self.input.clone();
        let approval = self.approval_summary();
        let question = self.question_summary();
        let picker = self.picker_summary();
        terminal
            .draw(|frame| {
                let chunks = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(3),
                ])
                .split(frame.area());
                // 状态行。
                let mut status_line = vec![
                    Span::styled(
                        if connected { "● dsh" } else { "○ dsh" },
                        Style::default().fg(if connected { Color::Green } else { Color::Red }),
                    ),
                    Span::raw(" "),
                    Span::styled(model_label, Style::default().fg(Color::Cyan)),
                    Span::raw("  "),
                    Span::raw(session_label.to_owned()),
                    Span::raw("  "),
                    Span::styled(
                        if running { "running" } else { "idle" },
                        Style::default().fg(if running {
                            Color::Yellow
                        } else {
                            Color::DarkGray
                        }),
                    ),
                ];
                if notices > 0 {
                    status_line.push(Span::styled(
                        format!("  ⚠ {notices} unknown event type(s)"),
                        Style::default().fg(Color::Magenta),
                    ));
                }
                if let Some(status) = status {
                    status_line.push(Span::styled(
                        format!("  {status}"),
                        Style::default().fg(Color::Gray),
                    ));
                }
                frame.render_widget(Paragraph::new(Line::from(status_line)), chunks[0]);
                // 会话区。
                let width = chunks[1].width.saturating_sub(2) as usize;
                let height = chunks[1].height.saturating_sub(2) as usize;
                let total = self
                    .transcript
                    .model
                    .total_lines(ToolCardVisibility::Collapsed);
                if self.scroll_bottom {
                    self.scroll_offset = 0;
                }
                let start = total.saturating_sub(height + self.scroll_offset);
                let lines = self.transcript.model.visible_lines(
                    start,
                    height,
                    width,
                    ToolCardVisibility::Collapsed,
                );
                let mut body: Vec<Line> = Vec::new();
                if let Some(banner) = &banner {
                    body.push(Line::from(Span::styled(
                        format!(" ⚠ {banner}"),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )));
                    body.push(Line::from(""));
                }
                body.extend(lines);
                frame.render_widget(
                    Paragraph::new(body).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" clat dsh — DSH session "),
                    ),
                    chunks[1],
                );
                // 输入框（对话框存在时显示对话框提示行）。
                let input_line = if let Some(text) = &approval {
                    format!(" approval: {text}  [y] allow  [n] deny  ")
                } else if let Some(text) = &question {
                    format!(" question ({text})  ")
                } else if let Some(text) = &picker {
                    format!(" {text}  ↑↓ move · enter select · esc cancel  ")
                } else {
                    format!(" {}", input)
                };
                frame.render_widget(
                    Paragraph::new(input_line).block(Block::default().borders(Borders::ALL).title(
                        if self.running {
                            " steer / esc cancel "
                        } else {
                            " message ( / for commands ) "
                        },
                    )),
                    chunks[2],
                );
            })
            .map(|_| ())
    }

    fn approval_summary(&self) -> Option<String> {
        self.approval.as_ref().map(|pending| {
            format!(
                "{}{}",
                pending.tool_name,
                pending
                    .reason
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>()
            )
        })
    }

    fn question_summary(&self) -> Option<String> {
        self.question.as_ref().map(|pending| {
            let question = &pending.questions[pending.index.min(pending.questions.len() - 1)];
            let text = question
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .enumerate()
                        .map(|(index, option)| {
                            format!(
                                "{}{}",
                                index + 1,
                                option.get("label").and_then(Value::as_str).unwrap_or("?")
                            )
                        })
                        .collect::<String>()
                })
                .unwrap_or_default();
            format!(
                "{}/{} {text} {options} {}",
                pending.index + 1,
                pending.questions.len(),
                pending.input
            )
        })
    }

    fn picker_summary(&self) -> Option<String> {
        match self.picker {
            Some(Picker::Sessions { ref rows, selected }) => rows.get(selected).map(|row| {
                format!(
                    "{} — {}",
                    row.title.as_deref().unwrap_or(&row.session_id),
                    row.workspace_title
                )
            }),
            Some(Picker::Models {
                ref entries,
                selected,
            }) => entries
                .get(selected)
                .map(|(group, _provider, model)| format!("{group} › {model}")),
            None => None,
        }
    }
}

fn open_downlink(port: u16, path: &'static str, ui_tx: &Sender<UiEvent>) -> Result<(), String> {
    let stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|error| format!("cannot connect {path}: {error}"))?;
    let host = format!("127.0.0.1:{port}");
    let (ws_tx, ws_rx) = channel::<WsMessage>();
    ws::connect_downlink(stream, path, &host, ws_tx)?;
    let ui_tx = ui_tx.clone();
    let is_host = path.ends_with("events.host");
    std::thread::spawn(move || {
        while let Ok(message) = ws_rx.recv() {
            match message {
                WsMessage::Text(text) => {
                    let frame = parse_frame(&text);
                    let event = if is_host {
                        UiEvent::Host(frame)
                    } else {
                        UiEvent::Mux(frame)
                    };
                    if ui_tx.send(event).is_err() {
                        return;
                    }
                }
                WsMessage::Closed(reason) | WsMessage::Failed(reason) => {
                    let _ = ui_tx.send(UiEvent::LinkDown(reason));
                    return;
                }
            }
        }
    });
    Ok(())
}

fn spawn_worker(client: DshClient, port: u16, tasks: Receiver<WorkerTask>, ui: Sender<UiEvent>) {
    std::thread::spawn(move || {
        let mut client = client;
        let mut port = port;
        while let Ok(task) = tasks.recv() {
            // 每次 apply 后回写（重连会换 client）。
            let reply = run_task(&task, &mut client, &mut port, &ui);
            if let Some(reply) = reply
                && ui.send(UiEvent::Worker(reply)).is_err()
            {
                return;
            }
        }
    });
}

fn run_task(
    task: &WorkerTask,
    client: &mut DshClient,
    port: &mut u16,
    ui: &Sender<UiEvent>,
) -> Option<WorkerReply> {
    // 重连是特殊路径：可能 spawn，阻塞到就绪，随后本线程换 client 并
    // 通知主循环重开 WS。
    if matches!(task, WorkerTask::Reconnect) {
        let home = files::dsh_home();
        return match crate::dsh::connect::ensure_online(*port, "dsh", home.as_deref()) {
            Ok(online) => {
                *port = online.port;
                *client = DshClient::new(online.port);
                // 主循环重开 WS：借 Ready 帧触发（app 侧对 Ready 的
                // connected 置位 + LinkDown 复位已就绪）。WS 重开需要
                // 主循环执行（channel 归它）——app 收到 Reconnected 后
                // 自行处理。
                Some(WorkerReply::Reconnected(online.port))
            }
            Err(ConnectFailure::Failed(message)) => Some(WorkerReply::Failed(message)),
            Err(ConnectFailure::NotInstalled) => {
                Some(WorkerReply::Failed("dsh is not installed".to_owned()))
            }
        };
    }
    let _ = ui;
    match task {
        WorkerTask::Restore => {
            let list = client.call("session.list", json!({})).ok()?;
            let recent = list
                .get("items")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("blank").and_then(Value::as_bool) != Some(true))
                        .or_else(|| items.first())
                })
                .and_then(|item| item.get("sessionId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(WorkerReply::Restored { session: recent })
        }
        WorkerTask::Prompt {
            session,
            steer,
            text,
        } => {
            let mode = if *steer { "steer" } else { "queue" };
            call_status(
                client,
                "session.prompt",
                json!({
                    "sessionId": session,
                    "mode": mode,
                    "content": [{"type": "text", "text": text}],
                }),
                "prompt sent",
            )
        }
        WorkerTask::Cancel { session } => call_status(
            client,
            "session.cancel",
            json!({"sessionId": session}),
            "cancel sent",
        ),
        WorkerTask::Create { cwd } => {
            let payload = match cwd {
                Some(cwd) => json!({"cwd": cwd}),
                None => json!({}),
            };
            let value = client.call("session.create", payload).ok()?;
            let session = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)?;
            Some(WorkerReply::Created(session))
        }
        WorkerTask::History { session } => {
            let value = client
                .call("session.history", json!({"sessionId": session}))
                .ok()?;
            let mut events = Vec::new();
            if let Some(items) = value.get("events").and_then(Value::as_array) {
                for item in items {
                    if let Ok(event) = serde_json::from_value::<SessionEvent>(
                        item.get("event").cloned().unwrap_or(Value::Null),
                    ) {
                        events.push(event);
                    }
                }
            }
            events.sort_by_key(|event| event.seq);
            Some(WorkerReply::History {
                session: session.clone(),
                events,
            })
        }
        WorkerTask::Models { session } => {
            let Some(session) = session else {
                return Some(WorkerReply::Failed("no active session".to_owned()));
            };
            let value = client
                .call("session.models", json!({"sessionId": session}))
                .ok()?;
            let mut entries = Vec::new();
            if let Some(groups) = value.get("groups").and_then(Value::as_array) {
                for group in groups {
                    let group_name = group.get("name").and_then(Value::as_str).unwrap_or("?");
                    for model in group
                        .get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        entries.push((
                            group_name.to_owned(),
                            value
                                .get("current")
                                .and_then(|current| current.get("provider"))
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_owned(),
                            model
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                                .to_owned(),
                        ));
                    }
                }
            }
            Some(WorkerReply::Models(entries))
        }
        WorkerTask::Select {
            session,
            provider,
            model,
        } => call_status(
            client,
            "session.selectModel",
            json!({"sessionId": session, "provider": provider, "model": model}),
            "model selected",
        ),
        WorkerTask::Rename { session, title } => call_status(
            client,
            "session.rename",
            json!({"sessionId": session, "title": title}),
            "renamed",
        ),
        WorkerTask::Respond { rpc_id, result } => match client.respond(rpc_id, result.clone()) {
            Ok(true) => Some(WorkerReply::Status("answer accepted".to_owned())),
            Ok(false) => Some(WorkerReply::Status(
                "answer not pending (first answer wins)".to_owned(),
            )),
            Err(error) => Some(WorkerReply::Failed(error.to_string())),
        },
        WorkerTask::Reconnect => unreachable!("handled above"),
    }
}

fn call_status(
    client: &DshClient,
    method: &str,
    payload: Value,
    ok_message: &str,
) -> Option<WorkerReply> {
    match client.call(method, payload) {
        Ok(_) => Some(WorkerReply::Status(ok_message.to_owned())),
        Err(error) => Some(WorkerReply::Failed(error.to_string())),
    }
}
