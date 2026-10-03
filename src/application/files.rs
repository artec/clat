//! Bounded project-only browser capability; does not inherit ambient tool reads.
use super::TrustedProjectApplication;
use crate::Project;
use crate::project::review::digest;
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READ_CAP: usize = 256 * 1024;
#[derive(Clone)]
pub struct FileBrowser {
    project: Project,
    private_storage: PathBuf,
}

impl TrustedProjectApplication {
    pub fn file_browser(&self) -> FileBrowser {
        FileBrowser {
            project: self.project.clone(),
            private_storage: self.host_storage.root().to_path_buf(),
        }
    }
}

impl FileBrowser {
    pub fn search(&self, query: &str) -> Result<Value, String> {
        if query.len() > 256 {
            return Err("File query exceeds 256 bytes".into());
        }
        let started = Instant::now();
        let private_storage = self
            .private_storage
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let query = query.to_lowercase();
        let mut paths = Vec::new();
        let mut truncated = false;
        let mut walker = ignore::WalkBuilder::new(self.project.root());
        walker.follow_links(false).max_depth(Some(32)).hidden(false);
        walker.filter_entry(move |e| {
            e.path()
                .canonicalize()
                .is_ok_and(|p| !p.starts_with(&private_storage))
                && e.path()
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_none_or(|s| !excluded_component(s))
        });
        for (index, entry) in walker.build().enumerate() {
            if index >= 10_000
                || started.elapsed() > Duration::from_millis(200)
                || paths.len() >= 100
            {
                truncated = true;
                break;
            }
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry
                .file_type()
                .is_some_and(|t| t.is_file() && !t.is_symlink())
            {
                continue;
            }
            let path = entry
                .path()
                .strip_prefix(self.project.root())
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            if path.to_lowercase().contains(&query) && valid_path(&path).is_ok() {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(json!({"paths":paths,"truncated":truncated,"scope":"current_project","limit":100}))
    }

    pub fn preview(&self, path: &str, start: u64, end: u64) -> Result<Value, String> {
        valid_path(path)?;
        let storage = self
            .private_storage
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let target = self
            .project
            .root()
            .join(path)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if target.starts_with(storage) {
            return Err("Private CLAT storage is not project file context".into());
        }
        if start == 0 || end < start || end - start >= 200 || end > 100_000 {
            return Err("Preview range: 1-based, at most 200 lines, end <= 100000".into());
        }
        let (bytes, metadata) = self
            .project
            .file_snapshot(path, READ_CAP + 1)
            .map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Err(
                "Binary file preview unsupported; use the existing image attachment workflow"
                    .into(),
            );
        }
        let truncated = bytes.len() > READ_CAP;
        let slice = &bytes[..bytes.len().min(READ_CAP)];
        let text = match std::str::from_utf8(slice) {
            Ok(s) => s,
            Err(e) if truncated && e.error_len().is_none() => {
                std::str::from_utf8(&slice[..e.valid_up_to()]).map_err(|e| e.to_string())?
            }
            Err(_) => return Err("Preview supports UTF-8 text only".into()),
        };
        let lines: Vec<_> = text.split('\n').collect();
        let first = start as usize - 1;
        if first >= lines.len() {
            return Err(if truncated {
                "Range not in bounded prefix"
            } else {
                "Range beyond end of file"
            }
            .into());
        }
        let last = (end as usize).min(lines.len());
        let content = lines[first..last].join("\n");
        let modified_time = metadata.modified().map_err(|e| e.to_string())?.into_std();
        let modified = format!("{modified_time:?}");
        let modified_at_ms = modified_time
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis());
        let version =
            digest(format!("{}:{}:{}", modified, metadata.len(), digest(&bytes)).as_bytes());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        Ok(
            json!({"path":path,"start_line":start,"end_line":last,"content":content,"version":version,
            "modified":modified,"modified_at_ms":modified_at_ms,"read_at_ms":now,"truncated":truncated,"scope":"current_project",
            "reference_semantics":"fixed_editable_text_snapshot","note":"Bounded UTF-8 preview. Refresh/recheck to detect staleness; no live file binding."}),
        )
    }

    pub fn reference(
        &self,
        path: &str,
        start: u64,
        end: u64,
        expected: &str,
    ) -> Result<Value, String> {
        let mut preview = self.preview(path, start, end)?;
        if preview["version"] != expected {
            return Err("File changed since preview; refresh before quoting".into());
        }
        let content = preview["content"].as_str().unwrap();
        if content.len() > 32 * 1024 {
            return Err("Quoted snapshot exceeds 32 KiB; narrow the line range".into());
        }
        let quoted = content
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        preview["reference"] = json!(format!(
            "File snapshot `{path}` lines {}-{} (version {expected}; fixed text, not a live path):\n{quoted}\n",
            preview["start_line"], preview["end_line"]
        ));
        Ok(preview)
    }
}

fn excluded_component(name: &str) -> bool {
    let n = name.to_lowercase();
    n == ".git"
        || n == ".clat"
        || n == ".release-secrets"
        || n == "node_modules"
        || n == "target"
        || n.starts_with(".env")
        || n.starts_with(".clat-restore-")
        || n.ends_with(".pem")
        || n.ends_with(".key")
        || matches!(
            n.as_str(),
            "credentials.json" | "web-token" | "id_rsa" | "id_ed25519" | ".ssh"
        )
}
fn valid_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\\', '\0', '\n', '\r'])
        || Path::new(path).components().any(
            |c| !matches!(c, Component::Normal(s) if !excluded_component(&s.to_string_lossy())),
        )
    {
        return Err("File path must be a non-sensitive project-relative path".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
