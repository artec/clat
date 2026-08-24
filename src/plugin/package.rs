//! Language-neutral CLAT plugin package manifest.
//!
//! The manifest is distribution metadata, not an executable Rust ABI. Both a
//! Rust-authored WASM component and a DSH-compatible MCP executable can use the
//! same identity/capability vocabulary; each runtime still enforces its own
//! capability boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub(crate) const MANIFEST_VERSION: u32 = 1;
pub(crate) const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
pub(crate) const MAX_MANIFEST_PROMPTS: usize = 32;
pub(crate) const MAX_MANIFEST_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_MANIFEST_ENTRY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginPackageManifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub runtime: PluginRuntime,
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    #[serde(default)]
    pub prompts: Vec<ManifestPrompt>,
    #[serde(default)]
    pub config_schema: Option<Value>,
    #[serde(default)]
    pub compatibility: Option<PluginCompatibility>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginRuntime {
    pub kind: PluginRuntimeKind,
    pub entry: String,
    pub sha256: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PluginRuntimeKind {
    WasmComponent,
    McpStdio,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginCapabilities {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub prompts: bool,
    #[serde(default)]
    pub sampling: bool,
    #[serde(default)]
    pub elicitation: bool,
    #[serde(default)]
    pub host_context: bool,
    #[serde(default)]
    pub host_tools: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManifestPrompt {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub system: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginCompatibility {
    pub kind: String,
    #[serde(default)]
    pub revision: Option<String>,
}

impl PluginPackageManifest {
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("stat manifest {}: {error}", path.display()))?;
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "manifest {} is {} bytes; the cap is {MAX_MANIFEST_BYTES}",
                path.display(),
                metadata.len()
            ));
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("read manifest {}: {error}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse manifest {}: {error}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(format!(
                "unsupported manifestVersion {}; expected {MANIFEST_VERSION}",
                self.manifest_version
            ));
        }
        validate_identifier(&self.id, "plugin id", 128)?;
        if self.name.trim().is_empty()
            || self.name.chars().count() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err("plugin name must contain 1..=128 non-control characters".into());
        }
        if self.version.trim().is_empty()
            || self.version.chars().count() > 64
            || self
                .version
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err("plugin version must contain 1..=64 non-whitespace characters".into());
        }
        if self.description.chars().count() > 4096 {
            return Err("plugin description exceeds 4096 characters".into());
        }
        validate_relative_entry(&self.runtime.entry)?;
        validate_sha256(&self.runtime.sha256)?;
        if self.runtime.args.len() > 64 || self.runtime.args.iter().any(|arg| arg.len() > 4096) {
            return Err("runtime args exceed the 64 item / 4096 byte-per-item limits".into());
        }
        if self.capabilities.host_tools.len() > 64 {
            return Err("capabilities.hostTools exposes more than 64 entries".into());
        }
        let mut host_tools = std::collections::HashSet::new();
        for name in &self.capabilities.host_tools {
            validate_identifier(name, "host tool", 128)?;
            if !host_tools.insert(name.as_str()) {
                return Err(format!("duplicate host tool capability `{name}`"));
            }
        }
        if self.prompts.len() > MAX_MANIFEST_PROMPTS {
            return Err(format!(
                "manifest exposes more than {MAX_MANIFEST_PROMPTS} prompts"
            ));
        }
        let mut prompt_names = std::collections::HashSet::new();
        for prompt in &self.prompts {
            validate_identifier(&prompt.name, "prompt name", 128)?;
            if !prompt_names.insert(prompt.name.as_str()) {
                return Err(format!("duplicate prompt name `{}`", prompt.name));
            }
            if prompt.description.chars().count() > 1024 {
                return Err(format!(
                    "prompt `{}` description exceeds 1024 characters",
                    prompt.name
                ));
            }
            if prompt.system.len() > MAX_MANIFEST_PROMPT_BYTES {
                return Err(format!(
                    "prompt `{}` exceeds {MAX_MANIFEST_PROMPT_BYTES} bytes",
                    prompt.name
                ));
            }
        }
        if !self.prompts.is_empty() && !self.capabilities.prompts {
            return Err("manifest has prompts but capabilities.prompts is false".into());
        }
        if let Some(compatibility) = &self.compatibility {
            if compatibility.kind.trim().is_empty()
                || compatibility.kind.len() > 64
                || compatibility.kind.chars().any(char::is_control)
            {
                return Err("compatibility.kind must contain 1..=64 bytes".into());
            }
            if compatibility
                .revision
                .as_ref()
                .is_some_and(|revision| revision.trim().is_empty() || revision.len() > 256)
            {
                return Err("compatibility.revision must contain 1..=256 bytes".into());
            }
        }
        validate_config_schema(self.config_schema.as_ref())
    }

    pub(crate) fn entry_path(&self, manifest_path: &Path) -> Result<PathBuf, String> {
        self.validate()?;
        let parent = manifest_path
            .parent()
            .ok_or_else(|| "manifest has no parent directory".to_owned())?;
        let package_root = parent
            .canonicalize()
            .map_err(|error| format!("resolve package root {}: {error}", parent.display()))?;
        let candidate = parent
            .join(&self.runtime.entry)
            .canonicalize()
            .map_err(|error| {
                format!(
                    "resolve package entry {}: {error}",
                    parent.join(&self.runtime.entry).display()
                )
            })?;
        if !candidate.starts_with(&package_root) {
            return Err("runtime.entry resolves outside the package directory".into());
        }
        Ok(candidate)
    }

    /// Verify the manifest entry before either installer publication or
    /// runtime activation. The entry digest is the executable-code identity;
    /// the package store additionally binds the complete directory tree.
    pub(crate) fn verify_entry_digest(&self, manifest_path: &Path) -> Result<PathBuf, String> {
        let entry = self.entry_path(manifest_path)?;
        if !entry.is_file() {
            return Err(format!(
                "runtime.entry is not a regular file: {}",
                entry.display()
            ));
        }
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| "package manifest has no parent directory".to_owned())?
            .canonicalize()
            .map_err(|error| format!("canonicalize package root: {error}"))?;
        let dir = cap_std::fs::Dir::open_ambient_dir(&package_root, cap_std::ambient_authority())
            .map_err(|error| format!("open package root: {error}"))?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let relative = Path::new(&self.runtime.entry);
        let metadata = dir
            .symlink_metadata(relative)
            .map_err(|error| format!("stat package entry {}: {error}", entry.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "runtime.entry must be a regular no-follow file: {}",
                entry.display()
            ));
        }
        let size = metadata.len();
        if size > MAX_MANIFEST_ENTRY_BYTES {
            return Err(format!(
                "package entry is {size} bytes; the cap is {MAX_MANIFEST_ENTRY_BYTES}"
            ));
        }
        use sha2::Digest as _;
        use std::io::Read as _;
        let mut file = dir
            .open_with(relative, &options)
            .map_err(|error| format!("open package entry {}: {error}", entry.display()))?;
        let mut digest = sha2::Sha256::new();
        let copied = std::io::copy(
            &mut file.by_ref().take(MAX_MANIFEST_ENTRY_BYTES + 1),
            &mut digest,
        )
        .map_err(|error| format!("hash package entry {}: {error}", entry.display()))?;
        if copied != size {
            return Err(format!(
                "package entry changed while hashing: {}",
                entry.display()
            ));
        }
        let actual = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let expected = self.runtime.sha256.trim().trim_start_matches("sha256:");
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "package entry sha256 mismatch: manifest {expected}, actual {actual}"
            ));
        }
        Ok(entry)
    }

    /// Deliberately small install-time JSON Schema guard. Full schema handling
    /// belongs in a future settings UI; the runtime still rejects the common
    /// dangerous mismatch (an object schema receiving a scalar) and missing
    /// required keys before executing third-party code.
    pub(crate) fn validate_config(&self, config: Option<&Value>) -> Result<(), String> {
        let Some(schema) = self.config_schema.as_ref() else {
            return Ok(());
        };
        let Some(schema_object) = schema.as_object() else {
            return Err("configSchema must be a JSON object".into());
        };
        if schema_object.get("type").and_then(Value::as_str) == Some("object") {
            let required = schema_object.get("required").and_then(Value::as_array);
            if config.is_none() && required.is_none_or(Vec::is_empty) {
                return Ok(());
            }
            let config = config.ok_or_else(|| "plugin requires a config object".to_owned())?;
            let object = config
                .as_object()
                .ok_or_else(|| "plugin config must be an object".to_owned())?;
            if let Some(required) = required {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        return Err(format!("plugin config is missing required key `{key}`"));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_identifier(value: &str, subject: &str, max: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    {
        return Err(format!(
            "{subject} `{value}` must be lowercase ASCII [a-z0-9._-], start with a letter/digit, and fit {max} bytes"
        ));
    }
    Ok(())
}

fn validate_relative_entry(entry: &str) -> Result<(), String> {
    let path = Path::new(entry);
    if entry.is_empty() || path.is_absolute() {
        return Err("runtime.entry must be a non-empty package-relative path".into());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("runtime.entry must not escape the package directory".into());
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    let digest = value.trim().trim_start_matches("sha256:");
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("runtime.sha256 must be exactly 64 hexadecimal characters".into());
    }
    Ok(())
}

fn validate_config_schema(schema: Option<&Value>) -> Result<(), String> {
    let Some(schema) = schema else {
        return Ok(());
    };
    if !schema.is_object() {
        return Err("configSchema must be a JSON object".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> PluginPackageManifest {
        serde_json::from_value(json!({
            "manifestVersion": 1,
            "id": "dev.clat.greeter",
            "name": "Greeter",
            "version": "1.0.0",
            "runtime": {
                "kind": "wasm-component",
                "entry": "greeter.wasm",
                "sha256": "00".repeat(32)
            },
            "capabilities": { "tools": true, "prompts": true },
            "prompts": [{ "name": "greeting", "system": "Be friendly." }],
            "configSchema": {
                "type": "object",
                "required": ["greeting"]
            }
        }))
        .expect("manifest shape")
    }

    #[test]
    fn validates_identity_entry_digest_prompt_and_config() {
        let manifest = manifest();
        manifest.validate().expect("valid");
        manifest
            .validate_config(Some(&json!({"greeting": "hello"})))
            .expect("config");
        assert!(manifest.validate_config(Some(&json!({}))).is_err());
        let mut optional = manifest.clone();
        optional.config_schema = Some(json!({"type": "object"}));
        optional
            .validate_config(None)
            .expect("an object schema without required keys is optional");
    }

    #[test]
    fn rejects_package_escape_and_undeclared_prompts() {
        let mut manifest = manifest();
        manifest.runtime.entry = "../escape.wasm".into();
        assert!(manifest.validate().is_err());
        manifest.runtime.entry = "plugin.wasm".into();
        manifest.capabilities.prompts = false;
        assert!(manifest.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_package_entry_symlink_that_escapes_the_package() {
        use std::os::unix::fs::symlink;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("clat-manifest-symlink-{unique}"));
        let package = root.join("package");
        std::fs::create_dir_all(&package).expect("package");
        let outside = root.join("outside.wasm");
        std::fs::write(&outside, b"component").expect("outside fixture");
        symlink(&outside, package.join("greeter.wasm")).expect("entry symlink");
        let error = manifest()
            .entry_path(&package.join("clat-plugin.json"))
            .expect_err("symlink escape must fail");
        assert!(error.contains("outside the package"), "{error}");
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
