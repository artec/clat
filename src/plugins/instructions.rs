//! 项目指令注入（能力批次 1 / B）。
//!
//! Trusted Project Scope 挂载时按 AGENTS.md → CLAUDE.md 顺序读取项目根的
//! 第一个存在者，作为 PromptRegistry 贡献注入系统提示。信任确认之前该
//! 插件不存在于任何 Catalog（信任门由 Scope 结构保证）；读取经
//! `Project::resolve_existing` 做根内校验，指向项目外的符号链接在挂载时
//! 显式失败，绝不静默当作"无指令"。

use super::services::{PROMPT_SERVICE, PROMPT_SERVICE_ID};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::project::Project;
use std::io;
use std::sync::Arc;

const ID: PluginId = PluginId::new("builtin.project_instructions");
const REQUIRES: &[ServiceId] = &[PROMPT_SERVICE_ID];
const DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES,
    optional: &[],
};

/// 候选文件按序探测，第一个存在者胜出。
const CANDIDATES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// 单文件读取上限：64 KiB 内容外加 1 字节探测超限。
const MAX_BYTES: usize = 64 * 1024;

/// 读取器接缝：测试注入 spy 统计探测次数；生产实现读真实文件系统。
type InstructionReader = Arc<dyn Fn(&Project, &str) -> io::Result<Option<Vec<u8>>> + Send + Sync>;

pub(crate) struct ProjectInstructionsPlugin {
    project: Project,
    reader: InstructionReader,
}

impl ProjectInstructionsPlugin {
    pub(crate) fn new(project: Project) -> Self {
        Self {
            project,
            reader: Arc::new(read_candidate_bytes),
        }
    }

    #[cfg(test)]
    fn with_reader(project: Project, reader: InstructionReader) -> Self {
        Self { project, reader }
    }
}

impl Plugin for ProjectInstructionsPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        for name in CANDIDATES {
            let bytes = match (self.reader)(&self.project, name) {
                Ok(None) => continue,
                Ok(Some(bytes)) => bytes,
                Err(error) => {
                    return Err(PluginError::new(format!("{name}: {error}")));
                }
            };
            let (mut text, truncated) = decode_limited(&bytes, name)?;
            // 空白文件视为无指令：不贡献任何文本（含标题行），保持
            // "无指令零影响"。
            if text.trim().is_empty() {
                return Ok(());
            }
            if truncated {
                text.push_str("\n\n(truncated at 64 KiB)");
            }
            let registry = context
                .require(PROMPT_SERVICE)
                .map_err(|error| PluginError::new(error.to_string()))?;
            let contribution = format!("# Project instructions ({name})\n\n{text}");
            let lease = registry
                .contribute(context.owner(), contribution)
                .map_err(|error| PluginError::new(error.to_string()))?;
            context.defer(move || {
                lease
                    .revoke()
                    .map_err(|error| DisposeError::new(error.to_string()))
            });
            // 第一个存在者胜出，不再探测后续候选。
            return Ok(());
        }
        Ok(())
    }
}

/// 读取单个候选文件：单次 capability-bound 解析/打开，最多
/// MAX_BYTES + 1 字节。不存在返回 None；symlink（含 broken）显式失败。
fn read_candidate_bytes(project: &Project, name: &str) -> io::Result<Option<Vec<u8>>> {
    project.read_file_limited(name, MAX_BYTES + 1)
}

