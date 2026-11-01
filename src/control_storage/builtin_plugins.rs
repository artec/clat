//! Activation policy is immutable for a Host lifetime. Corruption never re-enables plugins.
use super::{ControlError, ControlStorage, control_error, json_file};
use crate::plugin::selection::Configuration;
use serde::{Deserialize, Serialize};
const NAME: &str = "builtin-plugins.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    unit: json_file::UnitTag,
    #[serde(default)]
    disabled: std::collections::BTreeSet<String>,
}
pub(super) fn load(dir: &cap_std::fs::Dir) -> Result<Configuration, ControlError> {
    let bytes = json_file::read_file(dir, NAME, Some(65_536))
        .map_err(|error| control_error(error.message()))?;
    parse(bytes.as_deref())
}
fn parse(bytes: Option<&[u8]>) -> Result<Configuration, ControlError> {
    let Some(bytes) = bytes else {
        return Ok(Configuration::default());
    };
    let file: File = serde_json::from_slice(bytes)
        .map_err(|error| control_error(format!("cannot read {NAME}: {error}")))?;
    if !file.unit.matches(("builtin-plugins", 1)) {
        return Err(control_error(format!("unsupported {NAME} unit/version")));
    }
    Ok(Configuration {
        disabled: file.disabled,
    })
}

pub(crate) struct Snapshot {
    pub(crate) config: Configuration,
    pub(crate) revision: String,
}

impl ControlStorage {
    pub(crate) fn read_builtin_plugins(&self) -> Result<Snapshot, ControlError> {
        use sha2::{Digest, Sha256};
        let bytes = json_file::read_file(&self.dir, NAME, Some(65_536))
            .map_err(|error| control_error(error.message()))?;
        let mut hash = Sha256::new();
        hash.update([u8::from(bytes.is_some())]);
        hash.update(bytes.as_deref().unwrap_or_default());
        Ok(Snapshot {
            config: parse(bytes.as_deref())?,
            revision: format!("{:x}", hash.finalize()),
        })
    }
    /// The Host serializes callers while retaining its root lease. Never update the startup cache.
    pub(crate) fn save_builtin_plugins(
        &self,
        expected: &str,
        config: &Configuration,
    ) -> Result<(), ControlError> {
        let before = self.read_builtin_plugins()?;
        if before.revision != expected {
            return Err(control_error("extension.config_conflict"));
        }
        if before.config == *config {
            return Ok(());
        }
        let file = File {
            unit: json_file::UnitTag::new("builtin-plugins", 1),
            disabled: config.disabled.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|error| control_error(error.to_string()))?;
        if text.len() > 65_536 {
            return Err(control_error("extension.config_limit"));
        }
        #[cfg(test)]
        let fault = self
            .builtin_write_fault
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        #[cfg(test)]
        if fault == 1 {
            return Err(control_error("extension.config_write_uncertain"));
        }
        crate::private_fs::write_text_atomic_in_dir(&self.dir, NAME, &text)
            .map_err(|_| control_error("extension.config_write_uncertain"))?;
        #[cfg(test)]
        if fault == 2 {
            return Err(control_error("extension.config_write_uncertain"));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fail_builtin_write(&self, after_publish: bool) {
        self.builtin_write_fault.store(
            if after_publish { 2 } else { 1 },
            std::sync::atomic::Ordering::SeqCst,
        );
    }
    pub(crate) fn builtin_plugins(&self) -> Configuration {
        self.builtin_plugins.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_activation_file_is_never_salvaged_into_enabled_defaults() {
        let (root, _) = crate::test_support::roots("static-config-corrupt");
        std::fs::create_dir_all(&root).unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        assert!(load(&dir).unwrap().disabled.is_empty());
        let file = root.join(NAME);
        for text in [
            "{",
            "null",
            "{}",
            r#"{"unit":{"name":"builtin-plugins","version":2},"disabled":[]}"#,
            r#"{"unit":{"name":"builtin-plugins","version":1},"diabled":[]}"#,
            r#"{"unit":{"name":"builtin-plugins","version":1},"disabled":true}"#,
        ] {
            std::fs::write(&file, text).unwrap();
            for _ in 0..2 {
                assert!(
                    load(&dir).is_err(),
                    "invalid activation facts cannot become default-enabled: {text}"
                );
                assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
            }
            assert_eq!(
                std::fs::read_dir(&root).unwrap().count(),
                1,
                "no salvage or synthesized files"
            );
        }
        std::fs::write(&file, " ".repeat(65_537)).unwrap();
        assert!(load(&dir).is_err());
        std::fs::write(
            &file,
            r#"{"unit":{"name":"builtin-plugins","version":1},"disabled":["builtin.compaction"]}"#,
        )
        .unwrap();
        assert!(load(&dir).unwrap().disabled.contains("builtin.compaction"));
        drop(dir);
        crate::test_support::cleanup_tree(&root);
    }
}
