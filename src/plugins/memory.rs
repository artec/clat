use super::services::{
    COMMAND_SERVICE, COMMAND_SERVICE_ID, MEMORY_SERVICE, MEMORY_SERVICE_ID, TOOL_SERVICE,
    TOOL_SERVICE_ID,
};
use crate::application::TrustedProjectApplication;
use crate::command::{CommandError, CommandHandler, CommandOutcome, CommandSpec};
use crate::memory::{MemoryScope, MemoryService};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::{CancelToken, Project, Tool, ToolDefinition, ToolEffect, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.memory");
const REQUIRES: &[ServiceId] = &[TOOL_SERVICE_ID, COMMAND_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[MEMORY_SERVICE_ID],
    requires: REQUIRES,
    optional: &[],
};

pub(crate) struct MemoryPlugin {
    storage_root: PathBuf,
    project_root: PathBuf,
}

impl MemoryPlugin {
    pub(crate) fn new(storage_root: PathBuf, project_root: PathBuf) -> Self {
        Self {
            storage_root,
            project_root,
        }
    }
}

impl Plugin for MemoryPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        let service = Arc::new(
            MemoryService::open(&self.storage_root, &self.project_root)
                .map_err(PluginError::new)?,
        );
        let tools = context
            .require(TOOL_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let commands = context
            .require(COMMAND_SERVICE)
            .map_err(|error| PluginError::new(error.to_string()))?;
        let tool_lease = tools
            .register(
                context.owner(),
                Arc::new(MemorySearchTool {
                    service: Arc::clone(&service),
                }),
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            tool_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        let command_lease = commands
            .register(
                context.owner(),
                CommandSpec {
                    // A2（2026-09-02 负责人裁定）：主名 `mem`，旧名保留别名
                    //（INV-SC-2：lookup 按全部名字匹配，旧输入仍可派发）。
                    names: vec!["mem".into(), "memory".into()],
                    description: "list, add, edit, or delete explicit local memories".into(),
                    takes_args: true,
                    group: crate::command::CommandGroup::Experiments,
                    order: 11,
                    handler: Arc::new(MemoryCommand),
                },
            )
            .map_err(|error| PluginError::new(error.to_string()))?;
        context.defer(move || {
            command_lease
                .revoke()
                .map_err(|error| DisposeError::new(error.to_string()))
        });
        context
            .provide(MEMORY_SERVICE, service)
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct MemorySearchTool {
    service: Arc<MemoryService>,
}

impl Tool for MemorySearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory_search".into(),
            description: "Search user-approved local memories. Results include scope, provenance, revision, digest, staleness, and the reason each record matched. This tool is read-only; only the user can mutate memory with /memory.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": 4096},
                    "top_k": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            effect: ToolEffect::Read,
            strict: true,
        }
    }

    fn invoke(
        &self,
        arguments: &Value,
        _project: &Project,
        _cancel: &CancelToken,
    ) -> Result<Value, ToolError> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("memory_search requires string `query`"))?;
        let top_k = arguments
            .get("top_k")
            .and_then(Value::as_u64)
            .unwrap_or(crate::memory::DEFAULT_TOP_K as u64)
            .min(crate::memory::MAX_TOP_K as u64) as usize;
        let hits = self
            .service
            .search(query, top_k, crate::memory::MAX_RESULT_BYTES)
            .map_err(ToolError::new)?;
        let mut results = hits
            .into_iter()
            .map(|hit| {
                json!({
                    "id": hit.record.id,
                    "scope": hit.record.scope.as_str(),
                    "content": hit.record.content,
                    "source": hit.record.source,
                    "revision": hit.record.revision,
                    "digest": hit.record.digest,
                    "stale": hit.stale,
                    "score": hit.score,
                    "reason": hit.reason,
                })
            })
            .collect::<Vec<_>>();
        let mut value = json!({
            "query": query,
            "results": results.clone()
        });
        while serde_json::to_vec(&value)
            .map_err(|error| ToolError::new(error.to_string()))?
            .len()
            > crate::memory::MAX_RESULT_BYTES
        {
            if results.pop().is_none() {
                return Err(ToolError::new("memory query exceeds result byte budget"));
            }
            value["results"] = json!(results);
        }
        Ok(value)
    }
}

struct MemoryCommand;

