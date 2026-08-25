//! AG-3 Skills B1/B2: bounded, layered skill discovery plus a run-frozen
//! catalog used by the model prompt, request/header, and the `skill` tool.
//!
//! The loader never executes a skill or resource. Executable skills are only
//! catalogued when the existing graduated sandbox can satisfy
//! `sandbox=required, network=false`; actual execution remains an ordinary
//! Execute tool call through ProcessService.

use crate::sandbox::{SandboxRequest, SandboxService};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

pub(crate) const MAX_SKILL_NAME_CHARS: usize = 64;
pub(crate) const MAX_SKILL_DESCRIPTION_CHARS: usize = 500;
pub(crate) const MAX_SKILL_BODY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SKILLS_PER_LAYER: usize = 128;
pub(crate) const MAX_RENDERED_CATALOG_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RESOURCE_BYTES: usize = 1024 * 1024;
const MAX_SKILL_FILE_BYTES: usize = MAX_SKILL_BODY_BYTES + 16 * 1024;

const BUNDLED_SKILLS: &[&str] = &[
    r#"---
name: code-review
description: Review a scoped change for correctness, regressions, and maintainability.
requires-execution: false
---
Review the scoped change against its intended behavior. Prioritize correctness, regressions, security boundaries, error handling, and tests. Report concrete findings with file locations and avoid unrelated rewrites.
"#,
    r#"---
name: bug-diagnosis
description: Diagnose a reproducible defect before proposing the smallest safe fix.
requires-execution: false
---
Reproduce or trace the defect, identify the failing invariant and causal path, distinguish symptoms from root cause, and propose the smallest safe correction with discriminating validation.
"#,
    r#"---
name: change-verification
description: Verify a completed change with focused evidence and regression checks.
requires-execution: false
---
Verify the requested change against its acceptance criteria. Prefer targeted checks that would fail without the change, then run the relevant broader gates. Report what was actually verified and any remaining unverified boundary.
"#,
    r#"---
name: docs-sync
description: Synchronize documentation with implemented behavior without inventing capabilities.
requires-execution: false
---
Update documentation to match the implemented public behavior, configuration, constraints, and verification evidence. Remove stale claims and do not describe unimplemented or unverified capabilities as complete.
"#,
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SkillSource {
    Bundled,
    User,
    Project,
}

impl SkillSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SkillDiagnostic {
    pub source: SkillSource,
    pub name: Option<String>,
    pub kind: String,
    pub message: String,
}

impl SkillDiagnostic {
    fn new(
        source: SkillSource,
        name: Option<String>,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source,
            name,
            kind: kind.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
enum SkillOrigin {
    Bundled {
        body: String,
    },
    File {
        layer_root: Arc<Dir>,
        bundle_name: OsString,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub digest: String,
    pub requires_execution: bool,
    origin: SkillOrigin,
}

impl SkillCatalogEntry {
    pub(crate) fn header_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "source": self.source.as_str(),
            "digest": self.digest,
            "requiresExecution": self.requires_execution,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SkillCatalogSnapshot {
    pub entries: Vec<SkillCatalogEntry>,
    /// Collected so `/context` can expose the exact snapshot diagnostics without
    /// rescanning; the model/header paths still consume only the valid catalog.
    pub diagnostics: Vec<SkillDiagnostic>,
    rendered_catalog: String,
}

impl SkillCatalogSnapshot {
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

    fn entry(&self, name: &str) -> Option<&SkillCatalogEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

#[derive(Default)]
pub(crate) struct SkillCatalogSlot {
    current: RwLock<Option<Arc<SkillCatalogSnapshot>>>,
}

impl SkillCatalogSlot {
    pub(crate) fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn install(&self, snapshot: Arc<SkillCatalogSnapshot>) {
        if let Ok(mut current) = self.current.write() {
            *current = Some(snapshot);
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut current) = self.current.write() {
            *current = None;
        }
    }

    pub(crate) fn snapshot(&self) -> Option<Arc<SkillCatalogSnapshot>> {
        self.current.read().ok().and_then(|value| value.clone())
    }
}

pub(crate) struct SkillsService {
    project_root: PathBuf,
    user_root: PathBuf,
    sandbox: Arc<SandboxService>,
}

impl SkillsService {
    pub(crate) fn new(
        project_root: PathBuf,
        storage_root: PathBuf,
        sandbox: Arc<SandboxService>,
    ) -> Self {
        Self {
            project_root,
            user_root: storage_root.join("skills"),
            sandbox,
        }
    }

    pub(crate) fn snapshot(&self) -> Result<Arc<SkillCatalogSnapshot>, String> {
        let mut diagnostics = Vec::new();
        let bundled = scan_bundled(&mut diagnostics)?;
        let user = scan_layer(&self.user_root, SkillSource::User, &mut diagnostics)?;
        let project = scan_layer(
            &self.project_root.join(".clat/skills"),
            SkillSource::Project,
            &mut diagnostics,
        )?;

        // Resolve ownership first. A higher-priority unavailable executable
        // skill must not silently reveal a lower-priority skill with the same
        // name.
        let mut selected = BTreeMap::new();
        for candidate in bundled.into_values() {
            selected.insert(candidate.name.clone(), candidate);
        }
        for candidate in user.into_values() {
            selected.insert(candidate.name.clone(), candidate);
        }
        for candidate in project.into_values() {
            selected.insert(candidate.name.clone(), candidate);
        }

        let execution_capability = self.execution_capability();
        let mut entries = Vec::with_capacity(selected.len());
        for (_, candidate) in selected {
            if candidate.requires_execution
                && let Err(reason) = &execution_capability
            {
                diagnostics.push(SkillDiagnostic::new(
                    candidate.source,
                    Some(candidate.name.clone()),
                    "unavailable",
                    format!(
                        "requires execution but graduated required sandbox is unavailable: {reason}"
                    ),
                ));
                continue;
            }
            entries.push(candidate);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        let rendered_catalog = render_catalog(&entries);
        if rendered_catalog.len() > MAX_RENDERED_CATALOG_BYTES {
            return Err(format!(
                "skills catalog exceeds {MAX_RENDERED_CATALOG_BYTES} rendered bytes"
            ));
        }
        let header = Value::Array(entries.iter().map(SkillCatalogEntry::header_json).collect());
        let header_bytes = serde_json::to_vec(&header)
            .map_err(|error| format!("serialize skills catalog: {error}"))?;
        if header_bytes.len() > MAX_RENDERED_CATALOG_BYTES {
            return Err(format!(
                "skills header catalog exceeds {MAX_RENDERED_CATALOG_BYTES} bytes"
            ));
        }

        Ok(Arc::new(SkillCatalogSnapshot {
            entries,
            diagnostics,
            rendered_catalog,
        }))
    }

    fn execution_capability(&self) -> Result<(), String> {
        let planned = self.sandbox.plan(
            OsString::from("clat-skill-capability-probe"),
            Vec::new(),
            SandboxRequest::Required,
            false,
        )?;
        if planned.facts.provider == "none" || planned.facts.enforcement != "full" {
            return Err("required sandbox has no graduated OS enforcement".into());
        }
        Ok(())
    }

    pub(crate) fn load(
        &self,
        snapshot: &SkillCatalogSnapshot,
        name: &str,
        resource: Option<&str>,
    ) -> Result<Value, String> {
        let entry = snapshot
            .entry(name)
            .ok_or_else(|| format!("skill `{name}` is not in this run's catalog"))?;
        let current = read_current(entry)?;
        if current.name != entry.name
            || current.description != entry.description
            || current.requires_execution != entry.requires_execution
            || current.digest != entry.digest
        {
            return Err(format!(
                "skill `{name}` is stale; start a new run to rebuild the catalog"
            ));
        }

        match resource {
            None => Ok(json!({
                "name": entry.name,
                "source": entry.source.as_str(),
                "digest": entry.digest,
                "requires_execution": entry.requires_execution,
                "resource_base": format!("skill://{}/", entry.name),
                "body": current.body,
            })),
            Some(resource) => self.load_resource(entry, &current.body, resource),
        }
    }

    fn load_resource(
        &self,
        entry: &SkillCatalogEntry,
        body: &str,
        resource: &str,
    ) -> Result<Value, String> {
        let SkillOrigin::File {
            layer_root,
            bundle_name,
        } = &entry.origin
        else {
            return Err(format!("bundled skill `{}` has no resources", entry.name));
        };
        validate_resource_path(resource)?;
        if !resource_is_referenced(body, resource) {
            return Err(format!(
                "resource `{resource}` is not explicitly referenced by skill `{}`",
                entry.name
            ));
        }
        let bundle = open_dir_nofollow(layer_root, Path::new(bundle_name))?;
        let bytes = read_resource_bounded_nofollow(
            &bundle,
            Path::new(resource),
            MAX_RESOURCE_BYTES,
            "skill resource",
        )?;
        let digest = sha256_hex(&bytes);
        match String::from_utf8(bytes.clone()) {
            Ok(text) => Ok(json!({
                "name": entry.name,
                "source": entry.source.as_str(),
                "skill_digest": entry.digest,
                "resource": resource,
                "digest": digest,
                "bytes": bytes.len(),
                "binary": false,
                "content": text,
            })),
            Err(_) => Ok(json!({
                "name": entry.name,
                "source": entry.source.as_str(),
                "skill_digest": entry.digest,
                "resource": resource,
                "digest": digest,
                "bytes": bytes.len(),
                "binary": true,
            })),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    requires_execution: bool,
}

struct ParsedSkill {
    name: String,
    description: String,
    requires_execution: bool,
    body: String,
    digest: String,
}

fn parse_skill(text: &str) -> Result<ParsedSkill, String> {
    let (yaml, body) = split_frontmatter(text)?;
    let frontmatter: SkillFrontmatter = serde_yaml_ng::from_str(yaml)
        .map_err(|error| format!("malformed YAML frontmatter: {error}"))?;
    validate_name(&frontmatter.name)?;
    let description_chars = frontmatter.description.chars().count();
    if description_chars == 0 || description_chars > MAX_SKILL_DESCRIPTION_CHARS {
        return Err(format!(
            "description must contain 1..={MAX_SKILL_DESCRIPTION_CHARS} characters"
        ));
    }
    if body.len() > MAX_SKILL_BODY_BYTES {
        return Err(format!("skill body exceeds {MAX_SKILL_BODY_BYTES} bytes"));
    }
    Ok(ParsedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        requires_execution: frontmatter.requires_execution,
        body: body.to_owned(),
        digest: sha256_hex(body.as_bytes()),
    })
}

fn split_frontmatter(text: &str) -> Result<(&str, &str), String> {
    let first_line_end = text
        .find('\n')
        .ok_or_else(|| "SKILL.md must start with YAML frontmatter".to_owned())?;
    if text[..first_line_end].trim_end_matches('\r') != "---" {
        return Err("SKILL.md must start with `---` YAML frontmatter".into());
    }
    let yaml_start = first_line_end + 1;
    let mut offset = yaml_start;
    for line in text[yaml_start..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            let body_start = offset + line.len();
            return Ok((&text[yaml_start..offset], &text[body_start..]));
        }
        offset += line.len();
    }
    if offset < text.len() && text[offset..].trim_end_matches('\r') == "---" {
        return Ok((&text[yaml_start..offset], ""));
    }
    Err("SKILL.md YAML frontmatter is missing its closing `---`".into())
}

fn validate_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SKILL_NAME_CHARS {
        return Err(format!(
            "skill name must contain 1..={MAX_SKILL_NAME_CHARS} ASCII characters"
        ));
    }
    let mut segment_start = true;
    for byte in bytes {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => segment_start = false,
            b'-' if !segment_start => segment_start = true,
            _ => return Err("skill name must be kebab-case ASCII".into()),
        }
    }
    if segment_start {
        return Err("skill name must be kebab-case ASCII".into());
    }
    Ok(())
}

fn scan_bundled(
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Result<BTreeMap<String, SkillCatalogEntry>, String> {
    let mut candidates = BTreeMap::<String, Vec<SkillCatalogEntry>>::new();
    for text in BUNDLED_SKILLS {
        match parse_skill(text) {
            Ok(parsed) => {
                let entry = SkillCatalogEntry {
                    name: parsed.name.clone(),
                    description: parsed.description,
                    source: SkillSource::Bundled,
                    digest: parsed.digest,
                    requires_execution: parsed.requires_execution,
                    origin: SkillOrigin::Bundled { body: parsed.body },
                };
                candidates.entry(parsed.name).or_default().push(entry);
            }
            Err(error) => {
                return Err(format!("invalid compiled-in skill: {error}"));
            }
        }
    }
    resolve_same_layer(candidates, SkillSource::Bundled, diagnostics)
}

fn scan_layer(
    root: &Path,
    source: SkillSource,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Result<BTreeMap<String, SkillCatalogEntry>, String> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("scan {} skills root: {error}", source.as_str())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{} skills root must be a real directory, not a symlink",
            source.as_str()
        ));
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("resolve {} skills root: {error}", source.as_str()))?;
    let layer_root = Arc::new(
        Dir::open_ambient_dir(&canonical_root, ambient_authority())
            .map_err(|error| format!("open {} skills root: {error}", source.as_str()))?,
    );
    let mut entries = fs::read_dir(&canonical_root)
        .map_err(|error| format!("read {} skills root: {error}", source.as_str()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("enumerate {} skills root: {error}", source.as_str()))?;
    entries.sort_by_key(std::fs::DirEntry::path);

    let candidate_count = entries
        .iter()
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_dir() || kind.is_symlink())
                .unwrap_or(true)
        })
        .count();
    if candidate_count > MAX_SKILLS_PER_LAYER {
        return Err(format!(
            "{} skills layer contains {candidate_count} candidates; maximum is {MAX_SKILLS_PER_LAYER}",
            source.as_str()
        ));
    }

    let mut candidates = BTreeMap::<String, Vec<SkillCatalogEntry>>::new();
    for entry in entries {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(
                    source,
                    None,
                    "metadata",
                    format!("cannot inspect skill candidate: {error}"),
                ));
                continue;
            }
        };
        if file_type.is_symlink() {
            diagnostics.push(SkillDiagnostic::new(
                source,
                None,
                "symlink",
                "symlink skill candidate ignored",
            ));
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        let bundle_name = entry.file_name();
        let bundle = match open_dir_nofollow(&layer_root, Path::new(&bundle_name)) {
            Ok(bundle) => bundle,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(
                    source,
                    None,
                    "containment",
                    format!("cannot open skill candidate without following links: {error}"),
                ));
                continue;
            }
        };
        let bytes = match read_regular_file_bounded_at(
            &bundle,
            Path::new("SKILL.md"),
            MAX_SKILL_FILE_BYTES,
            "SKILL.md",
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(source, None, "invalid", error));
                continue;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                diagnostics.push(SkillDiagnostic::new(
                    source,
                    None,
                    "utf8",
                    "SKILL.md is not UTF-8",
                ));
                continue;
            }
        };
        let parsed = match parse_skill(&text) {
            Ok(parsed) => parsed,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(source, None, "invalid", error));
                continue;
            }
        };
        let skill_name = parsed.name.clone();
        candidates
            .entry(skill_name.clone())
            .or_default()
            .push(SkillCatalogEntry {
                name: skill_name,
                description: parsed.description,
                source,
                digest: parsed.digest,
                requires_execution: parsed.requires_execution,
                origin: SkillOrigin::File {
                    layer_root: Arc::clone(&layer_root),
                    bundle_name,
                },
            });
    }
    resolve_same_layer(candidates, source, diagnostics)
}

