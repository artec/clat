//! Transactional local plugin package store (phase 3).
//!
//! Immutable package trees are published before one atomic registry pointer
//! makes them active. Both WASM and MCP runtimes consume the same registry;
//! legacy user config remains an override at the adapter layer.

use super::{PluginCapabilities, PluginPackageManifest, PluginRuntimeKind};
use crate::control_storage::{json_file, sentinel};
use crate::session::root_lease::{StorageRootLease, try_acquire};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const STORE_DIR: &str = "plugin-store";
const REGISTRY_FILE: &str = "registry.json";
const MANIFEST_FILE: &str = "clat-plugin.json";
const ARTIFACTS_DIR: &str = "artifacts";
const STAGING_DIR: &str = "staging";
const PUBLISHER_FILE: &str = "clat-plugin.publisher.json";
const SIGNATURE_FILE: &str = "clat-plugin.minisig";
const REGISTRY_NAME: &str = "clat-plugin-registry";
const REGISTRY_VERSION: u64 = 1;
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_FILES: usize = 4_096;
const MAX_PACKAGE_DEPTH: usize = 32;
const MAX_PACKAGE_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PACKAGE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PLUGIN_CONFIG_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TrustLabel {
    LocalUnverified,
    PublisherVerified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallKind {
    Install,
    Update,
}

#[derive(Clone, Debug)]
pub(crate) struct PackageInspection {
    pub(crate) manifest: PluginPackageManifest,
    pub(crate) manifest_path: PathBuf,
    pub(crate) package_root: PathBuf,
    pub(crate) tree_sha256: String,
    pub(crate) files: usize,
    pub(crate) total_bytes: u64,
    pub(crate) capabilities: Vec<String>,
    pub(crate) trust: TrustLabel,
    pub(crate) publisher: Option<PublisherIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PublisherIdentity {
    pub(crate) publisher: String,
    pub(crate) public_key: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PackageMutation {
    pub(crate) id: String,
    pub(crate) version: String,
    pub(crate) runtime: PluginRuntimeKind,
    pub(crate) tree_sha256: String,
    pub(crate) enabled: bool,
    pub(crate) rollback_version: Option<String>,
    pub(crate) note: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PackageListEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) runtime: PluginRuntimeKind,
    pub(crate) tree_sha256: String,
    pub(crate) enabled: bool,
    pub(crate) rollback_version: Option<String>,
    pub(crate) trust: TrustLabel,
    pub(crate) publisher: Option<String>,
    pub(crate) health: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivePackage {
    pub(crate) id: String,
    pub(crate) manifest: PluginPackageManifest,
    pub(crate) manifest_path: PathBuf,
    pub(crate) config: Option<Value>,
    pub(crate) trust: TrustLabel,
    pub(crate) publisher: Option<PublisherIdentity>,
    pub(crate) tree_sha256: String,
}

#[derive(Debug)]
pub(crate) struct ActivePackages {
    pub(crate) packages: Vec<ActivePackage>,
    pub(crate) failures: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryUnit {
    name: String,
    version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageRegistry {
    unit: RegistryUnit,
    #[serde(default)]
    packages: BTreeMap<String, InstalledPlugin>,
}

impl Default for PackageRegistry {
    fn default() -> Self {
        Self {
            unit: RegistryUnit {
                name: REGISTRY_NAME.into(),
                version: REGISTRY_VERSION,
            },
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstalledPlugin {
    enabled: bool,
    active: Activation,
    #[serde(default)]
    rollback: Option<Activation>,
    artifacts: BTreeMap<String, ArtifactRecord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Activation {
    artifact: String,
    #[serde(default)]
    config: Option<Value>,
    accepted_capabilities: PluginCapabilities,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactRecord {
    tree_sha256: String,
    manifest: PluginPackageManifest,
    trust: TrustLabel,
    #[serde(default)]
    publisher: Option<PublisherIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublisherRecord {
    schema_version: u32,
    publisher: String,
    public_key: String,
}

struct FilePlan {
    relative: String,
    source: PathBuf,
    bytes: u64,
    sha256: String,
    mode: u32,
}

struct TreePlan {
    source_root: PathBuf,
    files: Vec<FilePlan>,
    total_bytes: u64,
    sha256: String,
}

/// Mutable package-store handle. Holding it means holding the same
/// storage-root lease as a TrustedProjectApplication for the entire mutation.
pub(crate) struct PackageStore {
    store_root: PathBuf,
    registry: PackageRegistry,
    _leases: Vec<StorageRootLease>,
    #[cfg(test)]
    fault_before_registry_publish: bool,
}

impl PackageStore {
    pub(crate) fn open(storage_root: &Path) -> Result<Self, String> {
        reject_final_symlink(storage_root, "storage root")?;
        let existed = storage_root.is_dir();
        if storage_root.exists() && !existed {
            return Err(format!(
                "storage root is not a directory: {}",
                storage_root.display()
            ));
        }
        let first = try_acquire(storage_root)
            .map_err(|error| format!("acquire storage-root lease: {error}"))?
            .ok_or_else(|| {
                "CLAT storage is busy; close the running CLAT process and retry".to_owned()
            })?;
        if !existed {
            create_private_dir(storage_root)?;
        }
        // A newly-created root is a deeper lock target than the ancestor held
        // by `first`; acquire it while retaining the ancestor to close the
        // creation window for cooperating CLAT processes.
        let mut leases = vec![first];
        if !existed {
            let root_lease = try_acquire(storage_root)
                .map_err(|error| format!("escalate storage-root lease: {error}"))?
                .ok_or_else(|| "CLAT storage became busy while initializing".to_owned())?;
            leases.push(root_lease);
        }
        let storage_root = storage_root
            .canonicalize()
            .map_err(|error| format!("canonicalize storage root: {error}"))?;
        let store_root = storage_root.join(STORE_DIR);
        ensure_store_layout(&store_root)?;
        cleanup_staging(&store_root.join(STAGING_DIR))?;
        let registry = load_registry(&store_root)?;
        validate_registry_shape(&registry)?;
        cleanup_orphan_artifacts(&store_root, &registry)?;
        Ok(Self {
            store_root,
            registry,
            _leases: leases,
            #[cfg(test)]
            fault_before_registry_publish: false,
        })
    }

    pub(crate) fn inspect(path: &Path) -> Result<PackageInspection, String> {
        inspect_source(path)
    }

    pub(crate) fn install(
        &mut self,
        path: &Path,
        config: Option<Value>,
        accept_capabilities: bool,
        kind: InstallKind,
    ) -> Result<PackageMutation, String> {
        let inspection = inspect_source(path)?;
        if let Some(config) = &config {
            let bytes = serde_json::to_vec(config)
                .map_err(|error| format!("serialize plugin config: {error}"))?;
            if bytes.len() > MAX_PLUGIN_CONFIG_BYTES {
                return Err(format!(
                    "plugin config is {} bytes; the cap is {MAX_PLUGIN_CONFIG_BYTES}",
                    bytes.len()
                ));
            }
        }
        inspection.manifest.validate_config(config.as_ref())?;
        let id = inspection.manifest.id.clone();
        let existing = self.registry.packages.get(&id);
        match (kind, existing.is_some()) {
            (InstallKind::Install, true) => {
                return Err(format!(
                    "plugin `{id}` is already installed; use `clat plugin update`"
                ));
            }
            (InstallKind::Update, false) => {
                return Err(format!(
                    "plugin `{id}` is not installed; use `clat plugin install`"
                ));
            }
            _ => {}
        }
        let old_capabilities = existing.map(|plugin| {
            &plugin
                .artifacts
                .get(&plugin.active.artifact)
                .expect("validated registry active artifact")
                .manifest
                .capabilities
        });
        if let Some(plugin) = existing {
            let active = plugin
                .artifacts
                .get(&plugin.active.artifact)
                .expect("validated registry active artifact");
            verify_artifact_record(&self.store_root, &id, active)?;
            if active.publisher != inspection.publisher {
                return Err(format!(
                    "plugin `{id}` publisher identity changed; uninstall it before accepting a different publisher key"
                ));
            }
        }
        let expansion = capability_expansion(old_capabilities, &inspection.manifest.capabilities);
        if !expansion.is_empty() && !accept_capabilities {
            return Err(format!(
                "plugin `{id}` requests new capabilities: {}; inspect them and retry with \
                 `--accept-capabilities`",
                expansion.join(", ")
            ));
        }

        let staging = self
            .store_root
            .join(STAGING_DIR)
            .join(uuid::Uuid::new_v4().to_string());
        let result = (|| {
            create_private_dir(&staging)?;
            let plan = scan_tree(&inspection.package_root)?;
            if plan.sha256 != inspection.tree_sha256 {
                return Err("package changed between inspection and staging".into());
            }
            copy_tree(&plan, &staging)?;
            let staged = inspect_source(&staging)?;
            if staged.tree_sha256 != inspection.tree_sha256
                || staged.manifest != inspection.manifest
            {
                return Err("staged package verification did not match the source".into());
            }
            let artifact_root = self
                .store_root
                .join(ARTIFACTS_DIR)
                .join(&id)
                .join(&inspection.tree_sha256);
            publish_artifact(&staging, &artifact_root, &staged)?;

            #[cfg(test)]
            if self.fault_before_registry_publish {
                return Err("injected failure before registry publication".into());
            }

            let artifact = ArtifactRecord {
                tree_sha256: inspection.tree_sha256.clone(),
                manifest: inspection.manifest.clone(),
                trust: inspection.trust,
                publisher: inspection.publisher.clone(),
            };
            let activation = Activation {
                artifact: inspection.tree_sha256.clone(),
                config,
                accepted_capabilities: inspection.manifest.capabilities.clone(),
            };
            let mut next = self.registry.clone();
            let rollback_version;
            let enabled;
            match next.packages.get_mut(&id) {
                Some(plugin) => {
                    enabled = plugin.enabled;
                    plugin
                        .artifacts
                        .insert(artifact.tree_sha256.clone(), artifact);
                    if plugin.active != activation {
                        plugin.rollback = Some(plugin.active.clone());
                        plugin.active = activation;
                    }
                    let active_key = plugin.active.artifact.clone();
                    let rollback_key = plugin
                        .rollback
                        .as_ref()
                        .map(|rollback| rollback.artifact.clone());
                    plugin.artifacts.retain(|digest, _| {
                        digest == &active_key || rollback_key.as_ref() == Some(digest)
                    });
                    rollback_version = plugin.rollback.as_ref().and_then(|rollback| {
                        plugin
                            .artifacts
                            .get(&rollback.artifact)
                            .map(|artifact| artifact.manifest.version.clone())
                    });
                }
                None => {
                    enabled = true;
                    next.packages.insert(
                        id.clone(),
                        InstalledPlugin {
                            enabled,
                            active: activation,
                            rollback: None,
                            artifacts: BTreeMap::from([(artifact.tree_sha256.clone(), artifact)]),
                        },
                    );
                    rollback_version = None;
                }
            }
            write_registry(&self.store_root, &next)?;
            self.registry = next;
            Ok(PackageMutation {
                id: id.clone(),
                version: inspection.manifest.version.clone(),
                runtime: inspection.manifest.runtime.kind,
                tree_sha256: inspection.tree_sha256.clone(),
                enabled,
                rollback_version,
                note: match kind {
                    InstallKind::Install => "installed and activated".into(),
                    InstallKind::Update => "updated and activated".into(),
                },
            })
        })();
        if staging.exists() {
            let _ = remove_tree(&staging);
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn list(&self) -> Vec<PackageListEntry> {
        registry_list(&self.registry)
    }

    pub(crate) fn set_enabled(
        &mut self,
        id: &str,
        enabled: bool,
    ) -> Result<PackageMutation, String> {
        let mut next = self.registry.clone();
        let plugin = next
            .packages
            .get_mut(id)
            .ok_or_else(|| format!("plugin `{id}` is not installed"))?;
        if enabled {
            let artifact = plugin
                .artifacts
                .get(&plugin.active.artifact)
                .expect("registry shape validates active artifact");
            verify_artifact_record(&self.store_root, id, artifact)?;
        }
        plugin.enabled = enabled;
        let mutation = mutation_for(id, plugin, if enabled { "enabled" } else { "disabled" })?;
        write_registry(&self.store_root, &next)?;
        self.registry = next;
        Ok(mutation)
    }

    pub(crate) fn rollback(&mut self, id: &str) -> Result<PackageMutation, String> {
        let mut next = self.registry.clone();
        let plugin = next
            .packages
            .get_mut(id)
            .ok_or_else(|| format!("plugin `{id}` is not installed"))?;
        let rollback = plugin
            .rollback
            .take()
            .ok_or_else(|| format!("plugin `{id}` has no rollback version"))?;
        let rollback_artifact = plugin
            .artifacts
            .get(&rollback.artifact)
            .expect("registry shape validates rollback artifact");
        verify_artifact_record(&self.store_root, id, rollback_artifact)?;
        let previous_active = std::mem::replace(&mut plugin.active, rollback);
        plugin.rollback = Some(previous_active);
        let mutation = mutation_for(id, plugin, "rolled back")?;
        write_registry(&self.store_root, &next)?;
        self.registry = next;
        Ok(mutation)
    }

    pub(crate) fn uninstall(&mut self, id: &str) -> Result<PackageMutation, String> {
        let mut next = self.registry.clone();
        let plugin = next
            .packages
            .remove(id)
            .ok_or_else(|| format!("plugin `{id}` is not installed"))?;
        let active = plugin
            .artifacts
            .get(&plugin.active.artifact)
            .ok_or_else(|| format!("plugin `{id}` registry is inconsistent"))?;
        let mut mutation = PackageMutation {
            id: id.to_owned(),
            version: active.manifest.version.clone(),
            runtime: active.manifest.runtime.kind,
            tree_sha256: active.tree_sha256.clone(),
            enabled: false,
            rollback_version: None,
            note: "uninstalled".into(),
        };
        // Pointer-first: after this commit no runtime reader can reach the
        // package, even if byte cleanup fails or the process crashes.
        write_registry(&self.store_root, &next)?;
        self.registry = next;
        let artifact_dir = self.store_root.join(ARTIFACTS_DIR).join(id);
        if artifact_dir.exists()
            && let Err(error) = remove_tree(&artifact_dir)
        {
            mutation.note = format!("uninstalled; inert artifact cleanup needs retry: {error}");
        }
        Ok(mutation)
    }

    #[cfg(test)]
    fn inject_failure_before_registry_publish(&mut self) {
        self.fault_before_registry_publish = true;
    }
}

/// Read-only runtime projection. Missing store/registry means no installed
/// packages; malformed or tampered state fails closed and prevents mounting.
#[cfg(test)]
pub(crate) fn active_packages(storage_root: &Path) -> Result<ActivePackages, String> {
    let mut result = active_packages_for_runtime(storage_root, PluginRuntimeKind::WasmComponent)?;
    let mcp = active_packages_for_runtime(storage_root, PluginRuntimeKind::McpStdio)?;
    result.packages.extend(mcp.packages);
    result.failures.extend(mcp.failures);
    result
        .packages
        .sort_by(|left, right| left.id.cmp(&right.id));
    result.failures.sort();
    Ok(result)
}

#[cfg(test)]
pub(crate) fn active_packages_for_runtime(
    storage_root: &Path,
    runtime: PluginRuntimeKind,
) -> Result<ActivePackages, String> {
    active_packages_for_runtime_excluding(storage_root, runtime, &BTreeSet::new())
}

pub(crate) fn active_packages_for_runtime_excluding(
    storage_root: &Path,
    runtime: PluginRuntimeKind,
    excluded_ids: &BTreeSet<String>,
) -> Result<ActivePackages, String> {
    reject_final_symlink(storage_root, "storage root")?;
    let store_root = storage_root.join(STORE_DIR);
    if !store_root.exists() {
        return Ok(ActivePackages {
            packages: Vec::new(),
            failures: Vec::new(),
        });
    }
    reject_final_symlink(&store_root, "plugin store")?;
    let registry = load_registry(&store_root)?;
    validate_registry_shape(&registry)?;
    let mut active = Vec::new();
    let mut failures = Vec::new();
    for (id, plugin) in registry.packages {
        if excluded_ids.contains(&id) {
            continue;
        }
        if !plugin.enabled {
            continue;
        }
        let artifact = plugin
            .artifacts
            .get(&plugin.active.artifact)
            .expect("registry shape validates active artifact");
        if artifact.manifest.runtime.kind != runtime {
            continue;
        }
        let loaded = (|| -> Result<ActivePackage, String> {
            verify_artifact_record(&store_root, &id, artifact)?;
            let manifest_path = artifact_manifest_path(&store_root, &id, &artifact.tree_sha256);
            artifact
                .manifest
                .validate_config(plugin.active.config.as_ref())?;
            Ok(ActivePackage {
                id: id.clone(),
                manifest: artifact.manifest.clone(),
                manifest_path,
                config: plugin.active.config.clone(),
                trust: artifact.trust,
                publisher: artifact.publisher.clone(),
                tree_sha256: artifact.tree_sha256.clone(),
            })
        })();
        match loaded {
            Ok(package) => active.push(package),
            Err(error) => failures.push(format!("installed plugin `{id}`: {error}")),
        }
    }
    active.sort_by(|left, right| left.id.cmp(&right.id));
    failures.sort();
    Ok(ActivePackages {
        packages: active,
        failures,
    })
}

pub(crate) fn installed_packages(storage_root: &Path) -> Result<Vec<PackageListEntry>, String> {
    reject_final_symlink(storage_root, "storage root")?;
    let store_root = storage_root.join(STORE_DIR);
    if !store_root.exists() {
        return Ok(Vec::new());
    }
    reject_final_symlink(&store_root, "plugin store")?;
    let registry = load_registry(&store_root)?;
    validate_registry_shape(&registry)?;
    let mut entries = registry_list(&registry);
    for entry in &mut entries {
        let plugin = registry
            .packages
            .get(&entry.id)
            .expect("list entry comes from registry");
        let artifact = plugin
            .artifacts
            .get(&plugin.active.artifact)
            .expect("registry shape validates active artifact");
        if let Err(error) = verify_artifact_record(&store_root, &entry.id, artifact) {
            entry.health = Some(error);
        }
    }
    Ok(entries)
}

fn registry_list(registry: &PackageRegistry) -> Vec<PackageListEntry> {
    registry
        .packages
        .iter()
        .filter_map(|(id, plugin)| {
            let active = plugin.artifacts.get(&plugin.active.artifact)?;
            let rollback_version = plugin.rollback.as_ref().and_then(|rollback| {
                plugin
                    .artifacts
                    .get(&rollback.artifact)
                    .map(|artifact| artifact.manifest.version.clone())
            });
            Some(PackageListEntry {
                id: id.clone(),
                name: active.manifest.name.clone(),
                version: active.manifest.version.clone(),
                runtime: active.manifest.runtime.kind,
                tree_sha256: active.tree_sha256.clone(),
                enabled: plugin.enabled,
                rollback_version,
                trust: active.trust,
                publisher: active
                    .publisher
                    .as_ref()
                    .map(|publisher| publisher.publisher.clone()),
                health: None,
            })
        })
        .collect()
}

fn mutation_for(id: &str, plugin: &InstalledPlugin, note: &str) -> Result<PackageMutation, String> {
    let active = plugin
        .artifacts
        .get(&plugin.active.artifact)
        .ok_or_else(|| format!("plugin `{id}` active artifact is missing"))?;
    let rollback_version = plugin.rollback.as_ref().and_then(|rollback| {
        plugin
            .artifacts
            .get(&rollback.artifact)
            .map(|artifact| artifact.manifest.version.clone())
    });
    Ok(PackageMutation {
        id: id.into(),
        version: active.manifest.version.clone(),
        runtime: active.manifest.runtime.kind,
        tree_sha256: active.tree_sha256.clone(),
        enabled: plugin.enabled,
        rollback_version,
        note: note.into(),
    })
}

fn inspect_source(path: &Path) -> Result<PackageInspection, String> {
    reject_final_symlink(path, "package root/manifest")?;
    let manifest_path = if path.is_dir() {
        path.join(MANIFEST_FILE)
    } else {
        path.to_owned()
    };
    if manifest_path.file_name().and_then(|name| name.to_str()) != Some(MANIFEST_FILE) {
        return Err(format!("package manifest must be named `{MANIFEST_FILE}`"));
    }
    reject_final_symlink(&manifest_path, "package manifest")?;
    let manifest = PluginPackageManifest::load(&manifest_path)?;
    let entry = manifest.verify_entry_digest(&manifest_path)?;
    #[cfg(unix)]
    if manifest.runtime.kind == PluginRuntimeKind::McpStdio {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&entry)
            .map_err(|error| format!("inspect MCP entry {}: {error}", entry.display()))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(format!(
                "mcp-stdio runtime entry is not executable: {}",
                entry.display()
            ));
        }
    }
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| "package manifest has no parent directory".to_owned())?
        .canonicalize()
        .map_err(|error| format!("canonicalize package root: {error}"))?;
    let manifest_path = package_root.join(MANIFEST_FILE);
    let tree = scan_tree(&package_root)?;
    let (trust, publisher) = inspect_trust(&package_root, &manifest, &tree)?;
    Ok(PackageInspection {
        capabilities: capability_labels(&manifest.capabilities),
        manifest,
        manifest_path,
        package_root,
        tree_sha256: tree.sha256,
        files: tree.files.len(),
        total_bytes: tree.total_bytes,
        trust,
        publisher,
    })
}

fn capability_labels(capabilities: &PluginCapabilities) -> Vec<String> {
    let mut labels = Vec::new();
    for (enabled, name) in [
        (capabilities.tools, "tools"),
        (capabilities.prompts, "prompts"),
        (capabilities.sampling, "sampling"),
        (capabilities.elicitation, "elicitation"),
        (capabilities.host_context, "hostContext"),
    ] {
        if enabled {
            labels.push(name.to_owned());
        }
    }
    labels.extend(
        capabilities
            .host_tools
            .iter()
            .map(|name| format!("hostTools.{name}")),
    );
    labels
}

fn capability_expansion(old: Option<&PluginCapabilities>, new: &PluginCapabilities) -> Vec<String> {
    let old: BTreeSet<String> = old
        .map(capability_labels)
        .unwrap_or_default()
        .into_iter()
        .collect();
    capability_labels(new)
        .into_iter()
        .filter(|capability| !old.contains(capability))
        .collect()
}

fn ensure_store_layout(store_root: &Path) -> Result<(), String> {
    reject_final_symlink(store_root, "plugin store")?;
    create_private_dir(store_root)?;
    for name in [ARTIFACTS_DIR, STAGING_DIR] {
        let path = store_root.join(name);
        reject_final_symlink(&path, name)?;
        create_private_dir(&path)?;
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("create directory {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("chmod directory {}: {error}", path.display()))?;
    }
    Ok(())
}

fn load_registry(store_root: &Path) -> Result<PackageRegistry, String> {
    let path = store_root.join(REGISTRY_FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PackageRegistry::default());
        }
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("{REGISTRY_FILE} must not be a symbolic link"));
        }
        Ok(metadata) if metadata.len() > MAX_REGISTRY_BYTES => {
            return Err(format!(
                "{REGISTRY_FILE} is {} bytes; the cap is {MAX_REGISTRY_BYTES}",
                metadata.len()
            ));
        }
        Ok(_) => {}
    }
    let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let registry: PackageRegistry = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    validate_registry_shape(&registry)?;
    Ok(registry)
}

fn validate_registry_shape(registry: &PackageRegistry) -> Result<(), String> {
    if registry.unit
        != (RegistryUnit {
            name: REGISTRY_NAME.into(),
            version: REGISTRY_VERSION,
        })
    {
        return Err(format!(
            "unsupported plugin registry unit {} v{}; expected {REGISTRY_NAME} v{REGISTRY_VERSION}",
            registry.unit.name, registry.unit.version
        ));
    }
    for (id, plugin) in &registry.packages {
        if plugin.artifacts.is_empty() {
            return Err(format!("plugin `{id}` has no artifacts"));
        }
        for (digest, artifact) in &plugin.artifacts {
            if !valid_digest(digest) || digest != &artifact.tree_sha256 {
                return Err(format!("plugin `{id}` has an invalid artifact digest"));
            }
            artifact.manifest.validate()?;
            if &artifact.manifest.id != id {
                return Err(format!(
                    "registry key `{id}` does not match artifact manifest id `{}`",
                    artifact.manifest.id
                ));
            }
        }
        for (label, activation) in [
            ("active", Some(&plugin.active)),
            ("rollback", plugin.rollback.as_ref()),
        ] {
            let Some(activation) = activation else {
                continue;
            };
            let artifact = plugin.artifacts.get(&activation.artifact).ok_or_else(|| {
                format!("plugin `{id}` {label} pointer references a missing artifact")
            })?;
            if activation.accepted_capabilities != artifact.manifest.capabilities {
                return Err(format!(
                    "plugin `{id}` {label} capability approval does not match its manifest"
                ));
            }
            match (artifact.trust, artifact.publisher.as_ref()) {
                (TrustLabel::LocalUnverified, None) | (TrustLabel::PublisherVerified, Some(_)) => {}
                _ => {
                    return Err(format!(
                        "plugin `{id}` {label} trust label and publisher identity disagree"
                    ));
                }
            }
            if activation.config.as_ref().is_some_and(|config| {
                serde_json::to_vec(config)
                    .map(|bytes| bytes.len() > MAX_PLUGIN_CONFIG_BYTES)
                    .unwrap_or(true)
            }) {
                return Err(format!(
                    "plugin `{id}` {label} config exceeds {MAX_PLUGIN_CONFIG_BYTES} bytes"
                ));
            }
            artifact
                .manifest
                .validate_config(activation.config.as_ref())?;
        }
    }
    Ok(())
}

fn verify_artifact_record(
    store_root: &Path,
    id: &str,
    artifact: &ArtifactRecord,
) -> Result<PackageInspection, String> {
    let artifact_root = validate_artifact_location(store_root, id, &artifact.tree_sha256)?;
    let inspected = inspect_source(&artifact_root)?;
    if inspected.manifest != artifact.manifest
        || inspected.tree_sha256 != artifact.tree_sha256
        || inspected.trust != artifact.trust
        || inspected.publisher != artifact.publisher
    {
        return Err(format!(
            "installed package `{id}` activation verification did not match the registry"
        ));
    }
    Ok(inspected)
}

fn write_registry(store_root: &Path, registry: &PackageRegistry) -> Result<(), String> {
    validate_registry_shape(registry)?;
    let dir = cap_std::fs::Dir::open_ambient_dir(store_root, cap_std::ambient_authority())
        .map_err(|error| format!("open plugin store: {error}"))?;
    json_file::write(&dir, store_root, REGISTRY_FILE, registry)
}

fn artifact_manifest_path(store_root: &Path, id: &str, digest: &str) -> PathBuf {
    artifact_root(store_root, id, digest).join(MANIFEST_FILE)
}

fn artifact_root(store_root: &Path, id: &str, digest: &str) -> PathBuf {
    store_root.join(ARTIFACTS_DIR).join(id).join(digest)
}

fn validate_artifact_location(
    store_root: &Path,
    id: &str,
    digest: &str,
) -> Result<PathBuf, String> {
    let artifacts = store_root.join(ARTIFACTS_DIR);
    let id_root = artifacts.join(id);
    let artifact = id_root.join(digest);
    reject_final_symlink(&id_root, "plugin artifact id directory")?;
    reject_final_symlink(&artifact, "plugin artifact directory")?;
    let canonical_base = artifacts
        .canonicalize()
        .map_err(|error| format!("canonicalize artifact store: {error}"))?;
    let canonical_artifact = artifact
        .canonicalize()
        .map_err(|error| format!("canonicalize plugin artifact: {error}"))?;
    if !canonical_artifact.starts_with(&canonical_base) {
        return Err(format!("plugin artifact `{id}` escapes the package store"));
    }
    Ok(canonical_artifact)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn scan_tree(root: &Path) -> Result<TreePlan, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("canonicalize package root: {error}"))?;
    let mut files = Vec::new();
    let mut total = 0u64;
    collect_files(&root, &root, 0, &mut files, &mut total)?;
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    let sha256 = tree_digest(&files, None);
    Ok(TreePlan {
        source_root: root,
        files,
        total_bytes: total,
        sha256,
    })
}

fn tree_digest(files: &[FilePlan], excluded_root_file: Option<&str>) -> String {
    let mut tree = Sha256::new();
    tree.update(b"clat-package-tree-v1\0");
    for file in files {
        if excluded_root_file.is_some_and(|excluded| file.relative == excluded) {
            continue;
        }
        tree.update((file.relative.len() as u64).to_be_bytes());
        tree.update(file.relative.as_bytes());
        tree.update(file.bytes.to_be_bytes());
        tree.update(file.sha256.as_bytes());
    }
    tree.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn inspect_trust(
    package_root: &Path,
    manifest: &PluginPackageManifest,
    tree: &TreePlan,
) -> Result<(TrustLabel, Option<PublisherIdentity>), String> {
    let publisher_path = package_root.join(PUBLISHER_FILE);
    let signature_path = package_root.join(SIGNATURE_FILE);
    let publisher_present = publisher_path.exists();
    let signature_present = signature_path.exists();
    if !publisher_present && !signature_present {
        return Ok((TrustLabel::LocalUnverified, None));
    }
    if publisher_present != signature_present {
        return Err(format!(
            "signed packages require both `{PUBLISHER_FILE}` and `{SIGNATURE_FILE}`"
        ));
    }
    reject_final_symlink(&publisher_path, "publisher record")?;
    reject_final_symlink(&signature_path, "package signature")?;
    let publisher_bytes = read_small_file(&publisher_path, 16 * 1024)?;
    let publisher: PublisherRecord = serde_json::from_slice(&publisher_bytes)
        .map_err(|error| format!("parse {PUBLISHER_FILE}: {error}"))?;
    if publisher.schema_version != 1
        || publisher.publisher.is_empty()
        || publisher.publisher.len() > 128
        || !publisher.publisher.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err("publisher record carries an invalid publisher id".into());
    }
    if publisher.public_key.len() > 1024 {
        return Err("publisher public key is too large".into());
    }
    let message = signature_message(manifest, &publisher, tree);
    let signature_bytes = read_small_file(&signature_path, 16 * 1024)?;
    let signature_text = std::str::from_utf8(&signature_bytes)
        .map_err(|_| "package signature is not UTF-8".to_owned())?;
    let signature = minisign_verify::Signature::decode(signature_text)
        .map_err(|error| format!("decode package signature: {error}"))?;
    let public_key = minisign_verify::PublicKey::from_base64(publisher.public_key.trim())
        .map_err(|error| format!("decode publisher public key: {error}"))?;
    public_key
        .verify(message.as_bytes(), &signature, false)
        .map_err(|error| format!("verify package signature: {error}"))?;
    Ok((
        TrustLabel::PublisherVerified,
        Some(PublisherIdentity {
            publisher: publisher.publisher,
            public_key: publisher.public_key,
        }),
    ))
}

fn signature_message(
    manifest: &PluginPackageManifest,
    publisher: &PublisherRecord,
    tree: &TreePlan,
) -> String {
    let content_digest = tree_digest(&tree.files, Some(SIGNATURE_FILE));
    format!(
        "clat-plugin-signature-v1\npublisher:{}\npublicKey:{}\nid:{}\nversion:{}\ncontentSha256:{}\n",
        publisher.publisher,
        publisher.public_key.trim(),
        manifest.id,
        manifest.version,
        content_digest,
    )
}

fn read_small_file(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if bytes.len() > cap {
        return Err(format!("{} exceeds {cap} bytes", path.display()));
    }
    Ok(bytes)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<FilePlan>,
    total: &mut u64,
) -> Result<(), String> {
    if depth > MAX_PACKAGE_DEPTH {
        return Err(format!(
            "package directory depth exceeds {MAX_PACKAGE_DEPTH}"
        ));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("read package directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read package directory entry: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect package path {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "package paths must not be symbolic links: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, depth + 1, files, total)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(format!(
                "package contains a non-regular file: {}",
                path.display()
            ));
        }
        if files.len() >= MAX_PACKAGE_FILES {
            return Err(format!(
                "package contains more than {MAX_PACKAGE_FILES} files"
            ));
        }
        if metadata.len() > MAX_PACKAGE_FILE_BYTES {
            return Err(format!(
                "package file {} is {} bytes; the cap is {MAX_PACKAGE_FILE_BYTES}",
                path.display(),
                metadata.len()
            ));
        }
        *total = total.saturating_add(metadata.len());
        if *total > MAX_PACKAGE_TOTAL_BYTES {
            return Err(format!(
                "package exceeds the {MAX_PACKAGE_TOTAL_BYTES} byte total cap"
            ));
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| "package path escaped its root".to_owned())?;
        let relative = relative_path
            .components()
            .map(|component| {
                component.as_os_str().to_str().ok_or_else(|| {
                    format!("package path is not UTF-8: {}", relative_path.display())
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let sha256 = hash_package_file(root, relative_path, metadata.len())?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let mode = 0;
        files.push(FilePlan {
            relative,
            source: path,
            bytes: metadata.len(),
            sha256,
            mode,
        });
    }
    Ok(())
}

#[cfg(test)]
fn hash_file_bounded(path: &Path, expected: u64) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("open package file {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read package file {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        read = read.saturating_add(count as u64);
        if read > expected || read > MAX_PACKAGE_FILE_BYTES {
            return Err(format!(
                "package file changed or exceeded its cap while reading: {}",
                path.display()
            ));
        }
        digest.update(&buffer[..count]);
    }
    if read != expected {
        return Err(format!(
            "package file changed size while reading: {}",
            path.display()
        ));
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn open_package_file(root: &Path, relative: &Path) -> Result<cap_std::fs::File, String> {
    let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|error| format!("open package root {}: {error}", root.display()))?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let metadata = dir
        .symlink_metadata(relative)
        .map_err(|error| format!("inspect package file {}: {error}", relative.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "package path is not a regular no-follow file: {}",
            relative.display()
        ));
    }
    dir.open_with(relative, &options)
        .map_err(|error| format!("open package file {}: {error}", relative.display()))
}

fn hash_package_file(root: &Path, relative: &Path, expected: u64) -> Result<String, String> {
    let mut file = open_package_file(root, relative)?;
    hash_reader_bounded(&mut file, relative, expected)
}

fn hash_reader_bounded(
    file: &mut impl Read,
    label: &Path,
    expected: u64,
) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read package file {}: {error}", label.display()))?;
        if count == 0 {
            break;
        }
        read = read.saturating_add(count as u64);
        if read > expected || read > MAX_PACKAGE_FILE_BYTES {
            return Err(format!(
                "package file changed or exceeded its cap while reading: {}",
                label.display()
            ));
        }
        digest.update(&buffer[..count]);
    }
    if read != expected {
        return Err(format!(
            "package file changed size while reading: {}",
            label.display()
        ));
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn copy_tree(plan: &TreePlan, destination: &Path) -> Result<(), String> {
    for file in &plan.files {
        let destination_file = destination.join(Path::new(&file.relative));
        let parent = destination_file
            .parent()
            .ok_or_else(|| "package destination has no parent".to_owned())?;
        create_private_dir(parent)?;
        let relative = Path::new(&file.relative);
        let mut source = open_package_file(&plan.source_root, relative)?;
        let current = source
            .metadata()
            .map_err(|error| format!("reinspect {}: {error}", file.source.display()))?;
        if !current.is_file() || current.len() != file.bytes {
            return Err(format!(
                "package file changed before copy: {}",
                file.source.display()
            ));
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut target = options
            .open(&destination_file)
            .map_err(|error| format!("create {}: {error}", destination_file.display()))?;
        let mut digest = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = source
                .read(&mut buffer)
                .map_err(|error| format!("read {}: {error}", file.source.display()))?;
            if count == 0 {
                break;
            }
            copied = copied.saturating_add(count as u64);
            if copied > file.bytes {
                return Err(format!(
                    "package file grew during copy: {}",
                    file.source.display()
                ));
            }
            digest.update(&buffer[..count]);
            target
                .write_all(&buffer[..count])
                .map_err(|error| format!("write {}: {error}", destination_file.display()))?;
        }
        let actual: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if copied != file.bytes || actual != file.sha256 {
            return Err(format!(
                "package file changed during copy: {}",
                file.source.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                &destination_file,
                fs::Permissions::from_mode(0o600 | (file.mode & 0o111)),
            )
            .map_err(|error| format!("chmod {}: {error}", destination_file.display()))?;
        }
        target
            .sync_all()
            .map_err(|error| format!("fsync {}: {error}", destination_file.display()))?;
    }
    sync_tree_directories(destination)?;
    Ok(())
}

fn sync_tree_directories(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_owned()];
    let mut index = 0;
    while index < directories.len() {
        let current = directories[index].clone();
        index += 1;
        for entry in fs::read_dir(&current)
            .map_err(|error| format!("read {}: {error}", current.display()))?
        {
            let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
            if entry
                .file_type()
                .map_err(|error| format!("inspect directory entry: {error}"))?
                .is_dir()
            {
                directories.push(entry.path());
            }
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sentinel::sync_dir(&directory)
            .map_err(|error| format!("fsync {}: {error}", directory.display()))?;
    }
    Ok(())
}

fn publish_artifact(
    staging: &Path,
    destination: &Path,
    staged: &PackageInspection,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "artifact destination has no parent".to_owned())?;
    reject_final_symlink(parent, "plugin artifact id directory")?;
    create_private_dir(parent)?;
    reject_final_symlink(destination, "plugin artifact directory")?;
    if destination.exists() {
        let existing = inspect_source(destination)?;
        if existing.tree_sha256 != staged.tree_sha256 || existing.manifest != staged.manifest {
            return Err(format!(
                "artifact destination {} exists with different content",
                destination.display()
            ));
        }
        return Ok(());
    }
    fs::rename(staging, destination).map_err(|error| {
        format!(
            "publish artifact {} -> {}: {error}",
            staging.display(),
            destination.display()
        )
    })?;
    sentinel::sync_dir(parent)
        .map_err(|error| format!("fsync artifact parent {}: {error}", parent.display()))
}

fn cleanup_staging(staging: &Path) -> Result<(), String> {
    if !staging.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(staging)
        .map_err(|error| format!("read staging directory {}: {error}", staging.display()))?
    {
        let path = entry
            .map_err(|error| format!("read staging entry: {error}"))?
            .path();
        remove_tree(&path)?;
    }
    Ok(())
}

fn cleanup_orphan_artifacts(store_root: &Path, registry: &PackageRegistry) -> Result<(), String> {
    let artifacts = store_root.join(ARTIFACTS_DIR);
    for id_entry in fs::read_dir(&artifacts)
        .map_err(|error| format!("read artifact directory {}: {error}", artifacts.display()))?
    {
        let id_entry = id_entry.map_err(|error| format!("read artifact id entry: {error}"))?;
        let id = id_entry
            .file_name()
            .to_str()
            .ok_or_else(|| "artifact id directory is not UTF-8".to_owned())?
            .to_owned();
        let id_path = id_entry.path();
        let metadata = fs::symlink_metadata(&id_path)
            .map_err(|error| format!("inspect artifact id {}: {error}", id_path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "unexpected artifact id path: {}",
                id_path.display()
            ));
        }
        let valid_id = registry.packages.get(&id);
        for digest_entry in fs::read_dir(&id_path)
            .map_err(|error| format!("read artifact id {}: {error}", id_path.display()))?
        {
            let digest_entry =
                digest_entry.map_err(|error| format!("read artifact digest entry: {error}"))?;
            let digest = digest_entry
                .file_name()
                .to_str()
                .ok_or_else(|| "artifact digest directory is not UTF-8".to_owned())?
                .to_owned();
            if !valid_digest(&digest) {
                return Err(format!(
                    "unexpected artifact digest path: {}",
                    digest_entry.path().display()
                ));
            }
            let referenced = valid_id.is_some_and(|plugin| plugin.artifacts.contains_key(&digest));
            if !referenced {
                remove_tree(&digest_entry.path())?;
            }
        }
        if fs::read_dir(&id_path)
            .map_err(|error| format!("read artifact id {}: {error}", id_path.display()))?
            .next()
            .is_none()
        {
            fs::remove_dir(&id_path).map_err(|error| {
                format!("remove empty artifact id {}: {error}", id_path.display())
            })?;
        }
    }
    sentinel::sync_dir(&artifacts)
        .map_err(|error| format!("fsync artifact store {}: {error}", artifacts.display()))
}

fn remove_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect cleanup target {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).map_err(|error| format!("remove {}: {error}", path.display()))
    } else if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("read cleanup directory {}: {error}", path.display()))?
        {
            let child = entry
                .map_err(|error| format!("read cleanup entry: {error}"))?
                .path();
            remove_tree(&child)?;
        }
        fs::remove_dir(path).map_err(|error| format!("remove {}: {error}", path.display()))
    } else {
        Err(format!(
            "refusing to remove special path {}",
            path.display()
        ))
    }
}

fn reject_final_symlink(path: &Path, subject: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "{subject} must not be a symbolic link: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect {subject} {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "clat-plugin-store-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("root");
        root
    }

    fn package(root: &Path, id: &str, version: &str, capabilities: Value) -> PathBuf {
        let package = root.join(format!("package-{version}"));
        fs::create_dir_all(&package).expect("package");
        let entry = package.join("plugin.wasm");
        fs::write(&entry, format!("fixture-{version}")).expect("entry");
        let digest = hash_file_bounded(&entry, fs::metadata(&entry).expect("metadata").len())
            .expect("digest");
        fs::write(
            package.join(MANIFEST_FILE),
            serde_json::to_vec(&json!({
                "manifestVersion": 1,
                "id": id,
                "name": "Fixture",
                "version": version,
                "runtime": {
                    "kind": "wasm-component",
                    "entry": "plugin.wasm",
                    "sha256": digest,
                },
                "capabilities": capabilities,
            }))
            .expect("manifest json"),
        )
        .expect("manifest");
        package
    }

    fn mcp_package(root: &Path, version: &str) -> PathBuf {
        let package = root.join(format!("mcp-package-{version}"));
        fs::create_dir_all(&package).expect("mcp package");
        let entry_name = if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        };
        let entry = package.join(entry_name);
        fs::write(&entry, format!("mcp-{version}")).expect("mcp entry");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&entry, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let digest = hash_file_bounded(&entry, fs::metadata(&entry).expect("metadata").len())
            .expect("digest");
        fs::write(
            package.join(MANIFEST_FILE),
            serde_json::to_vec(&json!({
                "manifestVersion": 1,
                "id": "dev.clat.mcp-lifecycle",
                "name": "MCP Lifecycle",
                "version": version,
                "runtime": {
                    "kind": "mcp-stdio",
                    "entry": entry_name,
                    "sha256": digest,
                },
                "capabilities": { "tools": true },
            }))
            .expect("manifest"),
        )
        .expect("write manifest");
        package
    }

    #[test]
    fn install_update_failure_reopen_rollback_enable_and_uninstall_sequence() {
        let root = root("sequence");
        let v1 = package(&root, "dev.clat.fixture", "1.0.0", json!({"tools": true}));
        let v2 = package(&root, "dev.clat.fixture", "2.0.0", json!({"tools": true}));
        let v2_digest = PackageStore::inspect(&v2).expect("inspect v2").tree_sha256;
        let mut store = PackageStore::open(&root).expect("open");
        assert!(
            store
                .install(&v1, None, false, InstallKind::Install)
                .expect_err("first capabilities need review")
                .contains("--accept-capabilities")
        );
        store
            .install(&v1, None, true, InstallKind::Install)
            .expect("install v1");
        store.inject_failure_before_registry_publish();
        assert!(
            store
                .install(&v2, None, false, InstallKind::Update)
                .expect_err("injected update failure")
                .contains("injected failure")
        );
        drop(store);
        let stale = root.join(STORE_DIR).join(STAGING_DIR).join("stale");
        fs::create_dir_all(&stale).expect("stale staging");
        fs::write(stale.join("partial"), "partial").expect("partial staging");

        let mut store = PackageStore::open(&root).expect("reopen");
        assert!(
            !stale.exists(),
            "stale staging must be inert and recoverable"
        );
        assert!(
            !root
                .join(STORE_DIR)
                .join(ARTIFACTS_DIR)
                .join("dev.clat.fixture")
                .join(&v2_digest)
                .exists(),
            "unreferenced published artifacts must be reclaimed on reopen"
        );
        assert_eq!(store.list()[0].version, "1.0.0");
        store
            .install(&v2, None, false, InstallKind::Update)
            .expect("same capabilities update");
        assert_eq!(store.list()[0].version, "2.0.0");
        store.rollback("dev.clat.fixture").expect("rollback");
        assert_eq!(store.list()[0].version, "1.0.0");
        store
            .set_enabled("dev.clat.fixture", false)
            .expect("disable");
        assert!(active_packages(&root).expect("active").packages.is_empty());
        store.set_enabled("dev.clat.fixture", true).expect("enable");
        assert_eq!(active_packages(&root).expect("active").packages.len(), 1);
        store.uninstall("dev.clat.fixture").expect("uninstall");
        assert!(store.list().is_empty());
        assert!(active_packages(&root).expect("active").packages.is_empty());
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn capability_expansion_requires_a_fresh_review() {
        let root = root("capabilities");
        let v1 = package(&root, "dev.clat.fixture", "1.0.0", json!({"tools": true}));
        let v2 = package(
            &root,
            "dev.clat.fixture",
            "2.0.0",
            json!({"tools": true, "hostContext": true, "hostTools": ["read_file"]}),
        );
        let mut store = PackageStore::open(&root).expect("open");
        store
            .install(&v1, None, true, InstallKind::Install)
            .expect("install");
        let error = store
            .install(&v2, None, false, InstallKind::Update)
            .expect_err("expansion must stop");
        assert!(error.contains("hostContext"));
        assert!(error.contains("hostTools.read_file"));
        assert_eq!(store.list()[0].version, "1.0.0");
        store
            .install(&v2, None, true, InstallKind::Update)
            .expect("reviewed update");
        assert_eq!(store.list()[0].version, "2.0.0");
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn mcp_runtime_uses_the_same_update_rollback_and_enable_lifecycle() {
        let root = root("mcp-lifecycle");
        let v1 = mcp_package(&root, "1.0.0");
        let v2 = mcp_package(&root, "2.0.0");
        let mut store = PackageStore::open(&root).expect("store");
        store
            .install(&v1, None, true, InstallKind::Install)
            .expect("install v1");
        store
            .install(&v2, None, false, InstallKind::Update)
            .expect("update v2");
        assert_eq!(store.list()[0].version, "2.0.0");
        store.rollback("dev.clat.mcp-lifecycle").expect("rollback");
        assert_eq!(store.list()[0].version, "1.0.0");
        store
            .set_enabled("dev.clat.mcp-lifecycle", false)
            .expect("disable");
        assert!(
            active_packages_for_runtime(&root, PluginRuntimeKind::McpStdio)
                .expect("active")
                .packages
                .is_empty()
        );
        store
            .set_enabled("dev.clat.mcp-lifecycle", true)
            .expect("enable");
        assert_eq!(
            active_packages_for_runtime(&root, PluginRuntimeKind::McpStdio)
                .expect("active")
                .packages[0]
                .manifest
                .version,
            "1.0.0"
        );
        store
            .uninstall("dev.clat.mcp-lifecycle")
            .expect("uninstall");
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_package_and_registry_attacks_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = root("symlink");
        let package = package(&root, "dev.clat.fixture", "1.0.0", json!({}));
        let outside = root.join("outside");
        fs::write(&outside, "outside").expect("outside");
        symlink(&outside, package.join("link")).expect("package symlink");
        assert!(
            PackageStore::inspect(&package)
                .expect_err("package symlink")
                .contains("symbolic")
        );
        fs::remove_file(package.join("link")).expect("remove link");

        let mut store = PackageStore::open(&root).expect("open");
        store
            .install(&package, None, false, InstallKind::Install)
            .expect("zero-capability install");
        drop(store);
        let registry = root.join(STORE_DIR).join(REGISTRY_FILE);
        let victim = root.join("victim");
        fs::write(&victim, "victim").expect("victim");
        fs::remove_file(&registry).expect("remove registry");
        symlink(&victim, &registry).expect("registry symlink");
        let error = match PackageStore::open(&root) {
            Ok(_) => panic!("registry symlink must fail"),
            Err(error) => error,
        };
        assert!(error.contains("symbolic"));
        assert_eq!(fs::read_to_string(victim).expect("victim"), "victim");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn registry_version_and_artifact_tree_tampering_fail_closed() {
        let root = root("tamper");
        let damaged = package(&root, "dev.clat.fixture", "1.0.0", json!({}));
        let healthy = package(&root, "dev.clat.healthy", "2.0.0", json!({}));
        let digest;
        {
            let mut store = PackageStore::open(&root).expect("open");
            digest = store
                .install(&damaged, None, false, InstallKind::Install)
                .expect("install")
                .tree_sha256;
            store
                .install(&healthy, None, false, InstallKind::Install)
                .expect("install healthy peer");
        }
        let artifact = root
            .join(STORE_DIR)
            .join(ARTIFACTS_DIR)
            .join("dev.clat.fixture")
            .join(&digest);
        fs::write(artifact.join("injected.js"), "malicious").expect("tamper tree");
        let active = active_packages(&root).expect("registry remains readable");
        assert_eq!(active.packages.len(), 1);
        assert_eq!(active.packages[0].id, "dev.clat.healthy");
        assert!(
            active
                .failures
                .iter()
                .any(|error| error.contains("activation verification")),
            "{:?}",
            active.failures
        );
        let listed = installed_packages(&root).expect("list remains available");
        assert!(
            listed
                .iter()
                .find(|entry| entry.id == "dev.clat.fixture")
                .and_then(|entry| entry.health.as_ref())
                .is_some()
        );
        let mut store = PackageStore::open(&root).expect("mutator opens around bad artifact");
        store
            .set_enabled("dev.clat.fixture", false)
            .expect("disable is a recovery operation");
        assert!(
            store
                .set_enabled("dev.clat.fixture", true)
                .expect_err("re-enable must verify bytes")
                .contains("activation verification")
        );
        store
            .uninstall("dev.clat.fixture")
            .expect("uninstall corrupted package");
        drop(store);

        let registry_path = root.join(STORE_DIR).join(REGISTRY_FILE);
        let mut registry: Value =
            serde_json::from_slice(&fs::read(&registry_path).expect("registry"))
                .expect("registry json");
        registry["unit"]["version"] = json!(999);
        fs::write(
            &registry_path,
            serde_json::to_vec(&registry).expect("serialize registry"),
        )
        .expect("write registry");
        let error = active_packages(&root).expect_err("unknown version must fail");
        assert!(
            error.contains("unsupported plugin registry unit"),
            "{error}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn incomplete_or_invalid_publisher_signatures_never_gain_trust() {
        let root = root("signature");
        let package = package(&root, "dev.clat.fixture", "1.0.0", json!({}));
        fs::write(
            package.join(PUBLISHER_FILE),
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "publisher": "dev.clat.publisher",
                "publicKey": "not-a-key"
            }))
            .expect("publisher"),
        )
        .expect("publisher");
        let error = PackageStore::inspect(&package).expect_err("missing signature");
        assert!(error.contains("require both"), "{error}");
        fs::write(package.join(SIGNATURE_FILE), "not-a-signature").expect("signature");
        let error = PackageStore::inspect(&package).expect_err("invalid signature");
        assert!(
            error.contains("decode package signature") || error.contains("publisher public key"),
            "{error}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn a_running_storage_root_owner_makes_package_mutation_busy() {
        let root = root("busy");
        let application_lease = crate::session::root_lease::try_acquire(&root)
            .expect("lease")
            .expect("first owner");
        let error = match PackageStore::open(&root) {
            Ok(_) => panic!("a second package writer must not mount"),
            Err(error) => error,
        };
        assert!(error.contains("busy"), "{error}");
        drop(application_lease);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn package_and_config_bounds_apply_before_activation() {
        let root = root("bounds");
        let package = package(&root, "dev.clat.fixture", "1.0.0", json!({}));
        let oversized = package.join("oversized.bin");
        File::create(&oversized)
            .expect("large file")
            .set_len(MAX_PACKAGE_FILE_BYTES + 1)
            .expect("sparse length");
        let error = PackageStore::inspect(&package).expect_err("oversized package");
        assert!(error.contains("cap"), "{error}");
        fs::remove_file(&oversized).expect("remove oversized file");

        let mut store = PackageStore::open(&root).expect("store");
        let error = store
            .install(
                &package,
                Some(Value::String("x".repeat(MAX_PLUGIN_CONFIG_BYTES + 1))),
                false,
                InstallKind::Install,
            )
            .expect_err("oversized config");
        assert!(error.contains("plugin config"), "{error}");
        assert!(store.list().is_empty());
        drop(store);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
