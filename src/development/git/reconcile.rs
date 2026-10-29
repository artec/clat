//! Receipt recovery is based on parent, approved tree and message, never success guesses.
use super::*;
impl GitService {
    pub(super) fn approve_tree(
        &self,
        op: &Operation,
        index: &super::index::FrozenIndex,
    ) -> Result<(), String> {
        let mut cmd = command::base(&self.project, false)?;
        cmd.env("GIT_INDEX_FILE", index.path()).arg("write-tree");
        let result =
            crate::process::capture_command(cmd, 4096, std::time::Duration::from_secs(10))?;
        if !result.status.success() {
            return Err("git_index".into());
        }
        let tree = String::from_utf8(result.stdout).map_err(|_| "git_encoding")?;
        op.saved.lock().map_err(|_| "git_busy")?.request["approvedTree"] = json!(tree.trim());
        self.persist()
    }
    pub(super) fn commit_matches(&self, saved: &SavedOperation) -> bool {
        let Ok(raw) = command::query(&self.project, &["log", "-1", "--format=%P%n%T%n%B"]) else {
            return false;
        };
        let raw = String::from_utf8_lossy(&raw);
        let Some((parent, rest)) = raw.split_once('\n') else {
            return false;
        };
        let Some((tree, message)) = rest.split_once('\n') else {
            return false;
        };
        parent.trim() == saved.request["expected"]["head"].as_str().unwrap_or("")
            && Some(tree.trim()) == saved.request["approvedTree"].as_str()
            && Some(message.trim()) == saved.view.message.as_deref().map(str::trim)
    }
}