/// 解码为 UTF-8 并在超限时退到合法字符边界截断。读取范围内出现非法
/// UTF-8 是显式错误，绝不静默丢弃。
fn decode_limited(bytes: &[u8], name: &str) -> Result<(String, bool), PluginError> {
    let truncated = bytes.len() > MAX_BYTES;
    let limit = bytes.len().min(MAX_BYTES);
    match std::str::from_utf8(&bytes[..limit]) {
        Ok(text) => Ok((text.to_owned(), truncated)),
        Err(error) => {
            // error_len() == None 表示切片在多字节字符中间被截断——退到
            // valid_up_to 即可；Some(_) 表示文件本身含非法字节，拒绝。
            if truncated && error.error_len().is_none() {
                let text = std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .map_err(|_| PluginError::new(format!("{name} is not valid UTF-8")))?;
                Ok((text.to_owned(), true))
            } else {
                Err(PluginError::new(format!(
                    "{name} is not valid UTF-8 within the first 64 KiB"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginManager;
    use crate::plugins::{DefaultPromptPlugin, PromptRegistryPlugin, bootstrap_catalog};
    use crate::storage::Storage;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clat-instructions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn mount_instructions(project: Project) -> String {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(PromptRegistryPlugin),
                Arc::new(DefaultPromptPlugin),
                Arc::new(ProjectInstructionsPlugin::new(project)),
            ])
            .expect("mount");
        let registry = manager.require(PROMPT_SERVICE).expect("registry");
        let text = registry.instructions();
        manager.close().expect("close");
        text
    }

    fn default_only() -> String {
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(PromptRegistryPlugin),
                Arc::new(DefaultPromptPlugin),
            ])
            .expect("mount");
        let registry = manager.require(PROMPT_SERVICE).expect("registry");
        let text = registry.instructions();
        manager.close().expect("close");
        text
    }

    #[test]
    fn no_instruction_files_leave_prompts_untouched() {
        // INV-I3：无 AGENTS.md/CLAUDE.md 时逐字节一致。
        let root = temp_root("none");
        let with_plugin = mount_instructions(Project::new(&root));
        assert_eq!(with_plugin, default_only());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn claude_md_only_is_contributed_with_source_name() {
        let root = temp_root("claude");
        std::fs::write(root.join("CLAUDE.md"), "Be terse.").expect("write");
        let text = mount_instructions(Project::new(&root));
        assert!(text.contains("# Project instructions (CLAUDE.md)"));
        assert!(text.contains("Be terse."));
        assert!(text.ends_with("Be terse."));
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn agents_md_takes_priority_and_stops_probing() {
        let root = temp_root("both");
        std::fs::write(root.join("CLAUDE.md"), "claude rules").expect("write");
        // spy 读取器：AGENTS.md 命中后不得再探测任何候选。
        let mut manager = PluginManager::root(ScopeKind::TrustedProject);
        manager
            .mount_all(vec![
                Arc::new(PromptRegistryPlugin),
                Arc::new(DefaultPromptPlugin),
                Arc::new(ProjectInstructionsPlugin::with_reader(
                    Project::new(&root),
                    Arc::new(move |_project, name| {
                        assert_eq!(name, "AGENTS.md", "must stop probing after first hit");
                        Ok(Some(b"agents rules".to_vec()))
                    }),
                )),
            ])
            .expect("mount");
        let registry = manager.require(PROMPT_SERVICE).expect("registry");
        let text = registry.instructions();
        assert!(text.contains("# Project instructions (AGENTS.md)"));
        assert!(text.contains("agents rules"));
        assert!(!text.contains("claude rules"));
        manager.close().expect("close");
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn oversized_cjk_file_truncates_on_char_boundary() {
        let root = temp_root("cjk");
        // '中' 为 3 字节：21845 个字符占 65535 字节，64 KiB 边界落在第
        // 21846 个字符中间，截断必须退到 65535。
        let content = "中".repeat(30_000);
        std::fs::write(root.join("AGENTS.md"), content).expect("write");
        let text = mount_instructions(Project::new(&root));
        let (_, body) = text
            .split_once("# Project instructions (AGENTS.md)\n\n")
            .expect("header");
        assert!(body.ends_with("(truncated at 64 KiB)"));
        let kept = body.trim_end_matches("\n\n(truncated at 64 KiB)");
        assert_eq!(kept.chars().count(), 21_845);
        assert_eq!(kept.len(), 65_535);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn symlink_escaping_the_project_root_is_rejected() {
        #[cfg(unix)]
        {
            let root = temp_root("symlink");
            let outside = std::env::temp_dir()
                .join(format!("clat-instructions-outside-{}", std::process::id()));
            std::fs::write(&outside, "secret").expect("write outside");
            std::os::unix::fs::symlink(&outside, root.join("AGENTS.md")).expect("symlink");
            let mut manager = PluginManager::root(ScopeKind::TrustedProject);
            let result = manager.mount_all(vec![
                Arc::new(PromptRegistryPlugin),
                Arc::new(ProjectInstructionsPlugin::new(Project::new(&root))),
            ]);
            let error = result.expect_err("must reject");
            assert!(error.to_string().contains("AGENTS.md"), "got: {error}");
            std::fs::remove_dir_all(root).expect("cleanup");
            std::fs::remove_file(outside).expect("cleanup outside");
        }
    }

    #[test]
    fn broken_instruction_symlink_is_an_explicit_error() {
        #[cfg(unix)]
        {
            let root = temp_root("broken-symlink");
            std::os::unix::fs::symlink("missing-target", root.join("AGENTS.md")).expect("symlink");
            let mut manager = PluginManager::root(ScopeKind::TrustedProject);
            let result = manager.mount_all(vec![
                Arc::new(PromptRegistryPlugin),
                Arc::new(ProjectInstructionsPlugin::new(Project::new(&root))),
            ]);
            let error = result.expect_err("broken symlink must not mean absent");
            assert!(error.to_string().contains("AGENTS.md"), "got: {error}");
            std::fs::remove_dir_all(root).expect("cleanup");
        }
    }

    #[test]
    fn empty_instructions_file_contributes_nothing() {
        let root = temp_root("empty");
        std::fs::write(root.join("AGENTS.md"), "   \n").expect("write");
        let text = mount_instructions(Project::new(&root));
        assert_eq!(text, default_only());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn bootstrap_scope_has_no_prompt_or_instructions_service() {
        // 信任门前不存在 PromptRegistry/指令贡献：bootstrap Catalog 只
        // 提供 core.trust，ProjectInstructionsPlugin 在内的全部项目插件
        // 只在信任迁移后挂载（零读取的组成事实）。
        let storage_root = temp_root("bootstrap-storage");
        let storage = Storage::open(storage_root.clone()).expect("storage");
        let backend = crate::plugins::storage::StorageBackend::new(storage);
        let mut manager = PluginManager::root(ScopeKind::Bootstrap);
        manager
            .mount_all(bootstrap_catalog(Arc::new(backend)))
            .expect("mount bootstrap");
        assert!(manager.require(PROMPT_SERVICE).is_err());
        manager.close().expect("close");
        std::fs::remove_dir_all(storage_root).expect("cleanup");
    }
}
