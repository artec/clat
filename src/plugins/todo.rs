//! todo 工具（事件原生版，plan §13.3）。
//!
//! `TodoWriteTool` 声明 `ToolEffect::SessionWrite`：只改本会话的本地
//! 元数据，SafeByDefault 免审——免审来自准确的 effect 分类而非工具名
//! 特例。执行期在绑定的 RunJournal 上追加一条 `todo/write` 事件；恢复
//! 走 todo 投影，内存快照只是投影的运行时镜像。

use super::services::{
    SESSION_SERVICE_ID, TODO_SERVICE, TODO_SERVICE_ID, TOOL_SERVICE, TOOL_SERVICE_ID, TodoEntry,
    TodoService, TodoStatus,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.todo");
const REQUIRES: &[ServiceId] = &[SESSION_SERVICE_ID, TOOL_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[TODO_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct TodoPlugin;

impl Plugin for TodoPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = Arc::new(TodoService::new());
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let lease = tools
            .register(
                context.owner(),
                Arc::new(TodoWriteTool {
                    service: Arc::clone(&service),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(TODO_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct TodoWriteTool {
    service: Arc<TodoService>,
}

impl Tool for TodoWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_write".into(),
            description: "Replace this session's todo list. Submit the complete list every \
                 time (full-replacement semantics); entries omitted are dropped."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                },
                            },
                            "required": ["content", "status"],
                            "additionalProperties": false,
                        },
                    },
                },
                "required": ["todos"],
                "additionalProperties": false,
            }),
            effect: ToolEffect::SessionWrite,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let todos = parse_entries(arguments).map_err(ToolError::new)?;
        let entries = self.service.write(&todos).map_err(ToolError::new)?;
        Ok(json!({
            "ok": true,
            "todos": entries
                .iter()
                .map(|entry| {
                    json!({
                        "content": entry.content,
                        "status": entry.status.as_str(),
                    })
                })
                .collect::<Vec<_>>(),
        }))
    }
}

