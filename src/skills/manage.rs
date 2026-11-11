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
        if expected == "missing" {
            crate::private_fs::write_text_new_atomic_in_dir(&bundle, "SKILL.md", content)?;
        } else {
            crate::private_fs::write_text_atomic_in_dir(&bundle, "SKILL.md", content)?;
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
        bundle.remove_file("SKILL.md").map_err(|e| e.to_string())?;
        #[cfg(unix)]
        bundle
            .open(".")
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        layer.remove_dir(name).map_err(|e| e.to_string())?;
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
