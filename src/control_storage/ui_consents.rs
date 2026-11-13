//! Application consent storage: bounded, fail closed and updated under the
//! control-storage lock. Session decisions take precedence over global allow.

use super::ControlStorage;
use serde::{Deserialize, Serialize};

const NAME: &str = "ui-consents.json";
const LIMIT: usize = 256;
// 256 entries with maximally escaped 256-byte session IDs still fit.
const FILE_LIMIT: usize = 524_288;

/// Bundle identity is preferred; the fallback is the canonical executable,
/// never the caller-provided path spelling. PID is deliberately not identity.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn app_identity(
    bundle: Option<&str>,
    executable: &std::path::Path,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    if let Some(bundle) = bundle.filter(|id| !id.is_empty()) {
        hash.update(b"macos:bundle-id:");
        hash.update(bundle.as_bytes());
    } else {
        let path = executable.canonicalize().map_err(|e| e.to_string())?;
        hash.update(b"macos:executable-path:");
        hash.update(path.as_os_str().as_encoded_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decision {
    Allow,
    Deny,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Unit {
    name: String,
    version: u8,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) decision: Decision,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsentFile {
    unit: Unit,
    entries: Vec<Entry>,
}

impl Default for ConsentFile {
    fn default() -> Self {
        Self {
            unit: Unit {
                name: "ui-consents".into(),
                version: 1,
            },
            entries: Vec::new(),
        }
    }
}

impl ConsentFile {
    fn parse(text: &str) -> Result<Self, String> {
        let file: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if file.unit.name != "ui-consents" || file.unit.version != 1 || file.entries.len() > LIMIT {
            return Err("invalid UI consent file".into());
        }
        for (index, entry) in file.entries.iter().enumerate() {
            validate(&entry.app, entry.session.as_deref(), entry.decision)?;
            validate_name(entry.name.as_deref())?;
            if file.entries[..index]
                .iter()
                .any(|other| other.app == entry.app && other.session == entry.session)
            {
                return Err("duplicate UI consent".into());
            }
        }
        Ok(file)
    }

    fn decision(&self, app: &str, session: &str) -> Option<Decision> {
        self.entries
            .iter()
            .find(|e| e.app == app && e.session.as_deref() == Some(session))
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|e| e.app == app && e.session.is_none())
            })
            .map(|e| e.decision)
    }

    #[cfg(test)]
    fn set(&mut self, app: &str, session: Option<&str>, decision: Decision) -> Result<(), String> {
        self.set_named(app, session, decision, None)
    }

    fn set_named(
        &mut self,
        app: &str,
        session: Option<&str>,
        decision: Decision,
        name: Option<&str>,
    ) -> Result<(), String> {
        validate(app, session, decision)?;
        validate_name(name)?;
        self.revoke(app, session);
        self.entries.push(Entry {
            app: app.into(),
            name: name.map(str::to_owned),
            session: session.map(str::to_owned),
            decision,
        });
        if self.entries.len() > LIMIT {
            self.entries.remove(0);
        }
        Ok(())
    }

    fn revoke(&mut self, app: &str, session: Option<&str>) {
        self.entries
            .retain(|e| e.app != app || e.session.as_deref() != session);
    }
}

fn validate_name(name: Option<&str>) -> Result<(), String> {
    if name.is_some_and(|name| {
        name.is_empty() || name.len() > 384 || name.chars().any(char::is_control)
    }) {
        return Err("invalid UI consent application name".into());
    }
    Ok(())
}

fn validate(app: &str, session: Option<&str>, decision: Decision) -> Result<(), String> {
    if app.len() != 64
        || !app
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || session.is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        || (session.is_none() && decision == Decision::Deny)
    {
        return Err("invalid UI consent entry".into());
    }
    Ok(())
}

impl ControlStorage {
    fn load_ui_consents_locked(&self) -> Result<ConsentFile, String> {
        match crate::private_fs::read_private_text(&self.dir, NAME, FILE_LIMIT)? {
            Some(text) => ConsentFile::parse(&text),
            None => Ok(ConsentFile::default()),
        }
    }

    pub(crate) fn ui_consent(&self, app: &str, session: &str) -> Result<Option<Decision>, String> {
        validate(app, Some(session), Decision::Allow)?;
        let _lock = self.lock();
        Ok(self.load_ui_consents_locked()?.decision(app, session))
    }

    pub(crate) fn ui_consents(&self) -> Result<Vec<Entry>, String> {
        let _lock = self.lock();
        Ok(self.load_ui_consents_locked()?.entries)
    }

    #[cfg(test)]
    pub(crate) fn set_ui_consent(
        &self,
        app: &str,
        session: Option<&str>,
        decision: Decision,
    ) -> Result<(), String> {
        self.set_ui_consent_named(app, session, decision, None)
    }

    pub(crate) fn set_ui_consent_named(
        &self,
        app: &str,
        session: Option<&str>,
        decision: Decision,
        name: Option<&str>,
    ) -> Result<(), String> {
        let _lock = self.lock();
        let mut file = self.load_ui_consents_locked()?;
        file.set_named(app, session, decision, name)?;
        self.save_ui_consents_locked(&file)
    }