fn resolve_same_layer(
    candidates: BTreeMap<String, Vec<SkillCatalogEntry>>,
    source: SkillSource,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Result<BTreeMap<String, SkillCatalogEntry>, String> {
    let mut resolved = BTreeMap::new();
    for (name, mut entries) in candidates {
        if entries.len() != 1 {
            diagnostics.push(SkillDiagnostic::new(
                source,
                Some(name),
                "duplicate",
                "multiple skills in the same layer declare this name; all were ignored",
            ));
            continue;
        }
        resolved.insert(name, entries.pop().expect("one entry"));
    }
    Ok(resolved)
}

fn read_current(entry: &SkillCatalogEntry) -> Result<ParsedSkill, String> {
    match &entry.origin {
        SkillOrigin::Bundled { body } => Ok(ParsedSkill {
            name: entry.name.clone(),
            description: entry.description.clone(),
            requires_execution: entry.requires_execution,
            body: body.clone(),
            digest: sha256_hex(body.as_bytes()),
        }),
        SkillOrigin::File {
            layer_root,
            bundle_name,
        } => {
            let bundle = open_dir_nofollow(layer_root, Path::new(bundle_name))
                .map_err(|error| format!("skill `{}` is stale: {error}", entry.name))?;
            let bytes = read_regular_file_bounded_at(
                &bundle,
                Path::new("SKILL.md"),
                MAX_SKILL_FILE_BYTES,
                "SKILL.md",
            )
            .map_err(|error| format!("skill `{}` is stale: {error}", entry.name))?;
            let text = String::from_utf8(bytes).map_err(|_| {
                format!(
                    "skill `{}` is stale; SKILL.md is no longer UTF-8",
                    entry.name
                )
            })?;
            parse_skill(&text).map_err(|error| format!("skill `{}` is stale: {error}", entry.name))
        }
    }
}

fn render_catalog(entries: &[SkillCatalogEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut rendered = String::from(
        "Available skills (load exact instructions with the read-only `skill` tool):\n",
    );
    if entries.iter().any(|entry| entry.requires_execution) {
        rendered.push_str(
            "Skills marked requires-execution=true never grant execution authority; run any referenced script only through ordinary exec with sandbox=required and network=false.\n",
        );
    }
    for entry in entries {
        rendered.push_str("- ");
        rendered.push_str(&entry.name);
        rendered.push_str(": ");
        rendered.push_str(&entry.description);
        rendered.push_str(" [source=");
        rendered.push_str(entry.source.as_str());
        rendered.push_str(", digest=");
        rendered.push_str(&entry.digest);
        if entry.requires_execution {
            rendered.push_str(", requires-execution=true");
        }
        rendered.push_str("]\n");
    }
    rendered
}

fn validate_resource_path(resource: &str) -> Result<(), String> {
    if resource.is_empty() || resource.contains('\\') {
        return Err("skill resource must be a non-empty portable relative path".into());
    }
    let path = Path::new(resource);
    if path.is_absolute() {
        return Err("skill resource must be relative".into());
    }
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Err("skill resource must be relative".into());
    };
    if first != "references" && first != "scripts" && first != "assets" {
        return Err("skill resource must live under references/, scripts/, or assets/".into());
    }
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            return Err("skill resource cannot contain `..`, `.`, or path roots".into());
        }
    }
    Ok(())
}

