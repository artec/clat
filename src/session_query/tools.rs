use super::*;
use crate::{Project, Tool, ToolCapability, ToolDefinition, ToolEffect, ToolError};
#[derive(Clone, Copy)]
pub(super) enum Operation {
    Search,
    Events,
    Read,
}
pub(super) struct HistoryTool {
    pub service: Arc<SessionQueryService>,
    pub operation: Operation,
}
impl HistoryTool {
    fn run(&self, args: &Value, cancel: &CancelToken) -> Result<Value, QueryError> {
        let object = args
            .as_object()
            .ok_or_else(|| QueryError::new("invalid_query"))?;
        let allowed = match self.operation {
            Operation::Search => &["query"][..],
            Operation::Events => &["query", "session_id"][..],
            Operation::Read => &["session_id", "event_seq", "offset", "version"][..],
        };
        if object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(QueryError::new("invalid_query"));
        }
        match self.operation {
            Operation::Search | Operation::Events => {
                let query = args["query"]
                    .as_str()
                    .ok_or_else(|| QueryError::new("invalid_query"))?;
                let session = if matches!(self.operation, Operation::Events) {
                    Some(required_session(args)?)
                } else {
                    None
                };
                let mut page = self.service.search(
                    HistorySearch {
                        query: query.into(),
                        session,
                        ..Default::default()
                    },
                    cancel,
                    true,
                )?;
                // Domain bounds preserve every reference through general result pruning.
                page.truncated |= page.hits.len() > 8;
                page.hits.truncate(8);
                for hit in &mut page.hits {
                    hit.excerpt = hit.excerpt.chars().take(160).collect();
                    hit.title = hit.title.chars().take(120).collect();
                }
                Ok(bounded_results(json!(page)))
            }
            Operation::Read => self.read(args, cancel),
        }
    }
    fn read(&self, args: &Value, cancel: &CancelToken) -> Result<Value, QueryError> {
        let reference = HistoryReference {
            project: self.service.project.header_cwd.clone(),
            session: required_session(args)?,
            seq: Some(
                args["event_seq"]
                    .as_u64()
                    .ok_or_else(|| QueryError::new("invalid_reference"))?,
            ),
        };
        let offset = args
            .get("offset")
            .map(|v| {
                v.as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| QueryError::new("invalid_reference"))
            })
            .transpose()?
            .unwrap_or(0);
        let version = args
            .get("version")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| QueryError::new("invalid_reference"))
            })
            .transpose()?;
        self.service
            .read(
                HistoryReadRequest {
                    reference,
                    trusted_projects: false,
                    offset,
                    version,
                },
                cancel,
                true,
            )
            .map(|r| json!(r))
    }
}
impl Tool for HistoryTool {
    fn definition(&self) -> ToolDefinition {
        let (name, description, properties, required) = match self.operation {
            Operation::Search => (
                "session_search",
                "Search committed sessions of this project. Literal query only, excludes this session. Bounded results have stable references; narrow query when truncated.",
                json!({"query":{"type":"string","minLength":1,"maxLength":512}}),
                vec!["query"],
            ),
            Operation::Events => (
                "session_event_search",
                "Search committed visible messages inside one session in this project. Current session is bounded before this step. Excludes reasoning and raw diagnostics.",
                json!({"query":{"type":"string","minLength":1,"maxLength":512},"session_id":{"type":"string"}}),
                vec!["query", "session_id"],
            ),
            Operation::Read => (
                "session_event_read",
                "Read a bounded redacted message observation by stable session/event reference in this project. Continue with next_offset and returned version. No journal repair, write or session selection.",
                json!({"session_id":{"type":"string"},"event_seq":{"type":"integer","minimum":0},"offset":{"type":"integer","minimum":0},"version":{"type":"string"}}),
                vec!["session_id", "event_seq"],
            ),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
            effect: ToolEffect::Read,
            strict: true,
        }
    }
    fn error_output(&self, error: &ToolError) -> Value {
        let mut result = json!({"error":error.to_string()});
        if let Some(fields) = error.recovery().and_then(Value::as_object) {
            result.as_object_mut().unwrap().extend(fields.clone());
        }
        result
    }
    fn capability_metadata(&self) -> ToolCapability {
        ToolCapability::native(
            &["session", "history", "search", "会话"],
            &["current-project"],
            &["session/event-reference"],
            "Read-only committed history; current project only, bounded output.",
        )
    }
    fn invoke(&self, args: &Value, _: &Project, cancel: &CancelToken) -> Result<Value, ToolError> {
        self.run(args, cancel).map_err(|e| {
            ToolError::new(e.presentation().english()).with_recovery(
                json!({"code":format!("history.{}",e.code),"presentation":e.presentation()}),
            )
        })
    }
}
fn required_session(args: &Value) -> Result<String, QueryError> {
    args["session_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 1024)
        .map(str::to_owned)
        .ok_or_else(|| QueryError::new("invalid_reference"))
}

pub(super) fn bounded_results(mut value: Value) -> Value {
    let limit = crate::plugins::THRESHOLD_CHARS.saturating_sub(128);
    while serde_json::to_string(&value).is_ok_and(|s| s.chars().count() > limit) {
        let Some(hits) = value.get_mut("hits").and_then(Value::as_array_mut) else {
            break;
        };
        if hits.pop().is_none() {
            break;
        }
        value["truncated"] = json!(true);
    }
    value
}
