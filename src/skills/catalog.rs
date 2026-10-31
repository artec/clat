//! Immutable request catalog, including the pure empty shape for feature absence.
use super::*;
#[derive(Clone, Debug)]
pub(crate) struct SkillCatalogSnapshot {
    pub entries: Vec<SkillCatalogEntry>,
    /// Collected so `/context` can expose the exact snapshot diagnostics without
    /// rescanning; the model/header paths still consume only the valid catalog.
    pub diagnostics: Vec<SkillDiagnostic>,
    pub(super) rendered_catalog: String,
}

impl SkillCatalogSnapshot {
    pub(crate) fn empty() -> Self {
        Self {
            entries: Vec::new(),
            diagnostics: Vec::new(),
            rendered_catalog: String::new(),
        }
    }

    pub(crate) fn instructions(&self) -> Option<&str> {
        (!self.entries.is_empty()).then_some(self.rendered_catalog.as_str())
    }

    pub(crate) fn header_json(&self) -> Value {
        Value::Array(
            self.entries
                .iter()
                .map(SkillCatalogEntry::header_json)
                .collect(),
        )
    }

    pub(super) fn entry(&self, name: &str) -> Option<&SkillCatalogEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}