fn resource_is_referenced(body: &str, resource: &str) -> bool {
    body.match_indices(resource).any(|(offset, _)| {
        let before = body[..offset].chars().next_back();
        let after = body[offset + resource.len()..].chars().next();
        boundary(before) && boundary(after)
    })
}

fn boundary(value: Option<char>) -> bool {
    value.is_none_or(|character| {
        character.is_whitespace()
            || matches!(
                character,
                '(' | ')' | '[' | ']' | '<' | '>' | '`' | '\'' | '"' | ':' | ','
            )
    })
}

fn open_dir_nofollow(parent: &Dir, name: &Path) -> Result<Dir, String> {
    if name.components().count() != 1
        || !matches!(name.components().next(), Some(Component::Normal(_)))
    {
        return Err("skill path contains an invalid directory component".into());
    }
    let parent_file = parent
        .try_clone()
        .map_err(|error| format!("clone skill directory capability: {error}"))?
        .into_std_file();
    cap_primitives::fs::open_dir_nofollow(&parent_file, name)
        .map(Dir::from_std_file)
        .map_err(|error| format!("open skill directory without following links: {error}"))
}

fn read_resource_bounded_nofollow(
    bundle: &Dir,
    relative: &Path,
    max: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    let mut components = relative.components().peekable();
    let mut current = bundle
        .try_clone()
        .map_err(|error| format!("clone skill bundle capability: {error}"))?;
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return Err("skill resource contains an invalid path component".into());
        };
        let name = Path::new(component);
        if components.peek().is_none() {
            return read_regular_file_bounded_at(&current, name, max, label);
        }
        current = open_dir_nofollow(&current, name)?;
    }
    Err("skill resource path is empty".into())
}

