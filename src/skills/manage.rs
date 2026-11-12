//! Filesystem capability for human-managed Skills. This does not execute Skill scripts.
//! All paths after the trusted project/storage root are single validated components.
use super::*;
use cap_std::fs::Dir;
use serde_json::{Value, json};
use std::io::ErrorKind;

pub(crate) struct SkillFiles;

impl SkillFiles {
    pub(crate) fn names(root: &Path, user: bool) -> Result<Vec<Value>, String> {
        let Some(layer) = layer_dir(root, user, false)? else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        for item in layer
            .entries()
            .map_err(|e| format!("enumerate skill layer: {e}"))?
        {
            let item = item.map_err(|e| format!("read skill layer: {e}"))?;
            let name = item.file_name().to_string_lossy().into_owned();
            if validate_name(&name).is_err() {
                continue;
            }
            let Some(bundle) = (match bundle_dir(&layer, &name, false) {
                Ok(value) => value,
                Err(_) => continue,
            }) else {
                continue;
            };
            let raw = match read_regular_file_bounded_at(
                &bundle,
                Path::new("SKILL.md"),
                MAX_SKILL_FILE_BYTES,
                "SKILL.md",
            ) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let Ok(text) = String::from_utf8(raw) else {
                continue;
            };
            let Ok(parsed) = parse_skill(&text) else {
                continue;
            };
            if parsed.name != name {
                continue;
            }
            entries.push(json!({"name":name,"description":parsed.description,
                "requires_execution":parsed.requires_execution,"revision":sha256_hex(text.as_bytes())}));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(entries)
    }

    pub(crate) fn read(root: &Path, user: bool, name: &str) -> Result<Value, String> {
        validate_name(name)?;
        let Some(layer) = layer_dir(root, user, false)? else {
            return Ok(json!({"name":name,"revision":"missing","text":null}));
        };
        let Some(bundle) = bundle_dir(&layer, name, false)? else {
            return Ok(json!({"name":name,"revision":"missing","text":null}));
        };
        match read_regular_file_bounded_at(
            &bundle,
            Path::new("SKILL.md"),
            MAX_SKILL_FILE_BYTES,
            "SKILL.md",
        ) {
            Ok(bytes) => {
                let content = String::from_utf8(bytes).map_err(|_| "Skill is not UTF-8")?;
                let parsed = parse_skill(&content)?;
                if parsed.name != name {
                    return Err("Skill name differs from its directory".into());
                }
                Ok(
                    json!({"name":name,"revision":sha256_hex(content.as_bytes()),
                    "text":content,"description":parsed.description,
                    "requires_execution":parsed.requires_execution}),
                )
            }
            Err(error) if error.contains("No such file") || error.contains("not found") => {
                Ok(json!({"name":name,"revision":"missing","text":null}))
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn save(
        root: &Path,
        user: bool,
        name: &str,
        expected: &str,
        content: &str,
    ) -> Result<Value, String> {
        validate_name(name)?;
        if content.len() > MAX_SKILL_FILE_BYTES || content.contains('\0') {
            return Err("Skill text exceeds the size limit or contains NUL".into());
        }
        let parsed = parse_skill(content)?;
        if parsed.name != name {
            return Err("Skill frontmatter name must match its directory".into());
        }
        if expected.len() != 64 && expected != "missing" {
            return Err("invalid Skill revision".into());
        }
        let layer = layer_dir(root, user, true)?.ok_or("Skill layer is unavailable")?;
        let bundle = bundle_dir(&layer, name, true)?.ok_or("Skill bundle is unavailable")?;
        let previous = current_bytes(&bundle)?;
        let revision = previous
            .as_ref()
            .map(|s| sha256_hex(s.as_bytes()))
            .unwrap_or_else(|| "missing".into());
        if revision != expected {
            return Err("Skill was changed; reload before saving".into());
        }
        // CI-X3（2026-11-12）：Windows 上发布（硬链接/重命名）要求父目录
        // DELETE 共享；open_dir_nofollow 句柄（cap-std 刻意不带
        // FILE_SHARE_DELETE，见 I3）握在手里会自冲突出 os error 32。
        // 校验完成后释放 nofollow 句柄，用 ambient 句柄（共享位宽松，
        // 与 mcp-managed.json 路径同款、Windows CI 已验证）执行发布。
        drop(bundle);
        drop(layer);
        let components: &[&std::ffi::OsStr] = if user {
            &[std::ffi::OsStr::new("skills"), std::ffi::OsStr::new(name)]
        } else {
            &[
                std::ffi::OsStr::new(".clat"),
                std::ffi::OsStr::new("skills"),
                std::ffi::OsStr::new(name),
            ]
        };
        let bundle_path = components
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part));
        let publish_dir =
            Dir::open_ambient_dir(&bundle_path, ambient_authority()).map_err(|e| e.to_string())?;
        if expected == "missing" {
            crate::private_fs::write_text_new_atomic_in_dir(&publish_dir, "SKILL.md", content)?;
        } else {
            crate::private_fs::write_text_atomic_in_dir(&publish_dir, "SKILL.md", content)?;
        }
        Ok(json!({"saved":true,"name":name,"revision":sha256_hex(content.as_bytes())}))
    }

    pub(crate) fn remove(
        root: &Path,
        user: bool,
        name: &str,
        expected: &str,
    ) -> Result<Value, String> {
        validate_name(name)?;
        let layer = layer_dir(root, user, false)?.ok_or("Skill layer is absent")?;
        let bundle = bundle_dir(&layer, name, false)?.ok_or("Skill not found")?;
        let previous = current_bytes(&bundle)?.ok_or("Skill not found")?;
        if sha256_hex(previous.as_bytes()) != expected {
            return Err("Skill was changed; reload before deleting".into());
        }
        // A richer bundle may have scripts/assets/references. Do not partly delete it.
        let names: Vec<_> = bundle
            .entries()
            .map_err(|e| e.to_string())?
            .map(|item| item.map(|e| e.file_name()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        if names.len() != 1 || names[0] != "SKILL.md" {
            return Err(
                "Skill has bundled resources; use the bundle import/removal workflow".into(),
            );
        }
        // 同 save：先释放 nofollow 句柄再删（Windows DELETE 共享自冲突）；
        // 删除本身经 ambient 句柄执行（remove_file 后 remove_dir，不级联）。
        drop(bundle);
        drop(layer);
        let layer_components: &[&std::ffi::OsStr] = if user {
            &[std::ffi::OsStr::new("skills")]
        } else {
            &[
                std::ffi::OsStr::new(".clat"),
                std::ffi::OsStr::new("skills"),
            ]
        };
        let layer_path = layer_components
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part));
        let ambient_layer =
            Dir::open_ambient_dir(&layer_path, ambient_authority()).map_err(|e| e.to_string())?;
        ambient_layer
            .remove_file(std::path::Path::new(name).join("SKILL.md").as_os_str())
            .map_err(|e| e.to_string())?;
        ambient_layer.remove_dir(name).map_err(|e| e.to_string())?;
        Ok(json!({"removed":true,"name":name}))
    }
}

fn current_bytes(bundle: &Dir) -> Result<Option<String>, String> {
    match bundle.symlink_metadata("SKILL.md") {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("inspect SKILL.md: {e}")),
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            Err("SKILL.md must be a real regular file".into())
        }
        Ok(_) => {
            let bytes = read_regular_file_bounded_at(
                bundle,
                Path::new("SKILL.md"),
                MAX_SKILL_FILE_BYTES,
                "SKILL.md",
            )?;
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "SKILL.md is not UTF-8".into())
        }
    }
}

fn layer_dir(root: &Path, user: bool, create: bool) -> Result<Option<Dir>, String> {
    let mut current = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|e| format!("open Skill root: {e}"))?;
    let components: &[&str] = if user {
        &["skills"]
    } else {
        &[".clat", "skills"]
    };
    for part in components {
        match current.symlink_metadata(part) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err("Skill layer must be a real directory".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound && !create => return Ok(None),
            Err(e) if e.kind() == ErrorKind::NotFound => current
                .create_dir(part)
                .map_err(|e| format!("create Skill layer: {e}"))?,
            Err(e) => return Err(format!("inspect Skill layer: {e}")),
        }
        current = open_dir_nofollow(&current, Path::new(part))?;
    }
    Ok(Some(current))
}

fn bundle_dir(layer: &Dir, name: &str, create: bool) -> Result<Option<Dir>, String> {
    validate_name(name)?;
    match layer.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err("Skill bundle must be a real directory".into());
        }
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound && !create => return Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => layer
            .create_dir(name)
            .map_err(|e| format!("create Skill bundle: {e}"))?,
        Err(e) => return Err(format!("inspect Skill bundle: {e}")),
    }
    open_dir_nofollow(layer, Path::new(name)).map(Some)
}