fn parse_entries(arguments: &Value) -> Result<Vec<TodoEntry>, String> {
    let todos = arguments
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| "todo_write requires a `todos` array".to_owned())?;
    let mut entries = Vec::with_capacity(todos.len());
    for todo in todos {
        let content = todo
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "each todo requires string `content`".to_owned())?
            .to_owned();
        let status = todo
            .get("status")
            .and_then(Value::as_str)
            .and_then(TodoStatus::parse)
            .ok_or_else(|| "todo `status` must be pending/in_progress/completed".to_owned())?;
        entries.push(TodoEntry { content, status });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::run_journal::{NewSessionEvent, RunJournal, SeqRange};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn entry(content: &str, status: TodoStatus) -> TodoEntry {
        TodoEntry {
            content: content.into(),
            status,
        }
    }

    /// 记录事件供断言的测试 journal；`fail_appends` 可注入持久化失败。
    #[derive(Default)]
    struct RecordingJournal {
        events: Mutex<Vec<NewSessionEvent>>,
        fail_appends: bool,
    }

    impl RunJournal for RecordingJournal {
        fn append_atomic(&self, events: &[NewSessionEvent]) -> Result<SeqRange, String> {
            if self.fail_appends {
                return Err("injected journal failure".into());
            }
            self.events.lock().unwrap().extend(events.iter().cloned());
            Ok(SeqRange {
                start: 0,
                end_inclusive: 0,
            })
        }
        fn flush(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn recording_journal() -> (Arc<RecordingJournal>, Arc<dyn RunJournal>) {
        let journal = Arc::new(RecordingJournal::default());
        (
            Arc::clone(&journal),
            Arc::clone(&journal) as Arc<dyn RunJournal>,
        )
    }

    fn failing_journal() -> Arc<RecordingJournal> {
        Arc::new(RecordingJournal {
            events: Mutex::new(Vec::new()),
            fail_appends: true,
        })
    }

    fn session(id: u64) -> crate::SessionId {
        crate::SessionId::new(format!("session-{id}"))
    }

    /// CB1-06/INV-T3：绑定后才允许写入；未绑定/解绑后/错会话一律拒绝。
    fn bound_service(id: u64) -> (TodoService, Arc<RecordingJournal>) {
        let service = TodoService::new();
        service.restore(Some(session(id)), &[]);
        let (recording, journal) = recording_journal();
        assert!(
            service.bind_run(&session(id), journal),
            "binding must match the session"
        );
        (service, recording)
    }

    #[test]
    fn write_requires_an_active_run_binding() {
        let service = TodoService::new();
        // 从未绑定（模拟绕过 application 编排的直接消费方）。
        let error = service
            .write(&[entry("x", TodoStatus::Pending)])
            .expect_err("unbound write must fail");
        assert!(error.contains("active run"));
        // 恢复到会话但尚未 bind（run 间隙）。
        service.restore(Some(session(7)), &[]);
        assert!(service.write(&[entry("x", TodoStatus::Pending)]).is_err());
        // bind 到**另一个**会话：拒绝。
        let (_, journal) = recording_journal();
        assert!(
            !service.bind_run(&session(8), journal),
            "binding a foreign session must fail"
        );
        assert!(service.write(&[entry("x", TodoStatus::Pending)]).is_err());
        // 正确 bind：可写；unbind 后再写拒绝。
        let (_, journal) = recording_journal();
        assert!(service.bind_run(&session(7), journal));
        service
            .write(&[entry("x", TodoStatus::Pending)])
            .expect("bound write succeeds");
        service.unbind();
        assert!(service.write(&[entry("y", TodoStatus::Pending)]).is_err());
    }

    #[test]
    fn write_validates_appends_an_event_and_updates_the_snapshot() {
        let (service, journal) = bound_service(1);
        let entries = service
            .write(&[
                entry("first", TodoStatus::Completed),
                entry("second", TodoStatus::InProgress),
            ])
            .expect("write");
        assert_eq!(entries.len(), 2);
        let recorded = journal.events.lock().unwrap();
        assert_eq!(recorded.len(), 1, "exactly one todo/write event");
        assert_eq!(recorded[0].event_type, "todo/write");
        assert_eq!(recorded[0].data["todos"][0]["status"], "completed");
        drop(recorded);
        assert!(service.model_context().is_some());
        // 校验规则：两个 in_progress 拒绝；空内容拒绝；超长拒绝。
        assert!(
            service
                .write(&[
                    entry("a", TodoStatus::InProgress),
                    entry("b", TodoStatus::InProgress),
                ])
                .is_err()
        );
        assert!(service.write(&[entry("  ", TodoStatus::Pending)]).is_err());
        assert!(
            service
                .write(&[entry(&"x".repeat(501), TodoStatus::Pending)])
                .is_err()
        );
        assert!(
            service
                .write(
                    &(0..51)
                        .map(|_| entry("t", TodoStatus::Pending))
                        .collect::<Vec<_>>()
                )
                .is_err()
        );
        // 拒绝的写入不产生事件、不改变快照。
        assert_eq!(journal.events.lock().unwrap().len(), 1);
        assert_eq!(service.snapshot().len(), 2);
        // 相同内容重复写入幂等：不追加事件。
        service
            .write(&[
                entry("first", TodoStatus::Completed),
                entry("second", TodoStatus::InProgress),
            ])
            .expect("idempotent write");
        assert_eq!(journal.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn empty_list_write_clears_context_and_persists_an_empty_event() {
        let (service, journal) = bound_service(2);
        service
            .write(&[entry("only", TodoStatus::Pending)])
            .expect("write");
        service.write(&[]).expect("clear");
        assert!(service.model_context().is_none());
        let recorded = journal.events.lock().unwrap();
        assert_eq!(
            recorded[1].data,
            json!({"todos": []}),
            "explicit clearing must persist an empty todo/write event"
        );
    }

    #[test]
    fn failed_append_leaves_the_published_snapshot_untouched_and_retry_writes() {
        let service = TodoService::new();
        service.restore(Some(session(11)), &[entry("old", TodoStatus::Pending)]);
        service
            .bind_run(&session(11), failing_journal() as Arc<dyn RunJournal>)
            .then_some(())
            .expect("bind");
        // 修复前：内存快照先于 append 更新——失败后同一批 todos 会被
        // 幂等快路吞掉，事件永远不落盘（审计 P1-10 的失败序列）。
        assert!(
            service.write(&[entry("new", TodoStatus::Pending)]).is_err(),
            "the write must fail with the journal"
        );
        assert_eq!(
            service.snapshot(),
            vec![entry("old", TodoStatus::Pending)],
            "a failed write must not publish new todo state"
        );
        assert!(service.model_context().unwrap().contains("old"));
        // Retry the same todos through a healthy journal: the event lands.
        let (healthy, journal) = recording_journal();
        service.bind_run(&session(11), journal);
        service
            .write(&[entry("new", TodoStatus::Pending)])
            .expect("retry succeeds");
        let recorded = healthy.events.lock().unwrap();
        assert_eq!(recorded.len(), 1, "the retried write appends its event");
        assert_eq!(recorded[0].data["todos"][0]["content"], "new");
        drop(recorded);
        assert_eq!(service.snapshot(), vec![entry("new", TodoStatus::Pending)]);
    }

    struct OverlapDetectingJournal {
        active_writes: AtomicUsize,
        overlap: AtomicBool,
        first_append_seen: AtomicBool,
    }

    impl RunJournal for OverlapDetectingJournal {
        fn append_atomic(&self, _events: &[NewSessionEvent]) -> Result<SeqRange, String> {
            if self.active_writes.fetch_add(1, Ordering::SeqCst) != 0 {
                self.overlap.store(true, Ordering::SeqCst);
            }
            self.first_append_seen.store(true, Ordering::SeqCst);
            Ok(SeqRange {
                start: 0,
                end_inclusive: 0,
            })
        }

        fn flush(&self) -> Result<(), String> {
            // Keep the append→flush operation open long enough for a
            // parallel caller to overlap on the pre-fix implementation.
            std::thread::sleep(std::time::Duration::from_millis(40));
            self.active_writes.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn concurrent_todo_writes_share_one_commit_and_publish_lane() {
        let service = Arc::new(TodoService::new());
        service.restore(Some(session(12)), &[]);
        let journal = Arc::new(OverlapDetectingJournal {
            active_writes: AtomicUsize::new(0),
            overlap: AtomicBool::new(false),
            first_append_seen: AtomicBool::new(false),
        });
        assert!(service.bind_run(&session(12), Arc::clone(&journal) as Arc<dyn RunJournal>));

        let first_service = Arc::clone(&service);
        let first =
            std::thread::spawn(move || first_service.write(&[entry("first", TodoStatus::Pending)]));
        while !journal.first_append_seen.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        let second_service = Arc::clone(&service);
        let second = std::thread::spawn(move || {
            second_service.write(&[entry("second", TodoStatus::Pending)])
        });
        first.join().unwrap().expect("first write");
        second.join().unwrap().expect("second write");

        assert!(
            !journal.overlap.load(Ordering::SeqCst),
            "append→flush→publish transactions must not overlap"
        );
        assert_eq!(
            service.snapshot(),
            vec![entry("second", TodoStatus::Pending)]
        );
    }

    #[test]
    fn restore_validates_entries_from_the_projection() {
        let service = TodoService::new();
        service.restore(
            Some(session(3)),
            &[
                entry("keep", TodoStatus::Pending),
                entry("x", TodoStatus::InProgress),
                entry("y", TodoStatus::InProgress),
            ],
        );
        // 恢复侧复用写入校验：非法清单整体回退为空。
        assert!(service.snapshot().is_empty());
        service.restore(Some(session(3)), &[entry("keep", TodoStatus::Pending)]);
        assert_eq!(service.snapshot(), vec![entry("keep", TodoStatus::Pending)]);
    }

    #[test]
    fn tool_parses_arguments_and_reports_the_full_list() {
        let (service, _) = bound_service(5);
        let tool = TodoWriteTool {
            service: Arc::new(service),
        };
        let project = Project::new(".");
        let cancel = CancelToken::new();
        let result = tool
            .invoke(
                &json!({"todos": [{"content": "ship it", "status": "in_progress"}]}),
                &project,
                &cancel,
            )
            .expect("invoke");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["todos"][0]["status"], json!("in_progress"));
        // 参数错误转为 ToolError（结构化失败），不 panic。
        assert!(
            tool.invoke(&json!({"todos": "nope"}), &project, &cancel)
                .is_err()
        );
        assert!(
            tool.invoke(
                &json!({"todos": [{"content": "x", "status": "bogus"}]}),
                &project,
                &cancel,
            )
            .is_err()
        );
    }

    #[test]
    fn unbound_tool_invoke_fails() {
        let service = Arc::new(TodoService::new());
        service.restore(Some(session(5)), &[]);
        let tool = TodoWriteTool {
            service: Arc::clone(&service),
        };
        let project = Project::new(".");
        let cancel = CancelToken::new();
        let error = tool
            .invoke(
                &json!({"todos": [{"content": "x", "status": "pending"}]}),
                &project,
                &cancel,
            )
            .expect_err("unbound tool invoke must fail");
        assert!(error.to_string().contains("active run"));
    }
}
