//! Observe actual manifest bytes around authorized native writes/commands.
use super::*;
use crate::tool::{ToolInvocation, ToolMiddleware, ToolNext};
use std::collections::BTreeMap;

pub(super) struct Maintenance(pub Arc<RustSupport>);

impl ToolMiddleware for Maintenance {
    fn execute(
        &self,
        invocation: &ToolInvocation<'_>,
        next: &dyn ToolNext,
    ) -> Result<Value, crate::ToolError> {
        let effect = invocation.tool.definition().effect;
        let observe = matches!(
            effect,
            crate::ToolEffect::Write | crate::ToolEffect::Execute
        );
        let path = native_manifest_path(invocation);
        let same_project = invocation.project.root() == self.0.project.root();
        let observe = observe
            && same_project
            && path
                .as_ref()
                .is_none_or(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"));
        let before = observe.then(|| manifests(invocation.project));
        let mut result = next.execute(invocation);
        if !observe || invocation.cancel.is_cancelled() {
            return result;
        }
        // Failed commands can still have edited a manifest. Permission denials
        // never enter this already-authorized middleware.
        let reports = changed_reports(&self.0, before, invocation);
        if !reports.is_empty()
            && let Ok(Value::Object(output)) = &mut result
        {
            output.insert("rust_support".into(), json!(reports));
        }
        result
    }
}

fn native_manifest_path(invocation: &ToolInvocation<'_>) -> Option<PathBuf> {
    match invocation.tool.definition().name.as_str() {
        "write_file" | "edit_file" => invocation.arguments["path"].as_str().map(PathBuf::from),
        _ => None,
    }
}

type Snapshot = Result<BTreeMap<String, String>, String>;
fn changed_reports(
    service: &RustSupport,
    before: Option<Snapshot>,
    invocation: &ToolInvocation<'_>,
) -> Vec<Value> {
    let result = before
        .expect("observed invocation")
        .and_then(|before| manifests(invocation.project).map(|after| (before, after)));
    match result {
        Ok((before, after)) => after
            .into_iter()
            .filter(|(path, digest)| before.get(path) != Some(digest))
            .filter_map(|(path, _)| service.saved(&path, false, invocation.cancel))
            .collect(),
        Err(error) => vec![json!({"status":"failed","error":error,
            "presentation":crate::i18n::UiMessage::new("rust_support.failed")})],
    }
}

fn manifests(project: &Project) -> Result<BTreeMap<String, String>, String> {
    use sha2::{Digest, Sha256};
    let mut walk = ignore::WalkBuilder::new(project.root());
    walk.hidden(false).follow_links(false).max_depth(Some(32));
    walk.filter_entry(|entry| {
        !matches!(
            entry.file_name().to_str(),
            Some("target" | "node_modules" | ".git")
        )
    });
    let mut found = BTreeMap::new();
    for (index, entry) in walk.build().enumerate() {
        if index >= 20000 || found.len() >= 256 {
            return Err("Rust manifest scan limit reached".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() != "Cargo.toml"
            || !entry.file_type().is_some_and(|kind| kind.is_file())
        {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(project.root())
            .map_err(|e| e.to_string())?;
        let relative = relative.to_str().ok_or("Rust manifest path is not UTF-8")?;
        let (bytes, _) = project
            .file_snapshot(relative, 1024 * 1024 + 1)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Rust manifest exceeds byte limit".into());
        }
        found.insert(relative.to_owned(), format!("{:x}", Sha256::digest(bytes)));
    }
    Ok(found)
}
