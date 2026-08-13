use crate::presets::preset_by_id;
use crate::providers::ProviderRuntime;
use crate::storage::{Storage, StoredMessage};
use crate::tui_input::InputBuffer;
use crate::tui_markdown::render_markdown;
use crate::tui_model::{EditorAction, ModelEditor};
use crate::tui_worker::{WorkerMessage, execute_run};
use crate::{
    CancelToken, ModelConfig, ModelEvent, ModelItem, PermissionDecision, PermissionRequest,
    Project, RunEvent,
};
use crossterm::event::{
    self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::io::{self, Write, stdout};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChatRole {
    User,
    Assistant,
}

/// Spinner frames for the "thinking" indicator, advancing on every render
/// tick.
const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Official DeepSeek palette used by the harness turn-status shimmer: the
/// text sits in the brand blue while a lighter band glides across it.
const DEEPSEEK_500: Color = Color::Rgb(65, 118, 230);
const DEEPSEEK_200: Color = Color::Rgb(211, 226, 255);

const THINKING_TEXT: &str = "Thinking…";
/// One full band cycle, matching the harness animation duration (1.8s at
/// ~12.5 render ticks per second).
const SHIMMER_CYCLE_TICKS: f64 = 22.5;
/// Softness of the light band, in characters.
const SHIMMER_SIGMA: f64 = 1.2;

/// Builds the animated status line: a rotating spinner plus "Thinking…" in
/// the fixed DeepSeek blue, with a soft light band sweeping front to back.
/// This replicates the harness text-shimmer (a gradient
/// deepseek-500 → deepseek-200 → deepseek-500 gliding across the text and
/// wrapping around), and appends the elapsed thinking time like the
/// harness clock.
fn thinking_line(tick: u64, elapsed: Option<Duration>) -> Line<'static> {
    let frame = SPINNER_FRAMES[(tick as usize) % SPINNER_FRAMES.len()];
    let base = Style::default().fg(DEEPSEEK_500);

    let mut spans = vec![
        Span::styled(frame, base.add_modifier(Modifier::BOLD)),
        Span::styled(" ", base),
    ];

    let text_len = THINKING_TEXT.chars().count() as f64;
    let band_center = (tick as f64 % SHIMMER_CYCLE_TICKS) / SHIMMER_CYCLE_TICKS * text_len;
    for (index, ch) in THINKING_TEXT.chars().enumerate() {
        let position = index as f64 + 0.5;
        let mut distance = (position - band_center).abs();
        if distance > text_len / 2.0 {
            distance = text_len - distance; // the band wraps around the text
        }
        let intensity = (-(distance * distance) / (2.0 * SHIMMER_SIGMA * SHIMMER_SIGMA)).exp();
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(blend_color(DEEPSEEK_500, DEEPSEEK_200, intensity)),
        ));
    }

    if let Some(elapsed) = elapsed {
        spans.push(Span::styled(
            format!(" {:>3}s", elapsed.as_secs()),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Linear blend between two RGB colors; `amount` is in 0..=1.
fn blend_color(from: Color, to: Color, amount: f64) -> Color {
    fn channel(a: u8, b: u8, amount: f64) -> u8 {
        (a as f64 + (b as f64 - a as f64) * amount).round() as u8
    }
    match (from, to) {
        (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) => Color::Rgb(
            channel(fr, tr, amount),
            channel(fg, tg, amount),
            channel(fb, tb, amount),
        ),
        _ => from,
    }
}

/// Rows moved per mouse-wheel notch (and per Up/Down press while those
/// keys scroll the conversation). Claude Code moves about two rows per
/// notch; tune this to taste.
const WHEEL_SCROLL_ROWS: usize = 2;
/// Rows moved per PageUp/PageDown.
const PAGE_SCROLL_ROWS: usize = 8;

/// Input-poll interval while idle: wake often enough to keep animations on
/// schedule without pointlessly spinning the CPU.
const IDLE_POLL: Duration = Duration::from_millis(60);
/// Input-poll interval while the user is active (typing or scrolling), so
/// input feels immediate.
const ACTIVE_POLL: Duration = Duration::from_millis(16);
/// How long after the last input the fast interval is kept before drifting
/// back to the idle interval.
const ACTIVE_HOLD: Duration = Duration::from_secs(6);

/// Adaptive polling: any recent input keeps the fast interval; after a
/// quiet `ACTIVE_HOLD` the loop drifts back to the idle interval.
fn active_poll_interval(last_activity: Option<Instant>) -> Duration {
    match last_activity {
        Some(at) if at.elapsed() < ACTIVE_HOLD => ACTIVE_POLL,
        _ => IDLE_POLL,
    }
}

/// User messages get a soft dark block with bright text, in the spirit of
/// Claude Code, so they stand apart from the model's plain replies. The
/// explicit foreground keeps the block readable on light terminals too.
const USER_BG: Color = Color::Rgb(48, 50, 60);
const USER_FG: Color = Color::Rgb(233, 234, 239);

fn user_message_style() -> Style {
    Style::default().fg(USER_FG).bg(USER_BG)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChatMessage {
    role: ChatRole,
    content: String,
}

impl ChatMessage {
    fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }

    fn from_stored(message: StoredMessage) -> Option<Self> {
        let role = match message.role.as_str() {
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            _ => return None,
        };
        Some(Self {
            role,
            content: message.content,
        })
    }

    fn model_item(&self) -> ModelItem {
        match self.role {
            ChatRole::User => ModelItem::user_text(self.content.clone()),
            ChatRole::Assistant => ModelItem::assistant_text(self.content.clone()),
        }
    }
}

pub fn run(project: Project) -> io::Result<()> {
    let mut terminal = ratatui::init();

    // ratatui::init() 安装的 panic hook 只恢复 raw mode 和备用屏幕，
    // 不覆盖下面手动启用的鼠标模式和 kitty 键盘增强。这里补充一个 hook，
    // 确保这些模式在 panic 时也被清理，避免用户的终端残留异常状态。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture, PopKeyboardEnhancementFlags);
        let _ = stdout().write_all(b"\x1b[?1006l");
        let _ = stdout().flush();
        default_hook(info);
    }));

    // Enable mouse reporting without any-event tracking (1003): CLAT only
    // needs clicks (1000), drags (1002), and the wheel via SGR coordinates
    // (1006). crossterm's EnableMouseCapture also turns 1003 on, and in
    // that mode some terminals report the wheel as motion events that
    // crossterm parses as Moved instead of ScrollUp/ScrollDown.
    let mut out = stdout();
    if let Err(error) = out
        .write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h")
        .and_then(|_| out.flush())
    {
        ratatui::restore();
        return Err(error);
    }
    // Ask supporting terminals (iTerm2, WezTerm, kitty, VS Code, …) to
    // report key modifiers so Shift+Enter is distinguishable from Enter.
    // Terminals without the kitty protocol silently ignore this.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    let result = App::new(project)
        .map_err(io::Error::other)
        .and_then(|mut app| app.run(&mut terminal));

    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let mouse_result = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result.and(mouse_result)
}

