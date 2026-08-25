//! Explicit local knowledge store for `builtin.memory` (Agent phase 4-A).
//!
//! Mutations are reachable only through human Application/command APIs. The
//! model receives a bounded read-only search tool and an immutable run-start
//! injection; it can never add, edit, or delete records itself.

use crate::control_storage::json_file::{self, Loaded, UnitTag};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

const FILE_NAME: &str = "memory.json";
const UNIT: (&str, u64) = ("memory", 1);
pub(crate) const DEFAULT_TOP_K: usize = 5;
pub(crate) const MAX_TOP_K: usize = 10;
pub(crate) const MAX_INJECTION_BYTES: usize = 8 * 1024;
pub(crate) const MAX_RESULT_BYTES: usize = 16 * 1024;
const MAX_CONTENT_BYTES: usize = 16 * 1024;
const MAX_SOURCE_BYTES: usize = 1024;
const MAX_SOURCE_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECORDS: usize = 1_000;
const MAX_MEMORY_FILE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    Project,
    User,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "project" => Self::Project,
            "user" => Self::User,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    #[serde(rename = "projectKey", skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    pub content: String,
    pub source: String,
    #[serde(rename = "sourceDigest", skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub revision: u64,
    pub digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryHit {
    pub record: MemoryRecord,
    pub stale: bool,
    pub score: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct MemoryInjection {
    pub instructions: String,
    pub header: serde_json::Value,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryFile {
    unit: UnitTag,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    records: BTreeMap<String, MemoryRecord>,
}

impl MemoryFile {
    fn empty() -> Self {
        Self {
            unit: UnitTag::new(UNIT.0, UNIT.1),
            records: BTreeMap::new(),
        }
    }
}

pub(crate) struct MemoryService {
    root: PathBuf,
    dir: Dir,
    project_root: PathBuf,
    project_key: String,
    state: Mutex<MemoryFile>,
    diagnostic: Mutex<Option<String>>,
}

impl MemoryService {
    pub(crate) fn open(storage_root: &Path, project_root: &Path) -> Result<Self, String> {
        let root = storage_root
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize memory root: {error}"))?;
        let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority())
            .map_err(|error| format!("cannot open memory root: {error}"))?;
        let (state, diagnostic) = match json_file::load_limited::<MemoryFile>(
            &dir,
            &root,
            FILE_NAME,
            UNIT,
            MAX_MEMORY_FILE_BYTES,
        ) {
            Ok(Loaded::Missing) => (MemoryFile::empty(), None),
            Ok(Loaded::Intact(file)) => {
                validate_memory_file(&file)?;
                (file, None)
            }
            Ok(Loaded::Salvaged { remnant }) => (
                MemoryFile::empty(),
                Some(format!(
                    "memory state was torn; preserved as {remnant} and restarted empty"
                )),
            ),
            Err(error) => return Err(error.message()),
        };
        let project_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let project_key =
            crate::session::key::ProjectKey::from_cwd(&project_root.to_string_lossy()).bucket;
        Ok(Self {
            root,
            dir,
            project_root,
            project_key,
            state: Mutex::new(state),
            diagnostic: Mutex::new(diagnostic),
        })
    }

    pub(crate) fn take_diagnostic(&self) -> Option<String> {
        self.diagnostic.lock().expect("memory diagnostic").take()
    }

    pub(crate) fn add(
        &self,
        scope: MemoryScope,
        content: &str,
        source: Option<&str>,
    ) -> Result<MemoryRecord, String> {
        let content = validate_content(content)?;
        let source = validate_source(source.unwrap_or("user-command"))?;
        if scope == MemoryScope::User && source.starts_with("file:") {
            return Err("user-scope memory cannot cite project file content".into());
        }
        let source_digest = self.source_digest(scope, &source)?;
        let now = crate::control_storage::timestamp::now_iso8601();
        let id = uuid::Uuid::new_v4().to_string();
        let record = MemoryRecord {
            id: id.clone(),
            scope,
            project_key: (scope == MemoryScope::Project).then(|| self.project_key.clone()),
            digest: digest(&content),
            content,
            source,
            source_digest,
            created_at: now.clone(),
            updated_at: now,
            revision: 1,
        };
        self.commit(|file| {
            if file.records.len() >= MAX_RECORDS {
                return Err(format!("memory store is limited to {MAX_RECORDS} records"));
            }
            file.records.insert(id, record.clone());
            Ok(record)
        })
    }

    pub(crate) fn update(
        &self,
        id: &str,
        expected_revision: u64,
        content: &str,
    ) -> Result<MemoryRecord, String> {
        let content = validate_content(content)?;
        self.commit(|file| {
            let existing = file
                .records
                .get(id)
                .cloned()
                .ok_or_else(|| format!("memory `{id}` not found"))?;
            self.ensure_visible(&existing)?;
            if existing.revision != expected_revision {
                return Err(format!(
                    "memory `{id}` revision conflict: expected {expected_revision}, current {}",
                    existing.revision
                ));
            }
            let mut next = existing;
            next.content = content;
            next.digest = digest(&next.content);
            next.updated_at = crate::control_storage::timestamp::now_iso8601();
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or_else(|| "memory revision exhausted".to_owned())?;
            file.records.insert(id.to_owned(), next.clone());
            Ok(next)
        })
    }

    pub(crate) fn delete(&self, id: &str, expected_revision: u64) -> Result<MemoryRecord, String> {
        self.commit(|file| {
            let existing = file
                .records
                .get(id)
                .cloned()
                .ok_or_else(|| format!("memory `{id}` not found"))?;
            self.ensure_visible(&existing)?;
            if existing.revision != expected_revision {
                return Err(format!(
                    "memory `{id}` revision conflict: expected {expected_revision}, current {}",
                    existing.revision
                ));
            }
            file.records.remove(id);
            Ok(existing)
        })
    }

    pub(crate) fn get(&self, id: &str) -> Result<Option<MemoryHit>, String> {
        let record = self
            .state
            .lock()
            .map_err(|_| "memory lock poisoned".to_owned())?
            .records
            .get(id)
            .cloned();
        record
            .map(|record| {
                self.ensure_visible(&record)?;
                Ok(MemoryHit {
                    stale: self.is_stale(&record),
                    record,
                    score: 0,
                    reason: "selected by id".into(),
                })
            })
            .transpose()
    }

    pub(crate) fn list(&self, scope: Option<MemoryScope>) -> Result<Vec<MemoryHit>, String> {
        let mut records = self
            .state
            .lock()
            .map_err(|_| "memory lock poisoned".to_owned())?
            .records
            .values()
            .filter(|record| self.visible(record))
            .filter(|record| scope.is_none_or(|scope| record.scope == scope))
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| (Reverse(record.updated_at.clone()), record.id.clone()));
        Ok(records
            .into_iter()
            .map(|record| MemoryHit {
                stale: self.is_stale(&record),
                record,
                score: 0,
                reason: "listed explicitly".into(),
            })
            .collect())
    }

    pub(crate) fn search(
        &self,
        query: &str,
        top_k: usize,
        max_bytes: usize,
    ) -> Result<Vec<MemoryHit>, String> {
        if query.trim().is_empty() || query.len() > 4096 {
            return Err("memory query must contain 1..=4096 UTF-8 bytes".into());
        }
        let top_k = top_k.clamp(1, MAX_TOP_K);
        let max_bytes = max_bytes.clamp(1, MAX_RESULT_BYTES);
        let terms = terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let query_lower = query.to_lowercase();
        let mut ranked = self
            .state
            .lock()
            .map_err(|_| "memory lock poisoned".to_owned())?
            .records
            .values()
            .filter(|record| self.visible(record))
            .filter_map(|record| {
                let haystack = format!("{} {}", record.content, record.source).to_lowercase();
                let matched = terms
                    .iter()
                    .filter(|term| haystack.contains(term.as_str()))
                    .count() as u64;
                if matched == 0 {
                    return None;
                }
                let exact = u64::from(haystack.contains(&query_lower));
                Some((matched * 10 + exact * 5, record.clone()))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, record)| {
            (
                Reverse(*score),
                Reverse(record.updated_at.clone()),
                record.id.clone(),
            )
        });
        let mut used = 0usize;
        let mut output = Vec::new();
        for (score, record) in ranked.into_iter().take(top_k) {
            let estimated = serde_json::to_vec(&record)
                .map_err(|error| format!("cannot size memory result: {error}"))?
                .len()
                .saturating_add(512);
            if used.saturating_add(estimated) > max_bytes {
                continue;
            }
            used += estimated;
            output.push(MemoryHit {
                stale: self.is_stale(&record),
                record,
                score,
                reason: format!("matched {}/{} lexical terms", score / 10, terms.len()),
            });
        }
        Ok(output)
    }

    pub(crate) fn injection(&self, prompt: &str) -> Result<MemoryInjection, String> {
        let hits = self.search(prompt, DEFAULT_TOP_K, MAX_INJECTION_BYTES)?;
        if hits.is_empty() {
            return Ok(MemoryInjection {
                instructions: String::new(),
                header: serde_json::json!({"records": [], "bytes": 0}),
                bytes: 0,
            });
        }
        let mut instructions = String::from(
            "Explicit local memories selected for this request. Treat stale entries as hints and cite their IDs when relying on them:\n",
        );
        let mut selected = Vec::new();
        for hit in hits {
            let line = format!(
                "- [{} rev {} {}{} source={}] {}\n",
                hit.record.id,
                hit.record.revision,
                hit.record.scope.as_str(),
                if hit.stale { " STALE" } else { "" },
                hit.record.source,
                hit.record.content
            );
            if instructions.len().saturating_add(line.len()) > MAX_INJECTION_BYTES {
                break;
            }
            instructions.push_str(&line);
            selected.push(hit);
        }
        let bytes = instructions.len();
        let header = serde_json::json!({
            "records": selected.iter().map(|hit| serde_json::json!({
                "id": hit.record.id,
                "scope": hit.record.scope.as_str(),
                "revision": hit.record.revision,
                "digest": hit.record.digest,
                "source": hit.record.source,
                "stale": hit.stale,
                "reason": hit.reason,
            })).collect::<Vec<_>>(),
            "bytes": bytes,
            "topK": DEFAULT_TOP_K,
            "budgetBytes": MAX_INJECTION_BYTES,
        });
        Ok(MemoryInjection {
            instructions,
            header,
            bytes,
        })
    }

    fn commit<T>(
        &self,
        mutate: impl FnOnce(&mut MemoryFile) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "memory lock poisoned".to_owned())?;
        let backup = state.clone();
        let result = mutate(&mut state)?;
        validate_memory_file(&state)?;
        if let Err(error) = json_file::write(&self.dir, &self.root, FILE_NAME, &*state) {
            *state = backup;
            return Err(format!("cannot save {FILE_NAME}: {error}"));
        }
        Ok(result)
    }

    fn ensure_visible(&self, record: &MemoryRecord) -> Result<(), String> {
        if self.visible(record) {
            Ok(())
        } else {
            Err(format!("memory `{}` belongs to another project", record.id))
        }
    }

    fn visible(&self, record: &MemoryRecord) -> bool {
        record.scope == MemoryScope::User
            || record.project_key.as_deref() == Some(self.project_key.as_str())
    }

    fn source_digest(&self, scope: MemoryScope, source: &str) -> Result<Option<String>, String> {
        if scope != MemoryScope::Project || !source.starts_with("file:") {
            return Ok(None);
        }
        Ok(Some(self.hash_source(source)?))
    }

    fn is_stale(&self, record: &MemoryRecord) -> bool {
        let Some(expected) = &record.source_digest else {
            return false;
        };
        match self.hash_source(&record.source) {
            Ok(actual) => &actual != expected,
            Err(_) => true,
        }
    }

    fn source_relative_path(&self, source: &str) -> Result<PathBuf, String> {
        let relative = source
            .strip_prefix("file:")
            .ok_or_else(|| "file source must start with `file:`".to_owned())?;
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err("memory file source must be a normal project-relative path".into());
        }
        Ok(path.to_path_buf())
    }

    fn hash_source(&self, source: &str) -> Result<String, String> {
        let relative = self.source_relative_path(source)?;
        let bytes = crate::project::Project::new(&self.project_root)
            .read_file_limited(&relative, MAX_SOURCE_FILE_BYTES.saturating_add(1))
            .map_err(|error| {
                format!(
                    "cannot read memory source `{}`: {error}",
                    relative.display()
                )
            })?
            .ok_or_else(|| format!("memory source `{}` does not exist", relative.display()))?;
        if bytes.len() > MAX_SOURCE_FILE_BYTES {
            return Err("memory source must be a regular file no larger than 4 MiB".into());
        }
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

fn validate_content(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_CONTENT_BYTES
        || value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(format!(
            "memory content must contain 1..={MAX_CONTENT_BYTES} UTF-8 bytes"
        ));
    }
    Ok(value.to_owned())
}

