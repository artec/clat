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
            && path.as_ref().is_none_or(|path| {
                path.to_str()
                    .is_some_and(|path| super::reactions::find(path).is_some())
            });
        let before = observe.then(|| snapshot(invocation.project, path.as_deref()));
        let mut result = next.execute(invocation);
        if !observe || invocation.cancel.is_cancelled() {
            return result;
        }
        // Failed commands can still have edited a manifest. Permission denials
        // never enter this already-authorized middleware.
        let reports = changed_reports(&self.0, before, invocation, path.as_deref());
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

type Snapshot = Result<BTreeMap<String, Vec<u8>>, String>;
fn changed_reports(
    service: &RustSupport,
    before: Option<Snapshot>,
    invocation: &ToolInvocation<'_>,
    path: Option<&Path>,
) -> Vec<Value> {
    let result = before
        .expect("observed invocation")
        .and_then(|before| snapshot(invocation.project, path).map(|after| (before, after)));
    match result {
        Ok((before, after)) => before
            .keys()
            .chain(after.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|path| before.get(*path) != after.get(*path))
            .filter_map(|path| {
                service.react(
                    path,
                    false,
                    invocation.cancel,
                    before.get(path).map(Vec::as_slice),
                )
            })
            .collect(),
        Err(error) => vec![json!({"status":"failed","error":error,
            "presentation":crate::i18n::UiMessage::new("rust_support.operation_failed")})],
    }
}

fn snapshot(project: &Project, path: Option<&Path>) -> Snapshot {
    let Some(path) = path else {
        return manifests(project);
    };
    let path = path.to_str().ok_or("Rust artifact path is not UTF-8")?;
    match project.file_snapshot(path, 1024 * 1024 + 1) {
        Ok((bytes, _)) if bytes.len() <= 1024 * 1024 => Ok([(path.to_owned(), bytes)].into()),
        Ok(_) => Err("Rust artifact exceeds byte limit".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn manifests(project: &Project) -> Result<BTreeMap<String, Vec<u8>>, String> {
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
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(project.root())
            .map_err(|e| e.to_string())?;
        let relative = relative.to_str().ok_or("Rust manifest path is not UTF-8")?;
        if super::reactions::find(relative).is_none() {
            continue;
        }
        let (bytes, _) = project
            .file_snapshot(relative, 1024 * 1024 + 1)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("Rust manifest exceeds byte limit".into());
        }
        found.insert(relative.to_owned(), bytes);
    }
    Ok(found)
}