struct PendingPermission {
    request: PermissionRequest,
    decision_tx: Sender<PermissionDecision>,
}

struct App {
    project: Project,
    storage: Storage,
    session_id: i64,
    config: ModelConfig,
    provider_runtime: ProviderRuntime,
    messages: Vec<ChatMessage>,
    input: InputBuffer,
    status: String,
    editor: Option<ModelEditor>,
    running: bool,
    receiver: Option<Receiver<WorkerMessage>>,
    pending_permission: Option<PendingPermission>,
    cancel_token: Option<CancelToken>,
    thinking: bool,
    thinking_since: Option<Instant>,
    spinner_tick: u64,
    assistant_message_index: Option<usize>,
    /// Rendered markdown per message, keyed by (index, content length,
    /// width) so stable messages are not re-parsed every frame.
    markdown_cache: HashMap<(usize, usize, usize), Vec<Line<'static>>>,
    conversation_scroll_from_bottom: usize,
    input_area: Rect,
    editor_area: Option<Rect>,
    should_quit: bool,
}

impl App {
    fn new(project: Project) -> Result<Self, String> {
        let storage = Storage::open_default().map_err(|error| error.to_string())?;
        let session_id = storage
            .load_or_create_session(&project)
            .map_err(|error| error.to_string())?;
        let messages = storage
            .load_messages(session_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(ChatMessage::from_stored)
            .collect();
        let history = storage
            .load_input_history(&project, 500)
            .map_err(|error| error.to_string())?;
        let (config, provider_runtime) = storage
            .load_model_state()
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| {
                let config = ModelConfig::default();
                let runtime = ProviderRuntime::for_protocol(config.protocol);
                (config, runtime)
            });
        let status = format!("storage: {}", storage.root().display());

        Ok(Self {
            project,
            storage,
            session_id,
            config,
            provider_runtime,
            messages,
            input: InputBuffer::new(history),
            status,
            editor: None,
            running: false,
            receiver: None,
            pending_permission: None,
            cancel_token: None,
            thinking: false,
            thinking_since: None,
            spinner_tick: 0,
            assistant_message_index: None,
            markdown_cache: HashMap::new(),
            conversation_scroll_from_bottom: 0,
            input_area: Rect::default(),
            editor_area: None,
            should_quit: false,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        // Event-driven loop with adaptive polling. Input is polled on a
        // slow interval while idle, switches to a fast interval as soon as
        // any input arrives, and drifts back after a quiet hold — so a
        // scrolling burst or typing feels immediate without paying for
        // frequent wake-ups while nothing happens. Idle redraws stay on
        // the frame interval so animations keep their designed speed, and
        // a burst of pending events (a fast wheel scroll) is drained and
        // applied in one go, then rendered once.
        const FRAME_INTERVAL: Duration = Duration::from_millis(80);
        let mut last_activity: Option<Instant> = None;
        let mut last_draw = Instant::now() - FRAME_INTERVAL;
        while !self.should_quit {
            self.drain_worker();

            if last_draw.elapsed() >= FRAME_INTERVAL {
                terminal.draw(|frame| self.draw(frame))?;
                last_draw = Instant::now();
            }

            let poll_interval = active_poll_interval(last_activity);
            if event::poll(poll_interval)? {
                loop {
                    match event::read()? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            self.handle_key(key);
                            last_activity = Some(Instant::now());
                        }
                        Event::Paste(text) => {
                            self.handle_paste(&text);
                            last_activity = Some(Instant::now());
                        }
                        Event::Mouse(mouse) => {
                            self.handle_mouse(mouse);
                            last_activity = Some(Instant::now());
                        }
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
                terminal.draw(|frame| self.draw(frame))?;
                last_draw = Instant::now();
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // A permission decision is pending: every key belongs to the dialog
        // until the user allows or denies it.
        if self.pending_permission.is_some() {
            let allow = matches!(
                key.code,
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y')
            );
            let deny = matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
            );
            if (allow || deny)
                && let Some(pending) = self.pending_permission.take()
            {
                let decision = if allow {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny {
                        reason: "denied by user".into(),
                    }
                };
                let _ = pending.decision_tx.send(decision);
                self.status = if allow {
                    "permission granted".into()
                } else {
                    "permission denied — informing the model".into()
                };
            }
            return;
        }

        if let Some(editor) = &mut self.editor {
            let action = editor.handle_key(key);
            self.apply_editor_action(action);
            return;
        }

        match key.code {
            KeyCode::Enter if !self.running => {
                // Claude Code style: Shift+Enter (or Alt+Enter) inserts a
                // line break, plain Enter submits. Ctrl+J is the fallback
                // for terminals that cannot distinguish Shift+Enter.
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                {
                    self.input.insert_newline();
                } else {
                    self.submit_input();
                }
            }
            KeyCode::Char('j')
                if !self.running && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.input.insert_newline();
            }
            KeyCode::Backspace if !self.running => self.input.backspace(),
            KeyCode::Delete if !self.running => self.input.delete(),
            KeyCode::Left if !self.running => self.input.left(),
            KeyCode::Right if !self.running => self.input.right(),
            KeyCode::Home if !self.running => self.input.home(),
            KeyCode::End if !self.running => self.input.end(),
            KeyCode::Up if !self.running => {
                // With no input history to recall, the arrows scroll the
                // conversation instead of doing nothing.
                if self.input.history_is_empty() {
                    self.scroll_up(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_previous();
                }
            }
            KeyCode::Down if !self.running => {
                if self.input.history_is_empty() {
                    self.scroll_down(WHEEL_SCROLL_ROWS);
                } else {
                    self.input.history_next();
                }
            }
            KeyCode::PageUp => self.scroll_up(PAGE_SCROLL_ROWS),
            KeyCode::PageDown => self.scroll_down(PAGE_SCROLL_ROWS),
            KeyCode::Esc => {
                if self.running {
                    if let Some(cancel) = &self.cancel_token {
                        cancel.cancel();
                        self.status = "cancelling…".into();
                    }
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Char(ch) if !self.running => self.input.insert_char(ch),
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.handle_paste(text);
        } else if !self.running {
            self.input.insert_str(text);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let (Some(editor), Some(area)) = (&mut self.editor, self.editor_area) {
            let action = editor.handle_mouse(mouse, area);
            self.apply_editor_action(action);
            return;
        }
        // The wheel always scrolls the conversation, wherever the pointer
        // is: terminals report wheel positions unreliably, and a scoped
        // check would swallow the event.
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_up(WHEEL_SCROLL_ROWS),
            MouseEventKind::ScrollDown => self.scroll_down(WHEEL_SCROLL_ROWS),
            _ => {}
        }
    }

    fn apply_editor_action(&mut self, action: EditorAction) {
        match action {
            EditorAction::Continue => {}
            EditorAction::Cancel => {
                self.editor = None;
                self.status = "model configuration cancelled".into();
            }
            EditorAction::Save(saved) => {
                let (config, runtime) = *saved;
                match self.storage.save_model_state(&config, &runtime) {
                    Ok(()) => {
                        self.config = config;
                        self.provider_runtime = runtime;
                        self.status = format!(
                            "model saved: {} · {}",
                            self.config.protocol, self.config.model
                        );
                        self.editor = None;
                    }
                    Err(error) => {
                        self.status = format!("failed to save model: {error}");
                    }
                }
            }
        }
    }

    fn submit_input(&mut self) {
        let value = self.input.take();
        let value = value.trim().to_owned();
        if value.is_empty() {
            return;
        }
        let _ = self.storage.record_input(&self.project, &value);
        self.input.remember(value.clone());

        match value.as_str() {
            "/model" => {
                self.editor = Some(ModelEditor::new(
                    &self.config,
                    self.provider_runtime.clone(),
                ));
                self.status = "editing model configuration".into();
            }
            "/help" => {
                self.status =
                    "/model · /new · /clear · /quit · ↑/↓ input history · PgUp/PgDn chat".into();
            }
            "/new" | "/clear" => match self.storage.create_session(&self.project) {
                Ok(session_id) => {
                    self.session_id = session_id;
                    self.messages.clear();
                    self.markdown_cache.clear();
                    self.conversation_scroll_from_bottom = 0;
                    self.status = "new conversation".into();
                }
                Err(error) => self.status = format!("failed to create conversation: {error}"),
            },
            "/quit" | "/exit" => self.should_quit = true,
            command if command.starts_with('/') => {
                self.status = format!("unknown command: {command}");
            }
            prompt => self.start_run(prompt.to_owned()),
        }
    }

    fn start_run(&mut self, prompt: String) {
        if !self.config.is_configured() {
            self.status = "model is not configured — run /model first".into();
            return;
        }

        // Build the model context before touching display state, so the new
        // prompt is appended exactly once. Persisted items are the source of
        // truth for context; legacy sessions that only have display messages
        // are seeded from them once.
        let mut history_items = match self.storage.load_items(self.session_id) {
            Ok(items) if !items.is_empty() => items,
            _ => {
                let seeded: Vec<ModelItem> =
                    self.messages.iter().map(ChatMessage::model_item).collect();
                for item in &seeded {
                    let _ = self.storage.append_item(self.session_id, item);
                }
                seeded
            }
        };
        let user_item = ModelItem::user_text(prompt.clone());
        history_items.push(user_item.clone());

        self.messages.push(ChatMessage::user(prompt.clone()));
        if let Err(error) = self
            .storage
            .append_message(self.session_id, "user", &prompt)
        {
            self.status = format!("failed to persist user message: {error}");
        }
        if let Err(error) = self.storage.append_item(self.session_id, &user_item) {
            self.status = format!("failed to persist user context: {error}");
        }
        self.conversation_scroll_from_bottom = 0;

        let project = self.project.clone();
        let config = self.config.clone();
        let provider_runtime = self.provider_runtime.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = CancelToken::new();
        self.receiver = Some(receiver);
        self.cancel_token = Some(cancel.clone());
        self.running = true;
        self.assistant_message_index = None;
        self.status = "starting model…".into();

        thread::spawn(move || {
            let result = execute_run(
                project,
                config,
                provider_runtime,
                history_items,
                prompt,
                sender.clone(),
                cancel,
            );
            let _ = sender.send(WorkerMessage::Done(result));
        });
    }

    fn drain_worker(&mut self) {
        loop {
            let message = match &self.receiver {
                Some(receiver) => receiver.try_recv(),
                None => return,
            };
            match message {
                Ok(WorkerMessage::Event(event)) => self.handle_run_event(event),
                Ok(WorkerMessage::PermissionRequest {
                    request,
                    decision_tx,
                }) => {
                    self.thinking = false;
                    self.thinking_since = None;
                    self.pending_permission = Some(PendingPermission {
                        request,
                        decision_tx,
                    });
                    self.status = "permission required — allow or deny".into();
                }
                Ok(WorkerMessage::Done(result)) => {
                    self.finish_run(result);
                    return;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.running = false;
                    self.receiver = None;
                    self.status = "run worker disconnected unexpectedly".into();
                    return;
                }
            }
        }
    }

    fn handle_run_event(&mut self, event: RunEvent) {
        let was_thinking = self.thinking;
        self.thinking = false;
        match event {
            RunEvent::ModelRequested {
                turn,
                provider,
                model,
            } => {
                self.status = format!("{provider}/{model} · turn {turn}");
            }
            RunEvent::ModelStream {
                turn,
                event: ModelEvent::TextDelta { delta },
            }
            | RunEvent::ModelStream {
                turn,
                event: ModelEvent::RefusalDelta { delta },
            } => {
                self.append_assistant_delta(&delta);
                self.status = format!("answering · turn {turn}");
            }
            RunEvent::ModelStream {
                event: ModelEvent::ReasoningDelta { .. },
                ..
            } => {
                if !was_thinking {
                    self.thinking_since = Some(Instant::now());
                }
                self.thinking = true;
            }
            RunEvent::ToolRequested { call } => {
                self.status = format!("tool → {} {}", call.name, call.arguments);
            }
            RunEvent::PermissionDenied { tool, reason } => {
                self.status = format!("permission ✗ {tool} — {reason}");
            }
            RunEvent::ToolFinished { result } => {
                self.status = if result.is_error {
                    format!("tool ✗ {}", result.tool_name)
                } else {
                    format!("tool ✓ {}", result.tool_name)
                };
            }
            _ => {}
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        let index = match self.assistant_message_index {
            Some(index) => index,
            None => {
                self.messages.push(ChatMessage::assistant(String::new()));
                let index = self.messages.len() - 1;
                self.assistant_message_index = Some(index);
                index
            }
        };
        self.messages[index].content.push_str(delta);
        self.conversation_scroll_from_bottom = 0;
    }

    fn finish_run(&mut self, result: Result<crate::tui_worker::RunDone, String>) {
        self.running = false;
        self.receiver = None;
        self.cancel_token = None;
        self.thinking = false;
        self.thinking_since = None;
        match result {
            Ok(done) => {
                if self.assistant_message_index.is_none() && !done.output.trim().is_empty() {
                    self.messages.push(ChatMessage::assistant(done.output));
                    self.assistant_message_index = Some(self.messages.len() - 1);
                }
                self.persist_current_assistant(false);
                self.persist_items(done.new_items);
                self.status = if done.cancelled {
                    format!("cancelled · {} model turns", done.turns)
                } else {
                    format!("completed · {} model turns", done.turns)
                };
            }
            Err(error) => {
                // A failed run has no item list, so persist whatever partial
                // assistant text was streamed before the failure.
                self.persist_current_assistant(true);
                self.status = format!("run failed: {error}");
            }
        }
        self.assistant_message_index = None;
        self.conversation_scroll_from_bottom = 0;
    }

    fn persist_items(&mut self, items: Vec<ModelItem>) {
        for item in items {
            if let Err(error) = self.storage.append_item(self.session_id, &item) {
                self.status = format!("failed to persist conversation context: {error}");
            }
        }
    }

    fn persist_current_assistant(&mut self, also_item: bool) {
        let Some(index) = self.assistant_message_index else {
            return;
        };
        let content = self.messages[index].content.clone();
        if content.trim().is_empty() {
            return;
        }
        if let Err(error) = self
            .storage
            .append_message(self.session_id, "assistant", &content)
        {
            self.status = format!("failed to persist assistant message: {error}");
        }
        if also_item {
            let item = ModelItem::assistant_text(content);
            if let Err(error) = self.storage.append_item(self.session_id, &item) {
                self.status = format!("failed to persist assistant context: {error}");
            }
        }
    }

    fn scroll_up(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom = self
            .conversation_scroll_from_bottom
            .saturating_add(amount)
            .min(10_000);
    }

    fn scroll_down(&mut self, amount: usize) {
        self.conversation_scroll_from_bottom =
            self.conversation_scroll_from_bottom.saturating_sub(amount);
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.spinner_tick += 1;
        // The input box grows with the number of wrapped lines, up to
        // eight content rows, Claude Code style.
        let input_width = area.width.saturating_sub(2).max(1) as usize;
        let input_rows = (self.input.line_count(input_width) + 2).clamp(3, 10);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(input_rows as u16),
                Constraint::Length(1),
            ])
            .split(area);
        self.input_area = chunks[2];

        self.draw_header(frame, chunks[0]);
        self.draw_conversation(frame, chunks[1]);
        self.draw_input(frame, chunks[2]);
        if self.thinking {
            let elapsed = self.thinking_since.map(|since| since.elapsed());
            frame.render_widget(
                Paragraph::new(thinking_line(self.spinner_tick, elapsed)),
                chunks[3],
            );
        } else {
            frame.render_widget(Paragraph::new(self.status.as_str()), chunks[3]);
        }

        if let Some(editor) = &self.editor {
            let height = (editor.row_count() as u16 + 4).min(area.height.saturating_sub(2));
            let editor_area = centered_rect(94, height.max(8), area);
            self.editor_area = Some(editor_area);
            editor.draw(frame, editor_area);
        } else {
            self.editor_area = None;
            if !self.running && self.input_area.width > 2 && self.input_area.height > 2 {
                let width = self.input_area.width.saturating_sub(2) as usize;
                let (row, column) = self.input.cursor_position(width);
                let visible_rows = self.input_area.height.saturating_sub(2) as usize;
                let row = row.min(visible_rows.saturating_sub(1));
                frame.set_cursor_position((
                    self.input_area.x + 1 + column as u16,
                    self.input_area.y + 1 + row as u16,
                ));
            }
        }

        if let Some(pending) = &self.pending_permission {
            self.draw_permission_dialog(frame, pending);
        }
    }

    fn draw_permission_dialog(&self, frame: &mut Frame, pending: &PendingPermission) {
        let area = frame.area();
        let inner_width = 84usize.saturating_sub(2);
        let mut lines = vec![
            Line::from(Span::styled(
                "Permission required",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(format!("tool:      {}", pending.request.tool)),
            Line::from(format!("effect:    {}", pending.request.effect)),
            Line::from(format!("reason:    {}", pending.request.reason)),
            Line::from("arguments:"),
        ];
        let pretty = serde_json::to_string_pretty(&pending.request.arguments)
            .unwrap_or_else(|_| "<unavailable>".into());
        for source_line in pretty.split('\n').take(8) {
            for wrapped in wrap_text(source_line, inner_width.saturating_sub(2)) {
                lines.push(Line::from(format!("  {wrapped}")));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Enter / y — allow      ·      Esc / n — deny",
            Style::default().add_modifier(Modifier::BOLD),
        )));

        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
        let dialog = centered_rect(84, height.max(10), area);
        frame.render_widget(Clear, dialog);
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(" Permission ")),
            dialog,
        );
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let model = if self.config.is_configured() {
            match self.config.preset.as_deref().and_then(preset_by_id) {
                Some(preset) => format!("{} · {}", preset.name, self.config.model),
                None => format!("{} · {}", self.config.protocol, self.config.model),
            }
        } else {
            "not configured — /model".into()
        };
        let state = if self.running { "running" } else { "ready" };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("CLAT", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  {state}  ·  {model}")),
                ]),
                Line::from(format!("project: {}", self.project.root().display())),
            ])
            .block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn draw_conversation(&mut self, frame: &mut Frame, area: Rect) {
        let inner_width = area.width.saturating_sub(2).max(1) as usize;
        let lines = self.conversation_lines(inner_width);
        let total = lines.len();
        let visible = area.height.saturating_sub(2) as usize;
        let max_start = total.saturating_sub(visible);
        let start = max_start.saturating_sub(self.conversation_scroll_from_bottom.min(max_start));
        let visible_lines = lines
            .into_iter()
            .skip(start)
            .take(visible)
            .collect::<Vec<_>>();
        let block = Block::default().title("Conversation").borders(Borders::ALL);
        frame.render_widget(
            Paragraph::new(Text::from(visible_lines)).block(block.clone()),
            area,
        );

        let mut scrollbar_state = ScrollbarState::new(total)
            .position(scrollbar_position(start, max_start, total))
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol("┃")
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .style(Style::default().fg(Color::DarkGray))
                .thumb_style(Style::default().fg(Color::Cyan)),
            block.inner(area),
            &mut scrollbar_state,
        );
    }

