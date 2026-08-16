//! todo 工具（能力批次 1 / E）。
//!
//! `TodoWriteTool` 声明 `ToolEffect::SessionWrite`：只改本会话的本地
//! 元数据，SafeByDefault 免审——免审来自准确的 effect 分类而非工具名
//! 特例。执行期只更新内存快照并置 dirty；marker 由 application 在本轮
//! Run items 持久化之后统一落盘，保证 ToolCall → ToolResult → marker
//! 的顺序（INV-T4）。

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
    use crate::model::{ModelItem, ProviderState};
    use crate::plugins::services::TODO_MARKER_PROVIDER;

    fn entry(content: &str, status: TodoStatus) -> TodoEntry {
        TodoEntry {
            content: content.into(),
            status,
        }
    }

    /// CB1-06/INV-T3：绑定后才允许写入；未绑定/解绑后/错会话一律拒绝。
    fn bound_service(session: i64) -> TodoService {
        let service = TodoService::new();
        service.restore(Some(session), &[]);
        assert!(service.bind_run(session), "binding must match the session");
        service
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
        service.restore(Some(7), &[]);
        assert!(service.write(&[entry("x", TodoStatus::Pending)]).is_err());
        // bind 到**另一个**会话：拒绝。
        assert!(!service.bind_run(8), "binding a foreign session must fail");
        assert!(service.write(&[entry("x", TodoStatus::Pending)]).is_err());
        // 正确 bind：可写；unbind 后再写拒绝。
        assert!(service.bind_run(7));
        service
            .write(&[entry("x", TodoStatus::Pending)])
            .expect("bound write succeeds");
        service.unbind();
        assert!(service.write(&[entry("y", TodoStatus::Pending)]).is_err());
    }

    #[test]
    fn write_validates_and_updates_the_snapshot() {
        let service = bound_service(1);
        assert!(!service.is_dirty());
        let entries = service
            .write(&[
                entry("first", TodoStatus::Completed),
                entry("second", TodoStatus::InProgress),
            ])
            .expect("write");
        assert_eq!(entries.len(), 2);
        assert!(service.is_dirty());
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
        // 拒绝的写入不改变既有快照内容之外的 dirty 语义之外的状态。
        assert_eq!(service.snapshot().len(), 2);
    }

    #[test]
    fn empty_list_write_clears_context_but_still_marks_dirty() {
        let service = bound_service(2);
        service
            .write(&[entry("only", TodoStatus::Pending)])
            .expect("write");
        service.clear_dirty();
        service.write(&[]).expect("clear");
        assert!(service.is_dirty());
        assert!(service.model_context().is_none());
        let marker = service.marker();
        let ModelItem::ProviderState(state) = &marker else {
            panic!("todo marker must be a ProviderState item");
        };
        assert_eq!(
            state.data,
            json!({"version": 1, "todos": []}),
            "explicit clearing must persist an empty marker"
        );
    }

    #[test]
    fn corrupted_or_unknown_markers_fall_back_safely() {
        let valid = ModelItem::ProviderState(ProviderState {
            provider: TODO_MARKER_PROVIDER.into(),
            data: json!({"version": 1, "todos": [
                {"content": "keep", "status": "pending"},
            ]}),
        });
        let bad_version = ModelItem::ProviderState(ProviderState {
            provider: TODO_MARKER_PROVIDER.into(),
            data: json!({"version": 2, "todos": []}),
        });
        let bad_status = ModelItem::ProviderState(ProviderState {
            provider: TODO_MARKER_PROVIDER.into(),
            data: json!({"version": 1, "todos": [
                {"content": "x", "status": "wat"},
            ]}),
        });
        // 末尾损坏 → 回退到更早的合法快照。
        let items = vec![valid, bad_version.clone(), bad_status.clone()];
        let service = TodoService::new();
        service.restore(Some(7), &items);
        assert_eq!(service.snapshot(), vec![entry("keep", TodoStatus::Pending)]);
        // 全部损坏 → 空清单，无 panic。
        service.restore(Some(7), &[bad_version, bad_status]);
        assert!(service.snapshot().is_empty());
    }

    #[test]
    fn tool_parses_arguments_and_reports_the_full_list() {
        let service = Arc::new(TodoService::new());
        service.restore(Some(5), &[]);
        assert!(service.bind_run(5));
        let tool = TodoWriteTool {
            service: Arc::clone(&service),
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
        // 无活动 Run 绑定时工具必须失败（CB1-06）。
        service.unbind();
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
