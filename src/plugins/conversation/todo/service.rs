//! Domain-owned conversation state and contracts.
use crate::session::id::SessionId;
use crate::session::run_journal::RunJournal;
use std::sync::Arc;

/// 会话 todo 状态（事件原生版，plan §13.3）。内存快照只是投影的运行时
/// 镜像；持久化只经 `todo/write` 事件——`write` 在活动 Run 绑定的
/// RunJournal 上追加事件，恢复从 todo 投影读取。

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TodoEntry {
    pub content: String,
    pub status: TodoStatus,
}

pub(crate) struct TodoService {
    inner: std::sync::Mutex<TodoInner>,
    /// Serialize append→flush→publish as one commit lane. Parallel
    /// tool calls must not publish an older list after a newer event has
    /// already committed to the same journal.
    write_lane: std::sync::Mutex<()>,
}

struct TodoInner {
    /// 当前内存快照所属会话（restore/ensure 设置）；None = 未挂靠任何
    /// 会话（/new 后）。
    session: Option<SessionId>,
    /// 活动 Run 绑定（CB1-06）：write 只在绑定存在且与会话一致时可用。
    active_run: Option<(SessionId, Arc<dyn RunJournal>)>,
    entries: Vec<TodoEntry>,
}

impl TodoService {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(TodoInner {
                session: None,
                active_run: None,
                entries: Vec::new(),
            }),
            write_lane: std::sync::Mutex::new(()),
        }
    }

    /// 绑定活动 Run：只有当快照确实属于该会话时才允许（防止错位写入）。
    /// 未绑定成功时 write 拒绝（INV-T3 由行为而非注释保证）。
    pub(crate) fn bind_run(&self, session: &SessionId, journal: Arc<dyn RunJournal>) -> bool {
        let mut inner = self.inner.lock().expect("todo lock");
        if inner.session.as_ref() == Some(session) {
            inner.active_run = Some((session.clone(), journal));
            true
        } else {
            inner.active_run = None;
            false
        }
    }

    pub(crate) fn unbind(&self) {
        self.inner.lock().expect("todo lock").active_run = None;
    }

    /// The session the snapshot is attached to (None after `/new`).
    pub(crate) fn session(&self) -> Option<SessionId> {
        self.inner.lock().expect("todo lock").session.clone()
    }

    /// 从 todo 投影恢复快照并挂靠会话（INV-T5）。restore 语义就是
    /// "丢弃内存态换成目标会话的投影事实"。
    pub(crate) fn restore(&self, session: Option<SessionId>, entries: &[TodoEntry]) {
        let mut inner = self.inner.lock().expect("todo lock");
        inner.session = session;
        inner.active_run = None;
        inner.entries = validate_todos(entries).unwrap_or_else(|_| Vec::new());
    }

    /// 校验并全量替换清单（INV-T2/E.2 规则），经绑定的 RunJournal 追加
    /// 一条 `todo/write` 事件。需要活动 Run 绑定（CB1-06）：无绑定一律
    /// 拒绝，防止内存态永远不落盘。
    ///
    /// Audit P1-10: the durable event (append + flush) must commit BEFORE
    /// the in-memory projection publishes the new list. A failed append
    /// leaves the old entries untouched, so a retried write of the same
    /// todos is never mistaken for the idempotent no-op — and the model
    /// never sees todo state the log does not hold.
    pub(crate) fn write(&self, todos: &[TodoEntry]) -> Result<Vec<TodoEntry>, String> {
        let _lane = self.write_lane.lock().expect("todo write lane");
        let entries = validate_todos(todos)?;
        let (bound_session, journal) = {
            let inner = self.inner.lock().expect("todo lock");
            let Some((session, journal)) = inner.active_run.clone() else {
                return Err("todo_write requires an active run in this session".into());
            };
            if inner.entries == entries {
                // Idempotent no-op, judged against the *published* state:
                // a failed write below never mutates it, so this fast path
                // can never swallow an unwritten change.
                return Ok(entries);
            }
            (session, journal)
        };
        let payload_todos: Vec<(String, &'static str)> = entries
            .iter()
            .map(|entry| (entry.content.clone(), entry.status.as_str()))
            .collect();
        let event = crate::session::run_journal::NewSessionEvent::new(
            "todo/write",
            crate::session::event::payloads::todo_write(&payload_todos),
        )
        .log_only();
        journal.append(event)?;
        // Durability before publication: the tool result reports success
        // only once the event is on disk.
        journal.flush()?;
        let mut inner = self.inner.lock().expect("todo lock");
        // The bound run cannot change while we hold no lock only if the
        // caller honors one-run-at-a-time; publish under the current
        // binding's session, restoring nothing if it moved.
        let binding_unchanged = inner.active_run.as_ref().is_some_and(|(session, current)| {
            *session == bound_session && Arc::ptr_eq(current, &journal)
        }) && inner.session.as_ref() == Some(&bound_session);
        if binding_unchanged {
            inner.entries = entries.clone();
        } else {
            return Err(
                "todo event committed, but the active session binding changed; reload the session"
                    .into(),
            );
        }
        Ok(entries)
    }

    /// 注入模型视图的动态上下文（纯内容，无标题包装——由视图构建方唯
    /// 一次加边界）；空清单返回 None。非耐久请求组装的一部分。
    pub(crate) fn model_context(&self) -> Option<String> {
        let inner = self.inner.lock().expect("todo lock");
        if inner.entries.is_empty() {
            return None;
        }
        let mut text = String::from("Current todo list:");
        for entry in &inner.entries {
            text.push_str(&format!(
                "\n- [{}] {}",
                entry.status.as_str(),
                entry.content
            ));
        }
        Some(text)
    }

    /// Read-only model-maintained todo projection for tests and workflow views.
    pub(crate) fn snapshot(&self) -> Vec<TodoEntry> {
        self.inner.lock().expect("todo lock").entries.clone()
    }
}

/// 写入与恢复共用的校验（CB1-08 对称性）。
fn validate_todos(todos: &[TodoEntry]) -> Result<Vec<TodoEntry>, String> {
    if todos.len() > 50 {
        return Err("todo list is limited to 50 entries".into());
    }
    let mut in_progress = 0;
    let mut entries = Vec::with_capacity(todos.len());
    for todo in todos {
        let content = todo.content.trim();
        if content.is_empty() {
            return Err("todo content must not be empty".into());
        }
        if content.chars().count() > 500 {
            return Err("todo content is limited to 500 characters".into());
        }
        if todo.status == TodoStatus::InProgress {
            in_progress += 1;
            if in_progress > 1 {
                return Err("only one todo entry may be in_progress".into());
            }
        }
        entries.push(TodoEntry {
            content: content.to_owned(),
            status: todo.status,
        });
    }
    Ok(entries)
}