    /// Builds all conversation lines, caching the per-message markdown
    /// rendering so a long history is not re-parsed on every frame. Only
    /// the streaming message (whose length changes) and resized panels
    /// miss the cache.
    fn conversation_lines(&mut self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if self.messages.is_empty() {
            lines.push(Line::from("No messages yet."));
            lines.push(Line::from(""));
            return lines;
        }
        for index in 0..self.messages.len() {
            let key = (index, self.messages[index].content.len(), width);
            if let Some(cached) = self.markdown_cache.get(&key) {
                lines.extend(cached.clone());
            } else {
                let rendered = message_lines(&self.messages[index], width);
                self.markdown_cache.insert(key, rendered.clone());
                lines.extend(rendered);
            }
            // One uniform blank row after every message, including the
            // last, keeps the spacing regular and the panel bottom clear.
            lines.push(Line::from(""));
        }
        lines
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        let text = self.input.text().to_owned();
        let title = if self.running { "Running" } else { "Message" };
        frame.render_widget(
            Paragraph::new(text)
                .block(Block::default().title(title).borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

/// Renders one message into display lines, Claude Code style: user
/// messages are a solid background block hugging the text exactly,
/// prefixed with a bright yellow `❯`; assistant messages are plain
/// markdown text prefixed with `⏺`. Spacing between messages is handled
/// uniformly by the caller, never by padding rows inside the block.
fn message_lines(message: &ChatMessage, width: usize) -> Vec<Line<'static>> {
    let text_width = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    match message.role {
        ChatRole::User => {
            let style = user_message_style();
            let marker = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
            let wrapped = wrap_text(&message.content, text_width.saturating_sub(2).max(1));
            for (index, line) in wrapped.iter().enumerate() {
                let (prefix, prefix_style) = if index == 0 {
                    ("❯ ", marker)
                } else {
                    ("  ", style)
                };
                let used = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(line.as_str());
                let padding = " ".repeat(width.saturating_sub(used));
                lines.push(Line::from(vec![
                    Span::styled(prefix.to_owned(), prefix_style),
                    Span::styled(line.clone(), style),
                    Span::styled(padding, style),
                ]));
            }
        }
        ChatRole::Assistant => {
            let marker = Span::styled("⏺ ", Style::default().fg(Color::Gray));
            for (index, line) in render_markdown(&message.content, text_width)
                .into_iter()
                .enumerate()
            {
                let mut spans = vec![if index == 0 {
                    marker.clone()
                } else {
                    Span::raw("  ")
                }];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

/// Maps the first visible row (`start`, in 0..=max_start) to ratatui's
/// scrollbar position domain 0..=content_length-1, where 0 puts the thumb
/// at the very top and content_length-1 at the very bottom. Passing the
/// raw row index leaves the thumb short of the bottom by one viewport.
fn scrollbar_position(start: usize, max_start: usize, content_length: usize) -> usize {
    if max_start == 0 {
        return 0;
    }
    let domain = content_length.saturating_sub(1);
    (start.saturating_mul(domain) / max_start).min(domain)
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result = Vec::new();
    for source_line in text.split('\n') {
        if source_line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0usize;
        for ch in source_line.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > width {
                result.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        result.push(current);
    }
    result
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let top = area.height.saturating_sub(height) / 2;
    let vertical = Rect::new(area.x, area.y + top, area.width, height.min(area.height));
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical);
    horizontal[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_cjk_by_terminal_width() {
        assert_eq!(wrap_text("你好世界", 4), vec!["你好", "世界"]);
    }

    #[test]
    fn converts_stored_messages_to_chat() {
        let message = ChatMessage::from_stored(StoredMessage {
            role: "user".into(),
            content: "hello".into(),
        })
        .unwrap();
        assert_eq!(message.role, ChatRole::User);
        assert_eq!(message.content, "hello");
    }

    #[test]
    fn thinking_line_shimmers_a_soft_band_over_the_fixed_tone() {
        // Extract the (character, style) pairs of the text part; the first
        // two spans are the spinner frame and the separating space.
        fn text_spans(line: &Line<'static>) -> Vec<(char, Style)> {
            line.spans[2..]
                .iter()
                .map(|span| {
                    (
                        span.content.chars().next().expect("one char per span"),
                        span.style,
                    )
                })
                .collect()
        }

        let at_start = thinking_line(0, None);
        let text = text_spans(&at_start);
        // The spinner frame itself rotates and keeps the brand blue.
        assert_eq!(at_start.spans[0].content, SPINNER_FRAMES[0]);
        assert_eq!(at_start.spans[0].style.fg, Some(DEEPSEEK_500));

        // The band sits on the seam at the start: both ends glow while the
        // middle stays (approximately) in the fixed tone.
        let brightness = |style: &Style| match style.fg {
            Some(Color::Rgb(r, g, b)) => r as u32 + g as u32 + b as u32,
            _ => 0,
        };
        let close_to_base = |color: Color| match color {
            Color::Rgb(r, g, b) => {
                let (br, bg, bb) = (65u8, 118u8, 230u8);
                r.abs_diff(br) <= 2 && g.abs_diff(bg) <= 2 && b.abs_diff(bb) <= 2
            }
            _ => false,
        };
        assert!(brightness(&text[0].1) > brightness(&text[4].1));
        assert!(close_to_base(text[4].1.fg.unwrap_or_default()));

        // Mid-cycle the band has moved to the middle: the brightest
        // character is 'k', and its color approaches the light deepseek-200.
        let mid = thinking_line(11, None);
        let text = text_spans(&mid);
        let brightest = text
            .iter()
            .max_by_key(|(_, style)| brightness(style))
            .expect("text");
        assert_eq!(brightest.0, 'k');
        match brightest.1.fg {
            Some(Color::Rgb(r, g, b)) => {
                assert!(r > 190 && g > 210 && b > 240, "band color {r},{g},{b}");
            }
            other => panic!("expected RGB, got {other:?}"),
        }

        // Far from the band the text is (approximately) the base blue.
        assert!(close_to_base(text[0].1.fg.unwrap_or_default()));

        // The elapsed clock is appended when known.
        let with_clock = thinking_line(0, Some(Duration::from_secs(42)));
        let last = with_clock.spans.last().expect("clock span");
        assert!(last.content.contains("42s"));
    }

    #[test]
    fn scrollbar_position_reaches_both_ends() {
        // 100 content rows, 20 visible: start runs 0..=80.
        assert_eq!(scrollbar_position(0, 80, 100), 0);
        assert_eq!(scrollbar_position(80, 80, 100), 99);
        // Proportional in between.
        assert_eq!(scrollbar_position(40, 80, 100), 49);
        // Nothing to scroll: stays at the top.
        assert_eq!(scrollbar_position(0, 0, 5), 0);
    }

    #[test]
    fn adaptive_polling_fast_after_input_and_decays_after_hold() {
        assert_eq!(active_poll_interval(None), IDLE_POLL);
        let recent = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(active_poll_interval(recent), ACTIVE_POLL);
        let near_boundary = Some(Instant::now() - ACTIVE_HOLD + Duration::from_millis(10));
        assert_eq!(active_poll_interval(near_boundary), ACTIVE_POLL);
        let stale = Some(Instant::now() - ACTIVE_HOLD - Duration::from_millis(10));
        assert_eq!(active_poll_interval(stale), IDLE_POLL);
    }

    #[test]
    fn user_messages_render_as_a_full_width_background_block() {
        let message = ChatMessage::user("hello");
        let lines = message_lines(&message, 12);

        // One text row only: the block hugs the text exactly, with a
        // bright yellow "❯ " marker, padded to the full width.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "❯ ");
        assert_eq!(lines[0].spans[0].style.bg, Some(USER_BG));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(lines[0].spans[1].content, "hello");
        assert_eq!(lines[0].spans[1].style.bg, Some(USER_BG));
        let total_width: usize = lines[0]
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        assert_eq!(total_width, 12);
    }

    #[test]
    fn assistant_messages_render_markdown_without_background() {
        let message = ChatMessage::assistant("**bold** and `code`");
        let lines = message_lines(&message, 30);
        // The "⏺" marker prefixes the first line, no background.
        assert_eq!(lines[0].spans[0].content, "⏺ ");
        assert!(lines[0].spans[0].style.bg.is_none());
        // Bold and inline-code segments survive.
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "bold"
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "code" && span.style.bg.is_some())
        );
    }
}