    pub(crate) fn revoke_ui_consent(&self, app: &str, session: Option<&str>) -> Result<(), String> {
        validate(app, session, Decision::Allow)?;
        let _lock = self.lock();
        let mut file = self.load_ui_consents_locked()?;
        file.revoke(app, session);
        self.save_ui_consents_locked(&file)
    }

    fn save_ui_consents_locked(&self, file: &ConsentFile) -> Result<(), String> {
        let text = serde_json::to_string(file).map_err(|e| e.to_string())?;
        if text.len() > FILE_LIMIT {
            return Err("UI consent file exceeds limit".into());
        }
        crate::private_fs::write_text_atomic_in_dir(&self.dir, NAME, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_consent_identity_uses_bundle_or_canonical_executable() {
        let root = crate::control_storage::tests::temp_root("ui-identity");
        let path = root.join("app");
        std::fs::write(&path, "executable").unwrap();
        let alias = root.join(".").join("app");
        assert_eq!(
            app_identity(Some("org.example.app"), &path).unwrap(),
            "1a47bc9f10428da436070db853afab34a86b7928797314a3b2f9ff62cb490fb6"
        );
        assert_eq!(
            app_identity(None, &path).unwrap(),
            app_identity(None, &alias).unwrap()
        );
        assert_eq!(
            app_identity(Some("org.example.app"), &path).unwrap(),
            app_identity(Some("org.example.app"), &root.join("missing")).unwrap()
        );
        assert_ne!(
            app_identity(Some("org.example.app"), &path).unwrap(),
            app_identity(None, &path).unwrap()
        );
        assert!(app_identity(None, &root.join("missing")).is_err());
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn ui_consent_storage_round_trip_and_corruption_fails_closed() {
        let root = crate::control_storage::tests::temp_root("ui-consent");
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let storage = ControlStorage::open_ready(&root).unwrap();
        let app = "a".repeat(64);
        assert_eq!(storage.ui_consent(&app, "one").unwrap(), None);
        storage.set_ui_consent(&app, None, Decision::Allow).unwrap();
        storage
            .set_ui_consent(&app, Some("one"), Decision::Deny)
            .unwrap();
        assert_eq!(storage.ui_consents().unwrap().len(), 2);
        assert_eq!(
            storage.ui_consent(&app, "one").unwrap(),
            Some(Decision::Deny)
        );
        storage.revoke_ui_consent(&app, Some("one")).unwrap();
        assert_eq!(storage.ui_consents().unwrap().len(), 1);
        drop(storage);
        let storage = ControlStorage::open_ready(&root).unwrap();
        assert_eq!(
            storage.ui_consent(&app, "one").unwrap(),
            Some(Decision::Allow)
        );
        std::fs::write(root.join(NAME), "{}").unwrap();
        assert!(storage.ui_consent(&app, "one").is_err());
        assert!(storage.set_ui_consent(&app, None, Decision::Allow).is_err());
        crate::test_support::cleanup_tree(&root);
    }

    #[cfg(unix)]
    #[test]
    fn ui_consent_storage_rejects_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;
        let root = crate::control_storage::tests::temp_root("ui-consent-link");
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let storage = ControlStorage::open_ready(&root).unwrap();
        let target = root.join("other");
        std::fs::write(&target, "untouched").unwrap();
        symlink(&target, root.join(NAME)).unwrap();
        let app = "a".repeat(64);
        assert!(storage.ui_consent(&app, "one").is_err());
        assert!(storage.set_ui_consent(&app, None, Decision::Allow).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
        crate::test_support::cleanup_tree(&root);
    }

    #[test]
    fn ui_consent_session_denial_overrides_global_allow_and_revoke_is_scoped() {
        let app = "a".repeat(64);
        let mut records = ConsentFile::default();
        records.set(&app, None, Decision::Allow).unwrap();
        records
            .set(&app, Some("session-1"), Decision::Deny)
            .unwrap();
        assert_eq!(records.decision(&app, "session-1"), Some(Decision::Deny));
        assert_eq!(records.decision(&app, "session-2"), Some(Decision::Allow));
        records.revoke(&app, Some("session-1"));
        assert_eq!(records.decision(&app, "session-1"), Some(Decision::Allow));
        records.revoke(&app, None);
        assert_eq!(records.decision(&app, "session-2"), None);
    }

    #[test]
    fn ui_consent_fifo_is_bounded_and_invalid_global_denial_is_rejected() {
        let mut records = ConsentFile::default();
        for i in 0..257 {
            records
                .set(&format!("{i:064x}"), Some("session"), Decision::Deny)
                .unwrap();
        }
        assert_eq!(records.entries.len(), 256);
        assert_eq!(records.decision(&format!("{:064x}", 0), "session"), None);
        assert!(records.set(&"f".repeat(64), None, Decision::Deny).is_err());
    }

    #[test]
    fn ui_consent_rejects_corrupt_or_unrecognized_files() {
        for text in [
            "{}",
            r#"{"unit":{"name":"ui-consents","version":2},"entries":[]}"#,
            r#"{"unit":{"name":"ui-consents","version":1},"entries":[{"app":"bad","session":null,"decision":"allow"}]}"#,
        ] {
            assert!(ConsentFile::parse(text).is_err(), "{text}");
        }
    }
}