impl CommandHandler for MemoryCommand {
    fn run(
        &self,
        application: &mut TrustedProjectApplication,
        args: &str,
    ) -> Result<CommandOutcome, CommandError> {
        let (verb, rest) = split_once(args);
        match verb {
            "" | "list" => {
                let scope = match rest {
                    "" | "all" => None,
                    value => Some(parse_scope(value)?),
                };
                let hits = application.memory_list(scope).map_err(failed)?;
                let message = if hits.is_empty() {
                    "No explicit memories.".into()
                } else {
                    hits.into_iter()
                        .map(|hit| {
                            format!(
                                "{} rev={} scope={}{} source={} — {}",
                                hit.record.id,
                                hit.record.revision,
                                hit.record.scope.as_str(),
                                if hit.stale { " stale" } else { "" },
                                hit.record.source,
                                hit.record.content
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CommandOutcome::Status(message))
            }
            "show" => {
                let id = rest.trim();
                if id.is_empty() {
                    return Err(usage());
                }
                let hit = application.memory_get(id).map_err(failed)?;
                let Some(hit) = hit else {
                    return Err(failed(format!("memory `{id}` not found")));
                };
                Ok(CommandOutcome::Status(format!(
                    "{} rev={} scope={} stale={} digest={} source={}\n{}",
                    hit.record.id,
                    hit.record.revision,
                    hit.record.scope.as_str(),
                    hit.stale,
                    hit.record.digest,
                    hit.record.source,
                    hit.record.content
                )))
            }
            "add" => {
                let (scope, tail) = split_once(rest);
                let scope = parse_scope(scope)?;
                let (content, source) = split_source(tail)?;
                let record = application
                    .memory_add(scope, content, source)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status(format!(
                    "Memory {} added at revision 1.",
                    record.id
                )))
            }
            "edit" => {
                let (id, tail) = split_once(rest);
                let (revision, content) = split_once(tail);
                let revision = revision.parse::<u64>().map_err(|_| usage())?;
                if id.is_empty() || content.is_empty() {
                    return Err(usage());
                }
                let record = application
                    .memory_update(id, revision, content)
                    .map_err(failed)?;
                Ok(CommandOutcome::Status(format!(
                    "Memory {} updated to revision {}.",
                    record.id, record.revision
                )))
            }
            "delete" => {
                let (id, revision) = split_once(rest);
                let revision = revision.parse::<u64>().map_err(|_| usage())?;
                if id.is_empty() {
                    return Err(usage());
                }
                application.memory_delete(id, revision).map_err(failed)?;
                Ok(CommandOutcome::Status(format!("Memory {id} deleted.")))
            }
            _ => Err(usage()),
        }
    }
}

fn split_once(value: &str) -> (&str, &str) {
    let value = value.trim();
    value
        .split_once(char::is_whitespace)
        .map(|(head, tail)| (head, tail.trim()))
        .unwrap_or((value, ""))
}

fn parse_scope(value: &str) -> Result<MemoryScope, CommandError> {
    MemoryScope::parse(value).ok_or_else(usage)
}

fn split_source(value: &str) -> Result<(&str, Option<&str>), CommandError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(usage());
    }
    match value.rsplit_once(" --source ") {
        Some((content, source)) if !content.trim().is_empty() && !source.trim().is_empty() => {
            Ok((content.trim(), Some(source.trim())))
        }
        _ => Ok((value, None)),
    }
}

fn usage() -> CommandError {
    CommandError::Failed {
        message: "usage: /memory list [all|project|user] | show <id> | add <project|user> <content> [--source file:path] | edit <id> <revision> <content> | delete <id> <revision>".into(),
    }
}

fn failed(error: impl ToString) -> CommandError {
    CommandError::Failed {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{CommandsPlugin, ToolRegistryPlugin};

    #[test]
    fn catalog_omission_and_scope_close_remove_memory_surfaces() {
        let (storage, project) = crate::test_support::roots("memory-plugin-lifecycle");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let foundations = || {
            vec![
                Arc::new(ToolRegistryPlugin) as Arc<dyn Plugin>,
                Arc::new(CommandsPlugin),
            ]
        };
        let mut without = PluginManager::root(ScopeKind::TrustedProject);
        without.mount_all(foundations()).unwrap();
        let tools = without.require(TOOL_SERVICE).unwrap();
        let commands = without.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("memory_search").is_none());
        assert!(commands.lookup("memory").is_none());
        without.close().unwrap();

        let mut with = PluginManager::root(ScopeKind::TrustedProject);
        let mut catalog = foundations();
        catalog.push(Arc::new(MemoryPlugin::new(storage, project)));
        with.mount_all(catalog).unwrap();
        let tools = with.require(TOOL_SERVICE).unwrap();
        let commands = with.require(COMMAND_SERVICE).unwrap();
        assert!(tools.get("memory_search").is_some());
        assert!(commands.lookup("memory").is_some());
        with.close().unwrap();
        assert!(tools.get("memory_search").is_none());
        assert!(commands.lookup("memory").is_none());
    }
}