fn validate_memory_file(file: &MemoryFile) -> Result<(), String> {
    if file.records.len() > MAX_RECORDS {
        return Err(format!("memory store exceeds {MAX_RECORDS} records"));
    }
    for (key, record) in &file.records {
        if key != &record.id || uuid::Uuid::parse_str(&record.id).is_err() || record.revision == 0 {
            return Err("memory record id/revision is invalid".into());
        }
        if validate_content(&record.content)?.as_str() != record.content
            || validate_source(&record.source)?.as_str() != record.source
            || record.digest != digest(&record.content)
            || !is_hex_digest(&record.digest)
        {
            return Err(format!("memory record `{key}` content metadata is invalid"));
        }
        if !valid_timestamp(&record.created_at)
            || !valid_timestamp(&record.updated_at)
            || record.updated_at < record.created_at
        {
            return Err(format!("memory record `{key}` timestamps are invalid"));
        }
        match record.scope {
            MemoryScope::Project => {
                if record
                    .project_key
                    .as_deref()
                    .is_none_or(|value| value.is_empty() || value.len() > 4096)
                {
                    return Err(format!("memory record `{key}` lacks a project key"));
                }
                let valid_source_digest = if record.source.starts_with("file:") {
                    record.source_digest.as_deref().is_some_and(is_hex_digest)
                } else {
                    record.source_digest.is_none()
                };
                if !valid_source_digest {
                    return Err(format!("memory record `{key}` source digest is invalid"));
                }
            }
            MemoryScope::User => {
                if record.project_key.is_some()
                    || record.source_digest.is_some()
                    || record.source.starts_with("file:")
                {
                    return Err(format!(
                        "user memory record `{key}` carries project authority"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_timestamp(value: &str) -> bool {
    value.len() == 24
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value.as_bytes().get(10) == Some(&b'T')
        && value.as_bytes().get(13) == Some(&b':')
        && value.as_bytes().get(16) == Some(&b':')
        && value.as_bytes().get(19) == Some(&b'.')
        && value.ends_with('Z')
        && crate::control_storage::timestamp::iso8601_to_unix_seconds(value).is_some()
}

fn validate_source(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_SOURCE_BYTES || value.chars().any(char::is_control) {
        return Err(format!(
            "memory source must contain 1..={MAX_SOURCE_BYTES} printable UTF-8 bytes"
        ));
    }
    Ok(value.to_owned())
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn terms(value: &str) -> Vec<String> {
    let mut terms = BTreeSet::new();
    for term in value
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(str::trim)
        .filter(|term| term.chars().count() >= 2)
    {
        terms.insert(term.to_lowercase());
    }
    terms.into_iter().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(label: &str) -> (MemoryService, PathBuf, PathBuf) {
        let (storage, project) = crate::test_support::roots(label);
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let service = MemoryService::open(&storage, &project).unwrap();
        (service, storage, project)
    }

    #[test]
    fn explicit_mutations_are_durable_cas_and_project_scoped() {
        let (service, storage, project) = service("memory-cas");
        let created = service
            .add(MemoryScope::Project, "Rust source lives in src", None)
            .unwrap();
        assert_eq!(created.revision, 1);
        assert!(storage.join(FILE_NAME).is_file());
        let conflict = service.update(&created.id, 9, "wrong").unwrap_err();
        assert!(conflict.contains("revision conflict"));
        let updated = service
            .update(&created.id, 1, "Rust code lives in src")
            .unwrap();
        assert_eq!(updated.revision, 2);
        drop(service);
        let reopened = MemoryService::open(&storage, &project).unwrap();
        assert_eq!(reopened.get(&created.id).unwrap().unwrap().record, updated);
        assert!(reopened.delete(&created.id, 1).is_err());
        reopened.delete(&created.id, 2).unwrap();
        assert!(reopened.get(&created.id).unwrap().is_none());
    }

    #[test]
    fn search_is_bounded_deterministic_and_file_sources_become_stale() {
        let (service, _storage, project) = service("memory-search");
        std::fs::write(project.join("facts.txt"), "v1").unwrap();
        let sourced = service
            .add(
                MemoryScope::Project,
                "alpha architecture fact",
                Some("file:facts.txt"),
            )
            .unwrap();
        service
            .add(MemoryScope::User, "alpha personal preference", None)
            .unwrap();
        assert!(
            service
                .add(MemoryScope::User, "pollute", Some("file:facts.txt"))
                .is_err()
        );
        let hits = service
            .search("alpha architecture", 1, MAX_RESULT_BYTES)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.id, sourced.id);
        assert!(!hits[0].stale);
        std::fs::write(project.join("facts.txt"), "v2").unwrap();
        assert!(service.get(&sourced.id).unwrap().unwrap().stale);
        assert!(service.injection("alpha architecture").unwrap().bytes <= MAX_INJECTION_BYTES);
        assert!(
            service
                .search(&"q".repeat(4097), 1, MAX_RESULT_BYTES)
                .is_err()
        );
    }

    #[test]
    fn oversized_project_file_source_is_rejected_without_creating_a_record() {
        let (service, _storage, project) = service("memory-source-oversized");
        let source = std::fs::File::create(project.join("oversized.bin")).unwrap();
        source.set_len(MAX_SOURCE_FILE_BYTES as u64 + 1).unwrap();

        let error = service
            .add(
                MemoryScope::Project,
                "must remain bounded",
                Some("file:oversized.bin"),
            )
            .unwrap_err();
        assert!(error.contains("no larger than 4 MiB"), "{error}");
        assert!(service.list(None).unwrap().is_empty());
    }

    #[test]
    fn intact_json_with_invalid_record_metadata_fails_closed() {
        let (service, storage, project) = service("memory-invalid-record");
        drop(service);
        let mut file = serde_json::to_value(MemoryFile::empty()).unwrap();
        file["records"]["not-a-uuid"] = serde_json::json!({
            "id": "not-a-uuid",
            "scope": "user",
            "content": "tampered",
            "source": "user-command",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "revision": 1,
            "digest": "00"
        });
        std::fs::write(
            storage.join(FILE_NAME),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
        assert!(MemoryService::open(&storage, &project).is_err());
    }

    #[test]
    fn project_scope_cannot_be_read_or_mutated_from_another_project() {
        let (service, storage, project) = service("memory-project-isolation");
        let local = service
            .add(MemoryScope::Project, "project-only fact", None)
            .unwrap();
        let global = service
            .add(MemoryScope::User, "user-approved global fact", None)
            .unwrap();
        drop(service);
        let other_project = project.parent().unwrap().join("other-project");
        std::fs::create_dir_all(&other_project).unwrap();
        let other = MemoryService::open(&storage, &other_project).unwrap();
        let visible = other.list(None).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].record.id, global.id);
        assert!(other.get(&local.id).is_err());
        assert!(other.update(&local.id, 1, "polluted").is_err());
        assert!(other.delete(&local.id, 1).is_err());
    }

    #[test]
    fn version_torn_and_oversized_files_follow_fail_closed_recovery() {
        let (service, storage, project) = service("memory-recovery");
        drop(service);
        std::fs::write(
            storage.join(FILE_NAME),
            br#"{"unit":{"name":"memory","version":2},"records":{}}"#,
        )
        .unwrap();
        assert!(MemoryService::open(&storage, &project).is_err());

        std::fs::write(storage.join(FILE_NAME), b"{torn").unwrap();
        let recovered = MemoryService::open(&storage, &project).unwrap();
        assert!(recovered.take_diagnostic().unwrap().contains("preserved"));
        assert!(!storage.join(FILE_NAME).exists());
        assert!(std::fs::read_dir(&storage).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("memory.json.torn-")
        }));
        drop(recovered);

        let oversized = std::fs::File::create(storage.join(FILE_NAME)).unwrap();
        oversized.set_len(MAX_MEMORY_FILE_BYTES as u64 + 1).unwrap();
        assert!(MemoryService::open(&storage, &project).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn memory_store_never_follows_a_symlink() {
        use std::os::unix::fs::symlink;

        let (service, storage, project) = service("memory-symlink");
        drop(service);
        let victim = storage.join("victim.json");
        let victim_bytes = b"do not touch";
        std::fs::write(&victim, victim_bytes).unwrap();
        symlink(&victim, storage.join(FILE_NAME)).unwrap();
        assert!(MemoryService::open(&storage, &project).is_err());
        assert_eq!(std::fs::read(victim).unwrap(), victim_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn project_file_source_never_follows_a_symlink_outside_the_project() {
        use std::os::unix::fs::symlink;

        let (service, storage, project) = service("memory-source-symlink");
        let victim = storage.join("outside-secret.txt");
        std::fs::write(&victim, "outside secret").unwrap();
        symlink(&victim, project.join("facts.txt")).unwrap();

        let error = service
            .add(
                MemoryScope::Project,
                "must not cite escaped content",
                Some("file:facts.txt"),
            )
            .unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(service.list(None).unwrap().is_empty());
    }
}
