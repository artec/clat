use crate::model::CancelToken;
use crate::project::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    Pure,
    Read,
    Write,
    Execute,
    Network,
    /// Read-only work performed by an untrusted external process. Unlike
    /// native [`Read`](Self::Read), this still crosses a permission boundary.
    ExternalRead,
    /// An external operation advertised as destructive (delete, overwrite,
    /// revoke, and similar irreversible effects).
    Destructive,
}

impl fmt::Display for ToolEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pure => f.write_str("pure"),
            Self::Read => f.write_str("read-only"),
            Self::Write => f.write_str("writes files"),
            Self::Execute => f.write_str("runs commands"),
            Self::Network => f.write_str("network access"),
            Self::ExternalRead => f.write_str("external read access"),
            Self::Destructive => f.write_str("destructive external action"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub effect: ToolEffect,
    pub strict: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub output: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn invoke(
        &self,
        arguments: &Value,
        project: &Project,
        cancel: &CancelToken,
    ) -> Result<Value, ToolError>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        self.tools.push(Box::new(tool));
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .map(Box::as_ref)
    }
}
