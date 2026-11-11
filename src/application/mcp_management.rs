//! Host-level MCP settings, distinct from plugin package management.
//! All mounted projects receive one coordinated external-tool restart.
use super::{ApplicationError, HostApplication};
use crate::mcp::client::{McpServer, McpServerConfig};
use crate::mcp::manage::McpManaged;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[cfg(test)]
mod tests;

impl HostApplication {
    fn managed_mcp_project(&self, project: &str) -> Result<PathBuf, ApplicationError> {
        let path = Path::new(project);
        if !path.is_absolute() {
            return Err(ApplicationError::new("MCP project must be absolute"));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| ApplicationError::new("MCP project not found"))?;
        if !self.projects.contains_key(&canonical) {
            return Err(ApplicationError::new(
                "MCP project is not mounted or trusted",
            ));
        }
        Ok(canonical)
    }
    pub fn mcp_manage_list(&self, project: &str) -> Result<Value, ApplicationError> {
        let root = self.managed_mcp_project(project)?;
        let mut value =
            McpManaged::list(self.storage.root(), &root).map_err(ApplicationError::new)?;
        if let Some(app) = self.projects.get(&root) {
            value["runtime"] = serde_json::to_value(app.lock().expect("project").mcp_status())
                .map_err(|_| ApplicationError::new("Cannot serialize MCP status"))?;
        }
        Ok(value)
    }
    pub fn mcp_manage_get(
        &self,
        project: &str,
        scope: &str,
        name: &str,
    ) -> Result<Value, ApplicationError> {
        let root = self.managed_mcp_project(project)?;
        McpManaged::get(self.storage.root(), &root, scope, name).map_err(ApplicationError::new)
    }
    pub fn mcp_manage_save(
        &self,
        project: &str,
        scope: &str,
        name: &str,
        revision: &str,
        enabled: bool,
        config: Option<Value>,
    ) -> Result<Value, ApplicationError> {
        let root = self.managed_mcp_project(project)?;
        let storage = self.storage.root().to_path_buf();
        self.with_idle_plugin_runtimes(|| {
            McpManaged::save(&storage, &root, scope, name, revision, enabled, config)
                .map_err(ApplicationError::new)
        })
    }
    pub fn mcp_manage_remove(
        &self,
        project: &str,
        scope: &str,
        name: &str,
        revision: &str,
    ) -> Result<Value, ApplicationError> {
        let root = self.managed_mcp_project(project)?;
        let storage = self.storage.root().to_path_buf();
        self.with_idle_plugin_runtimes(|| {
            McpManaged::remove(&storage, &root, scope, name, revision)
                .map_err(ApplicationError::new)
        })
    }
    /// An explicit user-requested diagnostic, never invoked by a read/list operation.
    pub fn mcp_manage_test(
        &self,
        project: &str,
        scope: &str,
        name: &str,
        consent: bool,
    ) -> Result<Value, ApplicationError> {
        if !consent {
            return Err(ApplicationError::new(
                "MCP connection test requires explicit execution consent",
            ));
        }
        let root = self.managed_mcp_project(project)?;
        let storage = self.storage.root().to_path_buf();
        let config =
            McpManaged::configured(&storage, &root, scope, name).map_err(ApplicationError::new)?;
        let cwd = prepare_cwd(&storage, &config).map_err(ApplicationError::new)?;
        let server = McpServer::connect(name, &config, &cwd, None).map_err(|_| {
            ApplicationError::new(
                "MCP connection test failed; inspect configuration and server logs",
            )
        })?;
        let tools = server
            .list_tools()
            .map_err(|_| ApplicationError::new("MCP tools/list test failed"));
        let _ = server.shutdown();
        Ok(json!({"connected":true,"tools":tools?.len()}))
    }
}
fn prepare_cwd(storage: &Path, config: &McpServerConfig) -> Result<PathBuf, String> {
    let cwd = match config.cwd.as_deref() {
        None => storage.to_path_buf(),
        Some(value) if Path::new(value).is_absolute() => PathBuf::from(value),
        Some(value) => {
            let relative = Path::new(value);
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err("MCP cwd must not escape Host storage".into());
            }
            storage.join(relative)
        }
    };
    let resolved = cwd
        .canonicalize()
        .map_err(|_| "MCP cwd does not exist".to_owned())?;
    if !resolved.is_dir() {
        return Err("MCP cwd must be a directory".into());
    }
    if config
        .cwd
        .as_deref()
        .is_some_and(|value| !Path::new(value).is_absolute())
        && !resolved.starts_with(storage.canonicalize().map_err(|e| e.to_string())?)
    {
        return Err("MCP cwd escapes Host storage".into());
    }
    Ok(resolved)
}