fn read_regular_file_bounded_at(
    parent: &Dir,
    name: &Path,
    max: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    if name.components().count() != 1
        || !matches!(name.components().next(), Some(Component::Normal(_)))
    {
        return Err(format!("{label} path must be one regular file component"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = parent.open_with(name, &options).map_err(|error| {
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::ELOOP) {
            return format!("{label} may not be a symlink or escape its capability root");
        }
        let message = error.to_string();
        if message.contains("outside of the filesystem") {
            return format!("{label} may not be a symlink or escape its capability root");
        }
        format!("open {label} without following links: {message}")
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("read {label} metadata: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} must be a regular non-symlink file"));
    }
    if metadata.len() > max as u64 {
        return Err(format!("{label} exceeds {max} bytes"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {label}: {error}"))?;
    if bytes.len() > max {
        return Err(format!("{label} exceeds {max} bytes"));
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::PermissionMode;
    use crate::sandbox::SandboxModeSource;
    use std::sync::RwLock;

    fn make_service(tag: &str, mode: SandboxModeSource) -> (PathBuf, PathBuf, SkillsService) {
        let (storage, project) = crate::test_support::roots(tag);
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(&project).unwrap();
        let sandbox = Arc::new(SandboxService::new(project.clone(), mode).unwrap());
        let service = SkillsService::new(project.clone(), storage.clone(), sandbox);
        (storage, project, service)
    }

    fn write_skill(
        root: &Path,
        dir: &str,
        name: &str,
        description: &str,
        requires_execution: bool,
        body: &str,
    ) -> PathBuf {
        let bundle = root.join(dir);
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\nrequires-execution: {requires_execution}\n---\n{body}"
            ),
        )
        .unwrap();
        bundle
    }

    fn entry<'a>(snapshot: &'a SkillCatalogSnapshot, name: &str) -> &'a SkillCatalogEntry {
        snapshot
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("missing skill {name}"))
    }

    #[test]
    fn layering_is_project_then_user_then_bundled_and_order_is_deterministic() {
        let (storage, project, service) =
            make_service("skills-layering", SandboxModeSource::Classic);
        let user_root = storage.join("skills");
        let project_root = project.join(".clat/skills");
        let user_bundle = write_skill(
            &user_root,
            "user-review",
            "code-review",
            "User review instructions.",
            false,
            "USER BODY",
        );
        let project_bundle = write_skill(
            &project_root,
            "project-review",
            "code-review",
            "Project review instructions.",
            false,
            "PROJECT BODY",
        );
        write_skill(
            &project_root,
            "alpha-dir",
            "alpha-skill",
            "Alpha deterministic ordering.",
            false,
            "ALPHA",
        );

        let first = service.snapshot().unwrap();
        assert_eq!(entry(&first, "code-review").source, SkillSource::Project);
        assert_eq!(
            service.load(&first, "code-review", None).unwrap()["body"],
            "PROJECT BODY"
        );
        let names = first
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "final catalog order is by skill name");

        fs::remove_dir_all(project_bundle).unwrap();
        let second = service.snapshot().unwrap();
        assert_eq!(entry(&second, "code-review").source, SkillSource::User);
        assert_eq!(
            service.load(&second, "code-review", None).unwrap()["body"],
            "USER BODY"
        );

        fs::remove_dir_all(user_bundle).unwrap();
        let third = service.snapshot().unwrap();
        assert_eq!(entry(&third, "code-review").source, SkillSource::Bundled);
        assert!(
            service.load(&third, "code-review", None).unwrap()["body"]
                .as_str()
                .unwrap()
                .contains("correctness")
        );
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn same_layer_conflicts_and_malformed_candidates_are_diagnosed_not_order_winners() {
        let (storage, project, service) =
            make_service("skills-conflict", SandboxModeSource::Classic);
        let root = project.join(".clat/skills");
        write_skill(&root, "one", "same-name", "First duplicate.", false, "ONE");
        write_skill(&root, "two", "same-name", "Second duplicate.", false, "TWO");
        let malformed = root.join("bad-yaml");
        fs::create_dir_all(&malformed).unwrap();
        fs::write(
            malformed.join("SKILL.md"),
            "---\nname: bad-yaml\ndescription: [not a scalar]\n---\nbody",
        )
        .unwrap();
        let bad_bool = root.join("bad-bool");
        fs::create_dir_all(&bad_bool).unwrap();
        fs::write(
            bad_bool.join("SKILL.md"),
            "---\nname: bad-bool\ndescription: bad bool\nrequires-execution: yes-please\n---\nbody",
        )
        .unwrap();
        let non_utf8 = root.join("non-utf8");
        fs::create_dir_all(&non_utf8).unwrap();
        fs::write(non_utf8.join("SKILL.md"), [0xff, 0xfe, 0xfd]).unwrap();
        let oversized = root.join("oversized");
        fs::create_dir_all(&oversized).unwrap();
        let mut large = b"---\nname: oversized\ndescription: oversized body\n---\n".to_vec();
        large.extend(std::iter::repeat_n(b'x', MAX_SKILL_BODY_BYTES + 1));
        fs::write(oversized.join("SKILL.md"), large).unwrap();

        let snapshot = service.snapshot().unwrap();
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "same-name")
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "bad-yaml")
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "bad-bool")
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "oversized")
        );
        assert!(snapshot.diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == "duplicate" && diagnostic.name.as_deref() == Some("same-name")
        }));
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == "utf8")
        );
        assert!(
            snapshot
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == "invalid")
                .count()
                >= 3
        );
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn layer_and_rendered_catalog_limits_fail_loud_instead_of_truncating() {
        let (storage, project, service) =
            make_service("skills-layer-limit", SandboxModeSource::Classic);
        let root = project.join(".clat/skills");
        for index in 0..=MAX_SKILLS_PER_LAYER {
            write_skill(
                &root,
                &format!("candidate-{index:03}"),
                &format!("candidate-{index:03}"),
                "candidate",
                false,
                "body",
            );
        }
        let error = service
            .snapshot()
            .expect_err("129 candidates must fail loud");
        assert!(error.contains("maximum is 128"));
        crate::test_support::cleanup_tree(storage.parent().unwrap());

        let (storage, project, service) =
            make_service("skills-catalog-limit", SandboxModeSource::Classic);
        let root = project.join(".clat/skills");
        let description = "d".repeat(MAX_SKILL_DESCRIPTION_CHARS);
        for index in 0..MAX_SKILLS_PER_LAYER {
            write_skill(
                &root,
                &format!("wide-{index:03}"),
                &format!("wide-{index:03}"),
                &description,
                false,
                "body",
            );
        }
        let error = service
            .snapshot()
            .expect_err("rendered catalog must fail loud");
        assert!(error.contains("catalog exceeds") || error.contains("header catalog exceeds"));
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn body_staleness_and_resource_fences_are_discriminating() {
        let (storage, project, service) =
            make_service("skills-resources", SandboxModeSource::Classic);
        let root = project.join(".clat/skills");
        let bundle = write_skill(
            &root,
            "resourceful",
            "resourceful",
            "Loads declared resources only.",
            false,
            "Read `references/guide.md`, then inspect `assets/blob.bin`. The script is `scripts/run.sh`.",
        );
        fs::create_dir_all(bundle.join("references")).unwrap();
        fs::create_dir_all(bundle.join("assets")).unwrap();
        fs::create_dir_all(bundle.join("scripts")).unwrap();
        fs::write(bundle.join("references/guide.md"), "guide text").unwrap();
        fs::write(bundle.join("references/unmentioned.md"), "hidden").unwrap();
        fs::write(bundle.join("assets/blob.bin"), [0xff, 0x00, 0x01]).unwrap();
        fs::write(bundle.join("scripts/run.sh"), "#!/bin/sh\necho ok\n").unwrap();
        let snapshot = service.snapshot().unwrap();

        let body = service.load(&snapshot, "resourceful", None).unwrap();
        assert_eq!(body["source"], "project");
        assert_eq!(body["resource_base"], "skill://resourceful/");
        assert_eq!(body["requires_execution"], false);

        let text = service
            .load(&snapshot, "resourceful", Some("references/guide.md"))
            .unwrap();
        assert_eq!(text["binary"], false);
        assert_eq!(text["content"], "guide text");
        let binary = service
            .load(&snapshot, "resourceful", Some("assets/blob.bin"))
            .unwrap();
        assert_eq!(binary["binary"], true);
        assert_eq!(binary["bytes"], 3);
        assert!(binary.get("content").is_none());
        assert!(
            service
                .load(&snapshot, "resourceful", Some("references/unmentioned.md"))
                .unwrap_err()
                .contains("not explicitly referenced")
        );
        assert!(
            service
                .load(&snapshot, "resourceful", Some("references/../SKILL.md"))
                .is_err()
        );
        assert!(
            service
                .load(&snapshot, "resourceful", Some("/etc/passwd"))
                .is_err()
        );

        let oversized = bundle.join("references/too-large.txt");
        fs::write(&oversized, vec![b'x'; MAX_RESOURCE_BYTES + 1]).unwrap();
        let mut current = fs::read_to_string(bundle.join("SKILL.md")).unwrap();
        current.push_str("\nAlso `references/too-large.txt`.\n");
        fs::write(bundle.join("SKILL.md"), &current).unwrap();
        assert!(
            service
                .load(&snapshot, "resourceful", Some("references/too-large.txt"))
                .unwrap_err()
                .contains("stale")
        );
        let refreshed = service.snapshot().unwrap();
        assert!(
            service
                .load(&refreshed, "resourceful", Some("references/too-large.txt"))
                .unwrap_err()
                .contains("exceeds")
        );

        fs::write(
            bundle.join("SKILL.md"),
            "---\nname: resourceful\ndescription: Loads declared resources only.\nrequires-execution: false\n---\nCHANGED",
        )
        .unwrap();
        assert!(
            service
                .load(&refreshed, "resourceful", None)
                .unwrap_err()
                .contains("stale")
        );
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_skill_and_resource_are_rejected_without_following() {
        use std::os::unix::fs::symlink;
        let (storage, project, service) =
            make_service("skills-symlink", SandboxModeSource::Classic);
        let root = project.join(".clat/skills");
        fs::create_dir_all(&root).unwrap();
        let outside = project.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("SKILL.md"),
            "---\nname: escaped\ndescription: escaped\n---\nbody",
        )
        .unwrap();
        symlink(&outside, root.join("escaped-link")).unwrap();

        let bundle = write_skill(
            &root,
            "safe",
            "safe",
            "safe resources",
            false,
            "Read `references/link.txt`.",
        );
        fs::create_dir_all(bundle.join("references")).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(
            outside.join("secret.txt"),
            bundle.join("references/link.txt"),
        )
        .unwrap();

        let snapshot = service.snapshot().unwrap();
        assert!(snapshot.entries.iter().all(|entry| entry.name != "escaped"));
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == "symlink")
        );
        let error = service
            .load(&snapshot, "safe", Some("references/link.txt"))
            .unwrap_err();
        assert!(
            error.contains("symlink"),
            "unexpected symlink error: {error}"
        );
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn empty_user_and_project_skill_roots_are_valid_and_leave_bundled_catalog_intact() {
        let (storage, project, service) =
            make_service("skills-empty-roots", SandboxModeSource::Classic);
        fs::create_dir_all(storage.join("skills")).unwrap();
        fs::create_dir_all(project.join(".clat/skills")).unwrap();
        let snapshot = service.snapshot().unwrap();
        assert_eq!(snapshot.entries.len(), BUNDLED_SKILLS.len());
        assert!(snapshot.diagnostics.is_empty());
        assert_eq!(entry(&snapshot, "code-review").source, SkillSource::Bundled);
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_executable_skill_is_unavailable_before_any_spawn_seam() {
        let (storage, project, service) =
            make_service("skills-unsupported-platform", SandboxModeSource::Classic);
        write_skill(
            &project.join(".clat/skills"),
            "needs-exec",
            "needs-exec",
            "Unsupported required sandbox fixture.",
            true,
            "Run `scripts/check.sh` using ordinary exec with sandbox=required and network=false.",
        );
        let snapshot = service.snapshot().unwrap();
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "needs-exec")
        );
        assert!(snapshot.diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == "unavailable" && diagnostic.name.as_deref() == Some("needs-exec")
        }));
        // SkillsService owns no ProcessService and execution_capability() only plans the
        // sandbox wrapper, so rejection here structurally precedes the unique spawn seam.
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }

    #[test]
    fn executable_skills_fail_closed_when_required_sandbox_conflicts_with_policy() {
        let mode = Arc::new(RwLock::new(PermissionMode::FullAccess));
        let (storage, project, service) = make_service(
            "skills-exec-gating",
            SandboxModeSource::Shared(Arc::clone(&mode)),
        );
        let root = project.join(".clat/skills");
        write_skill(
            &root,
            "needs-exec",
            "needs-exec",
            "Requires a graduated sandbox.",
            true,
            "Run `scripts/check.sh` using ordinary exec with sandbox=required and network=false.",
        );
        let snapshot = service.snapshot().unwrap();
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.name != "needs-exec")
        );
        assert!(snapshot.diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == "unavailable" && diagnostic.name.as_deref() == Some("needs-exec")
        }));

        *mode.write().unwrap() = PermissionMode::ProjectWrite;
        let snapshot = service.snapshot().unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(entry(&snapshot, "needs-exec").source, SkillSource::Project);
            assert!(
                snapshot
                    .rendered_catalog
                    .contains("ordinary exec with sandbox=required and network=false")
            );
        } else {
            assert!(
                snapshot
                    .entries
                    .iter()
                    .all(|entry| entry.name != "needs-exec")
            );
            assert!(snapshot.diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == "unavailable" && diagnostic.name.as_deref() == Some("needs-exec")
            }));
        }
        crate::test_support::cleanup_tree(storage.parent().unwrap());
    }
}
