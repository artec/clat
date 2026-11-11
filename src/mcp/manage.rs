//! Host-owned user/project MCP server settings. Never present as plugin packages.
use super::client::{McpConfig, McpServerConfig};
use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

const FILE: &str = "mcp-managed.json";
const MAX_BYTES: usize = 1024 * 1024;
#[cfg(test)]
mod tests;
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct McpEntry {
    pub enabled: bool,
    pub config: McpServerConfig,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpManaged {
    #[serde(default)]
    user: BTreeMap<String, McpEntry>,
    #[serde(default)]
    projects: BTreeMap<String, BTreeMap<String, McpEntry>>,
}
impl McpManaged {
    fn directory(root: &Path) -> Result<Dir, String> {
        Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|e| format!("open Host storage: {e}"))
    }
    pub(crate) fn load(root: &Path) -> Result<(Self, String), String> {
        let dir = Self::directory(root)?;
        let content = crate::private_fs::read_private_text(&dir, FILE, MAX_BYTES)?;
        match content {
            None => Ok((Self::default(), "missing".into())),
            Some(value) => {
                let digest = sha(&value);
                let parsed =
                    serde_json::from_str(&value).map_err(|_| "Invalid MCP managed registry")?;
                Ok((parsed, digest))
            }
        }
    }
    fn write(&self, root: &Path) -> Result<String, String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        if text.len() > MAX_BYTES {
            return Err("MCP registry is too large".into());
        }
        let dir = Self::directory(root)?;
        crate::private_fs::write_text_atomic_in_dir(&dir, FILE, &text)?;
        Ok(sha(&text))
    }
    fn scope_mut<'a>(
        &'a mut self,
        scope: &str,
        project: &Path,
    ) -> Result<&'a mut BTreeMap<String, McpEntry>, String> {
        match scope {
            "user" => Ok(&mut self.user),
            "project" => Ok(self.projects.entry(project_key(project)?).or_default()),
            _ => Err("MCP scope must be user or project".into()),
        }
    }
    pub(crate) fn list(root: &Path, project: &Path) -> Result<Value, String> {
        let (registry, revision) = Self::load(root)?;
        let legacy = super::client::load_mcp_config(root)?;
        let summary = |entries: &BTreeMap<String, McpEntry>| {
            entries.iter().map(|(name,e)|json!({
            "name":name,"enabled":e.enabled,"transport":if e.config.is_http(){"http"}else{"stdio"},
            "has_credentials":!e.config.env.is_empty()||!e.config.headers.is_empty(),
        })).collect::<Vec<_>>()
        };
        let project_entries = registry.projects.get(&project_key(project)?);
        let legacy_rows = legacy
            .iter()
            .map(|(name, config)| {
                json!({"name":name,
            "transport":if config.is_http(){"http"}else{"stdio"},"source":"legacy"})
            })
            .collect::<Vec<_>>();
        Ok(json!({"revision":revision,"user":summary(&registry.user),
            "project":project_entries.map(summary).unwrap_or_default(),
            "legacy":legacy_rows,"project_root":project}))
    }
    pub(crate) fn get(
        root: &Path,
        project: &Path,
        scope: &str,
        name: &str,
    ) -> Result<Value, String> {
        validate_name(name)?;
        let (registry, revision) = Self::load(root)?;
        let entry = match scope {
            "user" => registry.user.get(name),
            "project" => registry
                .projects
                .get(&project_key(project)?)
                .and_then(|e| e.get(name)),
            _ => return Err("MCP scope must be user or project".into()),
        };
        let info = entry.map(|e| {
            // A remote URL can embed a credential in its query string. A
            // management GET is not a credential read. Keep such URLs write-only.
            let url_with_query =
                e.config.url.as_deref().is_some_and(|value| {
                    url::Url::parse(value).is_ok_and(|url| url.query().is_some())
                });
            let visible_url = if url_with_query {
                None
            } else {
                e.config.url.as_deref()
            };
            json!({"enabled":e.enabled,"command":e.config.command,"url":visible_url,
                "url_has_query":url_with_query,"cwd":e.config.cwd,"arg_count":e.config.args.len(),
                "env_keys":e.config.env.keys().collect::<Vec<_>>(),
                "header_keys":e.config.headers.keys().collect::<Vec<_>>()})
        });
        Ok(json!({"revision":revision,"entry":info,"name":name,"scope":scope}))
    }
    pub(crate) fn save(
        root: &Path,
        project: &Path,
        scope: &str,
        name: &str,
        expected: &str,
        enabled: bool,
        config: Option<Value>,
    ) -> Result<Value, String> {
        validate_name(name)?;
        let (mut registry, revision) = Self::load(root)?;
        if revision != expected {
            return Err("MCP settings changed; refresh before saving".into());
        }
        let entries = registry.scope_mut(scope, project)?;
        let previous = entries.get(name).cloned();
        let parsed = match config {
            None => previous
                .as_ref()
                .map(|e| e.config.clone())
                .ok_or("MCP configuration is required")?,
            Some(Value::Object(edit)) => {
                let mut value = previous
                    .as_ref()
                    .map(|e| serde_json::to_value(&e.config).expect("MCP config serialize"))
                    .unwrap_or(json!({}));
                let obj = value.as_object_mut().expect("MCP config object");
                for (key, entry) in edit {
                    if !matches!(
                        key.as_str(),
                        "command" | "args" | "env" | "cwd" | "url" | "headers"
                    ) {
                        return Err("Unknown MCP configuration field".into());
                    }
                    obj.insert(key, entry);
                }
                serde_json::from_value(value).map_err(|_| "Invalid MCP configuration")?
            }
            Some(_) => return Err("MCP config must be a JSON object".into()),
        };
        validate_config(&parsed)?;
        entries.insert(
            name.to_owned(),
            McpEntry {
                enabled,
                config: parsed,
            },
        );
        let revision = registry.write(root)?;
        Ok(json!({"saved":true,"revision":revision,"name":name}))
    }
    pub(crate) fn remove(
        root: &Path,
        project: &Path,
        scope: &str,
        name: &str,
        expected: &str,
    ) -> Result<Value, String> {
        validate_name(name)?;
        let (mut registry, revision) = Self::load(root)?;
        if revision != expected {
            return Err("MCP settings changed; refresh before removing".into());
        }
        let entries = registry.scope_mut(scope, project)?;
        if entries.remove(name).is_none() {
            return Err("MCP server is not managed in this scope".into());
        }
        let revision = registry.write(root)?;
        Ok(json!({"removed":true,"revision":revision,"name":name}))
    }
    pub(crate) fn merge(
        root: &Path,
        project: &Path,
        effective: &mut McpConfig,
        manual: &McpConfig,
    ) -> Result<Vec<String>, String> {
        let (registry, _) = Self::load(root)?;
        let mut overridden = Vec::new();
        for (name, entry) in &registry.user {
            if manual.contains_key(name) {
                continue;
            }
            overridden.push(name.clone());
            if entry.enabled {
                effective.insert(name.clone(), entry.config.clone());
            } else {
                effective.remove(name);
            }
        }
        effective.extend(manual.clone());
        if let Some(project_entries) = registry.projects.get(&project_key(project)?) {
            for (name, entry) in project_entries {
                // Explicit legacy mcp.json entries remain the escape hatch,
                // even when a project has a same-named managed override.
                if manual.contains_key(name) {
                    continue;
                }
                overridden.push(name.clone());
                if entry.enabled {
                    effective.insert(name.clone(), entry.config.clone());
                } else {
                    effective.remove(name);
                }
            }
        }
        Ok(overridden)
    }
    pub(crate) fn configured(
        root: &Path,
        project: &Path,
        scope: &str,
        name: &str,
    ) -> Result<McpServerConfig, String> {
        let (registry, _) = Self::load(root)?;
        let selected = match scope {
            "user" => registry.user.get(name),
            "project" => registry
                .projects
                .get(&project_key(project)?)
                .and_then(|items| items.get(name)),
            _ => return Err("MCP scope must be user or project".into()),
        };
        selected
            .filter(|entry| entry.enabled)
            .map(|entry| entry.config.clone())
            .ok_or("MCP server is not enabled".into())
    }
}
fn project_key(project: &Path) -> Result<String, String> {
    let resolved = project
        .canonicalize()
        .map_err(|_| "MCP project root is unavailable".to_owned())?;
    Ok(resolved.to_string_lossy().into_owned())
}
fn sha(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid MCP server name".into());
    }
    Ok(())
}
fn validate_config(c: &McpServerConfig) -> Result<(), String> {
    if c.is_http() {
        if !c.command.trim().is_empty()
            || !c.args.is_empty()
            || !c.env.is_empty()
            || c.cwd.is_some()
        {
            return Err("HTTP MCP cannot specify a process command or cwd".into());
        }
        let parsed =
            url::Url::parse(c.url.as_deref().unwrap_or("")).map_err(|_| "Invalid MCP URL")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err("Invalid MCP HTTP endpoint".into());
        }
        if c.headers.iter().any(|(k, v)| {
            k.is_empty()
                || k.contains('\r')
                || k.contains('\n')
                || v.contains('\r')
                || v.contains('\n')
        }) {
            return Err("Invalid MCP header".into());
        }
    } else {
        if c.command.trim().is_empty() || !c.headers.is_empty() {
            return Err("MCP command is missing or mixed with HTTP headers".into());
        }
        if c.command.contains('\0') || c.args.iter().any(|s| s.contains('\0')) {
            return Err("Invalid MCP command".into());
        }
    }
    Ok(())
}
