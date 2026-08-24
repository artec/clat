//! Signed remote plugin market, deterministic dependency solver and secure
//! download hand-off to the transactional local package store.

use super::{
    InstallKind, PackageInstallRequest, PackageMutation, PackageStore, PluginCapabilities,
    PluginRuntimeKind, PublisherIdentity, TrustLabel, unpack_bundle,
};
use crate::upgrade::target_triple;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ureq::Agent;
use url::Url;

pub(crate) const DEFAULT_MARKET_URL: &str = "https://pi.at.cn/";
const MARKET_ID: &str = "cn.at.pi";
const INDEX_FILE: &str = "index.json";
const SIGNATURE_FILE: &str = "index.json.minisig";
const RELEASE_PUBLIC_KEY_FILE: &str = include_str!("../../release/minisign.pub");
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_PACKAGES: usize = 4_096;
const MAX_VERSIONS: usize = 32_768;
const MAX_PUBLISHERS: usize = 4_096;
const MAX_DEPENDENCIES: usize = 256;
const MAX_SOLUTION_PACKAGES: usize = 256;
const MAX_CLOCK_SKEW_SECONDS: u64 = 15 * 60;
const MAX_INDEX_LIFETIME_SECONDS: u64 = 14 * 24 * 60 * 60;
const MAX_INSTALL_WALL_TIME: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketIndex {
    pub(crate) schema_version: u32,
    pub(crate) market: MarketMetadata,
    #[serde(default)]
    pub(crate) publishers: Vec<MarketPublisher>,
    #[serde(default)]
    pub(crate) packages: Vec<MarketPackage>,
    #[serde(default)]
    pub(crate) revocations: Vec<MarketRevocation>,
    #[serde(default)]
    pub(crate) vulnerabilities: Vec<MarketVulnerability>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketMetadata {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) generated_at_unix: u64,
    pub(crate) expires_at_unix: u64,
    pub(crate) homepage: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PublisherStatus {
    Trusted,
    Suspended,
    Revoked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PublisherKeyStatus {
    Active,
    Retired,
    Revoked,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketPublisher {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: PublisherStatus,
    pub(crate) review_url: String,
    pub(crate) keys: Vec<MarketPublisherKey>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketPublisherKey {
    pub(crate) id: String,
    pub(crate) public_key: String,
    pub(crate) status: PublisherKeyStatus,
    pub(crate) not_before_unix: u64,
    pub(crate) not_after_unix: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketPackage {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) homepage: String,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) versions: Vec<MarketVersion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketVersion {
    pub(crate) version: String,
    pub(crate) runtime: PluginRuntimeKind,
    pub(crate) publisher: String,
    pub(crate) publisher_key: String,
    pub(crate) published_at_unix: u64,
    #[serde(default)]
    pub(crate) capabilities: PluginCapabilities,
    #[serde(default)]
    pub(crate) dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) compatibility: MarketCompatibility,
    #[serde(default)]
    pub(crate) yanked: bool,
    pub(crate) artifacts: Vec<MarketArtifact>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketCompatibility {
    #[serde(default)]
    pub(crate) min_clat: Option<String>,
    #[serde(default)]
    pub(crate) max_clat: Option<String>,
    #[serde(default)]
    pub(crate) wit: Option<String>,
    #[serde(default)]
    pub(crate) dsh_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketArtifact {
    pub(crate) target: String,
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketRevocation {
    pub(crate) package: String,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) artifact_sha256: Option<String>,
    pub(crate) effective_at_unix: u64,
    pub(crate) reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VulnerabilitySeverity {
    Low,
    Moderate,
    High,
    Critical,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarketVulnerability {
    pub(crate) id: String,
    pub(crate) package: String,
    pub(crate) affected: String,
    pub(crate) severity: VulnerabilitySeverity,
    pub(crate) summary: String,
    pub(crate) url: String,
}

#[derive(Clone, Debug)]
pub(crate) struct MarketSelection {
    pub(crate) package: MarketPackage,
    pub(crate) version: MarketVersion,
    pub(crate) artifact: MarketArtifact,
}

#[derive(Clone, Debug)]
pub(crate) struct MarketAuditFinding {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) id: String,
    pub(crate) severity: VulnerabilitySeverity,
    pub(crate) summary: String,
    pub(crate) url: Option<String>,
}

pub(crate) struct MarketInstallOptions {
    pub(crate) root_id: String,
    pub(crate) version: String,
    pub(crate) config: Option<serde_json::Value>,
    pub(crate) accept_capabilities: bool,
    pub(crate) accept_vulnerabilities: bool,
    pub(crate) root_kind: InstallKind,
}

pub(crate) struct Market {
    base: Url,
    pub(crate) index: MarketIndex,
}

impl Market {
    pub(crate) fn load(storage_root: &Path, base: &str) -> Result<Self, String> {
        let base = parse_market_base(base)?;
        let agent = network_agent();
        let fetched = fetch_pair(&agent, &base);
        let (index_bytes, signature_bytes) = match fetched {
            Ok(pair) => {
                let index = verify_signed_index(&pair.0, &pair.1, now_unix()?)?;
                write_cache(storage_root, &base, &pair.0, &pair.1)?;
                return Ok(Self { base, index });
            }
            Err(network_error) => read_cache(storage_root, &base).map_err(|cache_error| {
                format!("market unavailable ({network_error}); no valid cache ({cache_error})")
            })?,
        };
        let index = verify_signed_index(&index_bytes, &signature_bytes, now_unix()?)?;
        Ok(Self { base, index })
    }

    #[cfg(test)]
    fn from_index(base: &str, index: MarketIndex) -> Self {
        Self {
            base: parse_market_base(base).unwrap(),
            index,
        }
    }

    #[cfg(test)]
    fn load_with_key(
        base: &str,
        public_key: &minisign_verify::PublicKey,
        now: u64,
    ) -> Result<Self, String> {
        let base = parse_market_base(base)?;
        let (index, signature) = fetch_pair(&network_agent(), &base)?;
        let index = verify_signed_index_with_key(&index, &signature, now, public_key)?;
        Ok(Self { base, index })
    }

    pub(crate) fn search(&self, query: &str) -> Vec<&MarketPackage> {
        let query = query.trim().to_ascii_lowercase();
        let mut packages = self
            .index
            .packages
            .iter()
            .filter(|package| {
                query.is_empty()
                    || package.id.to_ascii_lowercase().contains(&query)
                    || package.name.to_ascii_lowercase().contains(&query)
                    || package.summary.to_ascii_lowercase().contains(&query)
                    || package
                        .tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(&query))
            })
            .collect::<Vec<_>>();
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        packages
    }

    pub(crate) fn package(&self, id: &str) -> Result<&MarketPackage, String> {
        self.index
            .packages
            .iter()
            .find(|package| package.id == id)
            .ok_or_else(|| format!("market has no package `{id}`"))
    }

    pub(crate) fn latest(&self, id: &str) -> Result<MarketSelection, String> {
        self.solve(id, "*")?
            .into_iter()
            .find(|selection| selection.package.id == id)
            .ok_or_else(|| format!("market dependency solution lost root package `{id}`"))
    }

    pub(crate) fn solve(
        &self,
        root_id: &str,
        root_range: &str,
    ) -> Result<Vec<MarketSelection>, String> {
        let now = now_unix()?;
        let mut constraints = BTreeMap::<String, Vec<String>>::new();
        constraints.insert(root_id.to_owned(), vec![root_range.to_owned()]);
        let mut selected = BTreeMap::<String, MarketSelection>::new();
        let mut pending = BTreeSet::from([root_id.to_owned()]);
        while let Some(id) = pending.pop_first() {
            if selected.len() >= MAX_SOLUTION_PACKAGES && !selected.contains_key(&id) {
                return Err(format!(
                    "dependency solution exceeds {MAX_SOLUTION_PACKAGES} packages"
                ));
            }
            let ranges = constraints
                .get(&id)
                .expect("pending package has constraints");
            if let Some(existing) = selected.get(&id) {
                if ranges
                    .iter()
                    .all(|range| version_matches(&existing.version.version, range).unwrap_or(false))
                {
                    continue;
                }
                return Err(format!(
                    "dependency conflict for `{id}`: selected {}, constraints {}",
                    existing.version.version,
                    ranges.join(" & ")
                ));
            }
            let package = self.package(&id)?.clone();
            let mut candidates = package
                .versions
                .iter()
                .filter_map(|version| {
                    let parsed = SemVersion::parse(&version.version).ok()?;
                    if version.yanked
                        || ranges
                            .iter()
                            .any(|range| !version_matches_parsed(&parsed, range).unwrap_or(false))
                        || !clat_compatible(&version.compatibility)
                        || validate_publisher(&self.index, version, now).is_err()
                    {
                        return None;
                    }
                    let artifact = select_artifact(version)?;
                    if self
                        .revocation(version, &package.id, artifact, now)
                        .is_some()
                    {
                        return None;
                    }
                    Some((parsed, version.clone(), artifact.clone()))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| right.0.cmp(&left.0));
            let Some((_, version, artifact)) = candidates.into_iter().next() else {
                return Err(format!(
                    "no installable version of `{id}` satisfies {} for target {}",
                    ranges.join(" & "),
                    target_triple()
                ));
            };
            if version.dependencies.len() > MAX_DEPENDENCIES {
                return Err(format!(
                    "package `{id}` exceeds dependency cap {MAX_DEPENDENCIES}"
                ));
            }
            for (dependency, range) in &version.dependencies {
                constraints
                    .entry(dependency.clone())
                    .or_default()
                    .push(range.clone());
                pending.insert(dependency.clone());
            }
            selected.insert(
                id,
                MarketSelection {
                    package,
                    version,
                    artifact,
                },
            );
        }
        reject_dependency_cycles(&selected)?;
        topological_selections(selected)
    }

    pub(crate) fn audit_installed(
        &self,
        storage_root: &Path,
    ) -> Result<Vec<MarketAuditFinding>, String> {
        let now = now_unix()?;
        let installed = super::installed_packages(storage_root)?;
        let mut findings = Vec::new();
        for package in installed {
            for advisory in &self.index.vulnerabilities {
                if advisory.package == package.id
                    && version_matches(&package.version, &advisory.affected)?
                {
                    findings.push(MarketAuditFinding {
                        package: package.id.clone(),
                        version: package.version.clone(),
                        id: advisory.id.clone(),
                        severity: advisory.severity,
                        summary: advisory.summary.clone(),
                        url: Some(advisory.url.clone()),
                    });
                }
            }
            let Some(record) = self
                .index
                .packages
                .iter()
                .find(|record| record.id == package.id)
            else {
                continue;
            };
            let Some(version) = record
                .versions
                .iter()
                .find(|version| version.version == package.version)
            else {
                continue;
            };
            match validate_publisher(&self.index, version, now) {
                Ok(expected)
                    if package.publisher.as_deref() != Some(expected.publisher.as_str())
                        || package.publisher_key.as_deref().map(str::trim)
                            != Some(expected.public_key.trim()) =>
                {
                    findings.push(MarketAuditFinding {
                        package: package.id.clone(),
                        version: package.version.clone(),
                        id: "MARKET-TRUST-MISMATCH".into(),
                        severity: VulnerabilitySeverity::Critical,
                        summary: "installed publisher identity no longer matches the signed market record".into(),
                        url: None,
                    });
                }
                Err(error) => findings.push(MarketAuditFinding {
                    package: package.id.clone(),
                    version: package.version.clone(),
                    id: "MARKET-PUBLISHER-REVOKED".into(),
                    severity: VulnerabilitySeverity::Critical,
                    summary: error,
                    url: None,
                }),
                Ok(_) => {}
            }
            if let Some(revocation) = self.index.revocations.iter().find(|revocation| {
                revocation.effective_at_unix <= now
                    && revocation.package == package.id
                    && revocation
                        .version
                        .as_ref()
                        .is_none_or(|value| value == &version.version)
                    && revocation.artifact_sha256.as_ref().is_none_or(|digest| {
                        version
                            .artifacts
                            .iter()
                            .any(|artifact| artifact.sha256.eq_ignore_ascii_case(digest))
                    })
            }) {
                findings.push(MarketAuditFinding {
                    package: package.id.clone(),
                    version: package.version.clone(),
                    id: "MARKET-REVOKED".into(),
                    severity: VulnerabilitySeverity::Critical,
                    summary: revocation.reason.clone(),
                    url: None,
                });
            }
        }
        findings.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then_with(|| left.package.cmp(&right.package))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(findings)
    }

    pub(crate) fn install(
        &self,
        storage_root: &Path,
        options: MarketInstallOptions,
    ) -> Result<Vec<PackageMutation>, String> {
        let selections = self.solve(&options.root_id, &options.version)?;
        let mut store = PackageStore::open(storage_root)?;
        let installed = store
            .list()
            .into_iter()
            .map(|entry| (entry.id, entry.version))
            .collect::<BTreeMap<_, _>>();
        match options.root_kind {
            InstallKind::Install if installed.contains_key(&options.root_id) => {
                return Err(format!(
                    "plugin `{}` is already installed; use `clat plugin market update`",
                    options.root_id
                ));
            }
            InstallKind::Update if !installed.contains_key(&options.root_id) => {
                return Err(format!(
                    "plugin `{}` is not installed; use `clat plugin market install`",
                    options.root_id
                ));
            }
            _ => {}
        }
        if !options.accept_vulnerabilities {
            let mut blocked = Vec::new();
            for selection in &selections {
                for advisory in &self.index.vulnerabilities {
                    if advisory.package == selection.package.id
                        && version_matches(&selection.version.version, &advisory.affected)?
                    {
                        blocked.push(format!(
                            "{} {} ({:?}: {})",
                            advisory.id, selection.package.id, advisory.severity, advisory.summary
                        ));
                    }
                }
            }
            if !blocked.is_empty() {
                return Err(format!(
                    "known vulnerabilities block installation: {}; retry only after review with \
                     `--accept-vulnerabilities`",
                    blocked.join("; ")
                ));
            }
        }
        let root_staging = storage_root.join("plugin-market-staging");
        create_private_dir(&root_staging)?;
        let transaction = root_staging.join(uuid::Uuid::new_v4().to_string());
        create_private_dir(&transaction)?;
        let result = (|| {
            let started = Instant::now();
            let mut requests = Vec::new();
            for selection in selections {
                if started.elapsed() > MAX_INSTALL_WALL_TIME {
                    return Err("market install exceeded the 15 minute transaction limit".into());
                }
                if installed
                    .get(&selection.package.id)
                    .is_some_and(|version| version == &selection.version.version)
                {
                    continue;
                }
                let bundle = transaction.join(format!("{}.clatpkg", selection.package.id));
                self.download_artifact(&selection.artifact, &bundle)?;
                let package_root = transaction.join(format!("package-{}", selection.package.id));
                unpack_bundle(&bundle, &package_root)?;
                let inspection = PackageStore::inspect(&package_root)?;
                validate_downloaded_package(&self.index, &selection, &inspection, now_unix()?)?;
                let kind = if installed.contains_key(&selection.package.id) {
                    InstallKind::Update
                } else {
                    InstallKind::Install
                };
                requests.push(PackageInstallRequest {
                    path: package_root,
                    config: if selection.package.id == options.root_id {
                        options.config.clone()
                    } else {
                        None
                    },
                    accept_capabilities: options.accept_capabilities,
                    kind,
                });
            }
            if requests.is_empty() {
                return Err("selected market versions are already installed".into());
            }
            store.install_batch(requests)
        })();
        if transaction.exists() {
            let _ = fs::remove_dir_all(&transaction);
        }
        result
    }

    fn download_artifact(&self, artifact: &MarketArtifact, output: &Path) -> Result<(), String> {
        validate_digest(&artifact.sha256)?;
        if artifact.bytes == 0 || artifact.bytes > super::bundle::MAX_BUNDLE_BYTES {
            return Err(format!(
                "market artifact has invalid size {}",
                artifact.bytes
            ));
        }
        let url = self
            .base
            .join(&artifact.url)
            .map_err(|error| format!("resolve artifact URL: {error}"))?;
        validate_remote_url(&url)?;
        let response = network_agent()
            .get(url.as_str())
            .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
            .call()
            .map_err(|error| format!("download {}: {error}", artifact.url))?;
        if !response.status().is_success() {
            return Err(format!(
                "download {}: HTTP {}",
                artifact.url,
                response.status()
            ));
        }
        let (_, body) = response.into_parts();
        let mut reader = body.into_reader();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(output)
            .map_err(|error| format!("create downloaded bundle: {error}"))?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("read downloaded bundle: {error}"))?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(read as u64)
                .ok_or_else(|| "download size overflow".to_owned())?;
            if total > artifact.bytes || total > super::bundle::MAX_BUNDLE_BYTES {
                return Err("download exceeded advertised bundle size".into());
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("write downloaded bundle: {error}"))?;
        }
        file.sync_all()
            .map_err(|error| format!("sync downloaded bundle: {error}"))?;
        if total != artifact.bytes {
            return Err(format!(
                "downloaded bundle is {total} bytes; expected {}",
                artifact.bytes
            ));
        }
        let digest = format!("{:x}", hasher.finalize());
        if digest != artifact.sha256.to_ascii_lowercase() {
            return Err("downloaded bundle SHA-256 mismatch".into());
        }
        Ok(())
    }

    fn revocation<'a>(
        &'a self,
        version: &MarketVersion,
        package: &str,
        artifact: &MarketArtifact,
        now: u64,
    ) -> Option<&'a MarketRevocation> {
        self.index.revocations.iter().find(|revocation| {
            revocation.effective_at_unix <= now
                && revocation.package == package
                && revocation
                    .version
                    .as_ref()
                    .is_none_or(|value| value == &version.version)
                && revocation
                    .artifact_sha256
                    .as_ref()
                    .is_none_or(|value| value.eq_ignore_ascii_case(&artifact.sha256))
        })
    }
}

fn fetch_pair(agent: &Agent, base: &Url) -> Result<(Vec<u8>, Vec<u8>), String> {
    let index = base
        .join(INDEX_FILE)
        .map_err(|error| format!("resolve market index: {error}"))?;
    let signature = base
        .join(SIGNATURE_FILE)
        .map_err(|error| format!("resolve market signature: {error}"))?;
    Ok((
        download_small(agent, &index, MAX_INDEX_BYTES, "market index")?,
        download_small(
            agent,
            &signature,
            MAX_SIGNATURE_BYTES,
            "market index signature",
        )?,
    ))
}

fn download_small(agent: &Agent, url: &Url, cap: usize, label: &str) -> Result<Vec<u8>, String> {
    validate_remote_url(url)?;
    let response = agent
        .get(url.as_str())
        .header("User-Agent", format!("clat/{}", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|error| format!("download {label}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("download {label}: HTTP {}", response.status()));
    }
    let (_, body) = response.into_parts();
    let mut bytes = Vec::new();
    body.into_reader()
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {label}: {error}"))?;
    if bytes.len() > cap {
        return Err(format!("{label} exceeds {cap} bytes"));
    }
    Ok(bytes)
}

fn verify_signed_index(index: &[u8], signature: &[u8], now: u64) -> Result<MarketIndex, String> {
    verify_signed_index_with_key(index, signature, now, &release_public_key()?)
}

fn verify_signed_index_with_key(
    index: &[u8],
    signature: &[u8],
    now: u64,
    public_key: &minisign_verify::PublicKey,
) -> Result<MarketIndex, String> {
    let signature_text = std::str::from_utf8(signature)
        .map_err(|_| "market index signature is not UTF-8".to_owned())?;
    let signature = minisign_verify::Signature::decode(signature_text)
        .map_err(|error| format!("decode market index signature: {error}"))?;
    public_key
        .verify(index, &signature, false)
        .map_err(|error| format!("verify market index signature: {error}"))?;
    let parsed: MarketIndex =
        serde_json::from_slice(index).map_err(|error| format!("parse market index: {error}"))?;
    validate_index(&parsed, now)?;
    let expected = format!(
        "CLAT plugin index {} generated {}",
        parsed.market.id, parsed.market.generated_at_unix
    );
    if signature.trusted_comment() != expected {
        return Err(format!(
            "market signature trusted comment is bound to {:?}; expected {:?}",
            signature.trusted_comment(),
            expected
        ));
    }
    Ok(parsed)
}

fn validate_index(index: &MarketIndex, now: u64) -> Result<(), String> {
    if index.schema_version != 1 {
        return Err(format!(
            "unsupported market schema {}",
            index.schema_version
        ));
    }
    if index.market.id != MARKET_ID {
        return Err(format!("unexpected market id `{}`", index.market.id));
    }
    if index.market.generated_at_unix > now.saturating_add(MAX_CLOCK_SKEW_SECONDS) {
        return Err("market index was generated too far in the future".into());
    }
    if index.market.expires_at_unix <= now {
        return Err("market index is expired".into());
    }
    if index.market.expires_at_unix <= index.market.generated_at_unix
        || index
            .market
            .expires_at_unix
            .saturating_sub(index.market.generated_at_unix)
            > MAX_INDEX_LIFETIME_SECONDS
    {
        return Err("market index lifetime is invalid".into());
    }
    if index.packages.len() > MAX_PACKAGES
        || index.publishers.len() > MAX_PUBLISHERS
        || index
            .packages
            .iter()
            .map(|package| package.versions.len())
            .sum::<usize>()
            > MAX_VERSIONS
    {
        return Err("market index exceeds catalog limits".into());
    }
    validate_public_https(&index.market.homepage, "market homepage")?;
    let mut publisher_ids = BTreeSet::new();
    for publisher in &index.publishers {
        validate_identifier(&publisher.id, "publisher")?;
        validate_public_https(&publisher.review_url, "publisher review URL")?;
        if !publisher_ids.insert(&publisher.id) {
            return Err(format!("duplicate publisher `{}`", publisher.id));
        }
        if publisher.keys.is_empty() || publisher.keys.len() > 16 {
            return Err(format!(
                "publisher `{}` has invalid key count",
                publisher.id
            ));
        }
        let mut key_ids = BTreeSet::new();
        let mut key_material = BTreeSet::new();
        for key in &publisher.keys {
            validate_identifier(&key.id, "publisher key")?;
            if !key_ids.insert(&key.id) {
                return Err(format!(
                    "publisher `{}` has duplicate key `{}`",
                    publisher.id, key.id
                ));
            }
            if key.not_after_unix <= key.not_before_unix {
                return Err(format!("publisher key `{}` has invalid validity", key.id));
            }
            minisign_verify::PublicKey::from_base64(key.public_key.trim())
                .map_err(|error| format!("publisher key `{}` is invalid: {error}", key.id))?;
            if !key_material.insert(key.public_key.trim()) {
                return Err(format!(
                    "publisher `{}` repeats the same public key under multiple ids",
                    publisher.id
                ));
            }
        }
    }
    let mut package_ids = BTreeSet::new();
    for package in &index.packages {
        validate_identifier(&package.id, "package")?;
        if !package_ids.insert(&package.id) {
            return Err(format!("duplicate market package `{}`", package.id));
        }
        let mut versions = BTreeSet::new();
        for version in &package.versions {
            SemVersion::parse(&version.version)?;
            validate_identifier(&version.publisher, "version publisher")?;
            validate_identifier(&version.publisher_key, "version publisher key")?;
            if version.published_at_unix > index.market.generated_at_unix {
                return Err(format!(
                    "package `{}` version `{}` has a future publication timestamp",
                    package.id, version.version
                ));
            }
            validate_compatibility(&version.compatibility)?;
            if !versions.insert(&version.version) {
                return Err(format!(
                    "package `{}` has duplicate version `{}`",
                    package.id, version.version
                ));
            }
            if version.dependencies.len() > MAX_DEPENDENCIES {
                return Err(format!(
                    "package `{}` has too many dependencies",
                    package.id
                ));
            }
            for (dependency, range) in &version.dependencies {
                validate_identifier(dependency, "dependency")?;
                validate_range(range)?;
            }
            if version.artifacts.is_empty() || version.artifacts.len() > 32 {
                return Err(format!(
                    "package `{}` has invalid artifact count",
                    package.id
                ));
            }
            let mut artifact_targets = BTreeSet::new();
            for artifact in &version.artifacts {
                validate_digest(&artifact.sha256)?;
                if artifact.bytes == 0 || artifact.bytes > super::bundle::MAX_BUNDLE_BYTES {
                    return Err(format!(
                        "package `{}` has invalid artifact size",
                        package.id
                    ));
                }
                if artifact.target.is_empty() || artifact.target.len() > 128 {
                    return Err(format!(
                        "package `{}` has invalid artifact target",
                        package.id
                    ));
                }
                if !artifact_targets.insert(&artifact.target) {
                    return Err(format!(
                        "package `{}` version `{}` repeats artifact target `{}`",
                        package.id, version.version, artifact.target
                    ));
                }
                let joined = Url::parse(DEFAULT_MARKET_URL)
                    .expect("default market URL")
                    .join(&artifact.url)
                    .map_err(|error| format!("package artifact URL is invalid: {error}"))?;
                validate_remote_url(&joined)?;
            }
        }
    }
    for revocation in &index.revocations {
        validate_identifier(&revocation.package, "revoked package")?;
        if let Some(digest) = &revocation.artifact_sha256 {
            validate_digest(digest)?;
        }
    }
    for advisory in &index.vulnerabilities {
        validate_identifier(&advisory.id, "advisory")?;
        validate_identifier(&advisory.package, "advisory package")?;
        validate_range(&advisory.affected)?;
        validate_public_https(&advisory.url, "advisory URL")?;
    }
    Ok(())
}

fn validate_downloaded_package(
    index: &MarketIndex,
    selection: &MarketSelection,
    inspection: &super::PackageInspection,
    now: u64,
) -> Result<(), String> {
    if inspection.manifest.id != selection.package.id
        || inspection.manifest.version != selection.version.version
        || inspection.manifest.runtime.kind != selection.version.runtime
        || inspection.manifest.capabilities != selection.version.capabilities
    {
        return Err(format!(
            "downloaded package `{}` does not match its signed market record",
            selection.package.id
        ));
    }
    if inspection.trust != TrustLabel::PublisherVerified {
        return Err("remote market package is not publisher-signed".into());
    }
    let expected = validate_publisher(index, &selection.version, now)?;
    let actual = inspection
        .publisher
        .as_ref()
        .ok_or_else(|| "publisher-verified package lost publisher identity".to_owned())?;
    if actual.publisher != expected.publisher
        || actual.public_key.trim() != expected.public_key.trim()
    {
        return Err("downloaded package publisher does not match market trust record".into());
    }
    Ok(())
}

fn validate_compatibility(compatibility: &MarketCompatibility) -> Result<(), String> {
    let minimum = compatibility
        .min_clat
        .as_deref()
        .map(SemVersion::parse)
        .transpose()?;
    let maximum = compatibility
        .max_clat
        .as_deref()
        .map(SemVersion::parse)
        .transpose()?;
    if matches!((&minimum, &maximum), (Some(minimum), Some(maximum)) if minimum > maximum) {
        return Err("market compatibility minClat exceeds maxClat".into());
    }
    if compatibility
        .wit
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 128)
        || compatibility
            .dsh_revision
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 256)
    {
        return Err("market compatibility provenance is invalid".into());
    }
    Ok(())
}

fn validate_publisher(
    index: &MarketIndex,
    version: &MarketVersion,
    _now: u64,
) -> Result<PublisherIdentity, String> {
    let publisher = index
        .publishers
        .iter()
        .find(|publisher| publisher.id == version.publisher)
        .ok_or_else(|| {
            format!(
                "publisher `{}` is not in the market trust set",
                version.publisher
            )
        })?;
    if publisher.status != PublisherStatus::Trusted {
        return Err(format!(
            "publisher `{}` is {:?}",
            publisher.id, publisher.status
        ));
    }
    let key = publisher
        .keys
        .iter()
        .find(|key| key.id == version.publisher_key)
        .ok_or_else(|| format!("publisher key `{}` is unknown", version.publisher_key))?;
    if key.status == PublisherKeyStatus::Revoked {
        return Err(format!("publisher key `{}` is revoked", key.id));
    }
    if version.published_at_unix < key.not_before_unix
        || version.published_at_unix > key.not_after_unix
    {
        return Err(format!(
            "package was published outside key `{}` validity",
            key.id
        ));
    }
    Ok(PublisherIdentity {
        publisher: publisher.id.clone(),
        public_key: key.public_key.clone(),
    })
}

fn select_artifact(version: &MarketVersion) -> Option<&MarketArtifact> {
    version
        .artifacts
        .iter()
        .find(|artifact| artifact.target == target_triple())
        .or_else(|| {
            version
                .artifacts
                .iter()
                .find(|artifact| artifact.target == "any")
        })
}

fn clat_compatible(compatibility: &MarketCompatibility) -> bool {
    if let Some(minimum) = &compatibility.min_clat
        && matches!(
            compare_versions(env!("CARGO_PKG_VERSION"), minimum),
            Ok(Ordering::Less)
        )
    {
        return false;
    }
    if let Some(maximum) = &compatibility.max_clat
        && matches!(
            compare_versions(env!("CARGO_PKG_VERSION"), maximum),
            Ok(Ordering::Greater)
        )
    {
        return false;
    }
    true
}

fn reject_dependency_cycles(selected: &BTreeMap<String, MarketSelection>) -> Result<(), String> {
    fn visit(
        id: &str,
        selected: &BTreeMap<String, MarketSelection>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_owned()) {
            return Err(format!("dependency cycle includes `{id}`"));
        }
        let selection = selected
            .get(id)
            .ok_or_else(|| format!("dependency solution lost `{id}`"))?;
        for dependency in selection.version.dependencies.keys() {
            visit(dependency, selected, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in selected.keys() {
        visit(id, selected, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn topological_selections(
    mut selected: BTreeMap<String, MarketSelection>,
) -> Result<Vec<MarketSelection>, String> {
    let mut output = Vec::with_capacity(selected.len());
    while !selected.is_empty() {
        let ready = selected
            .iter()
            .filter(|(_, selection)| {
                selection
                    .version
                    .dependencies
                    .keys()
                    .all(|dependency| !selected.contains_key(dependency))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err("dependency graph could not be ordered".into());
        }
        for id in ready {
            output.push(selected.remove(&id).expect("ready selection exists"));
        }
    }
    Ok(output)
}

fn parse_market_base(base: &str) -> Result<Url, String> {
    let mut base = Url::parse(base).map_err(|error| format!("invalid market URL: {error}"))?;
    validate_remote_url(&base)?;
    if base.query().is_some() {
        return Err("market base URL may not contain a query".into());
    }
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok(base)
}

fn validate_remote_url(url: &Url) -> Result<(), String> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("market URLs may not contain credentials or fragments".into());
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if url.host_str().is_some_and(is_loopback_host) => Ok(()),
        _ => Err("market URLs must use HTTPS (loopback HTTP is test-only)".into()),
    }
}

fn validate_public_https(raw: &str, label: &str) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|error| format!("invalid {label}: {error}"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(format!("{label} must be an HTTPS URL without credentials"));
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn network_agent() -> Agent {
    Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .timeout_global(Some(Duration::from_secs(5 * 60)))
        // Every signed URL is authoritative. Following an unchecked redirect
        // would bypass the HTTPS/loopback scheme validation above.
        .max_redirects(0)
        .build()
        .new_agent()
}

fn release_public_key() -> Result<minisign_verify::PublicKey, String> {
    let encoded = RELEASE_PUBLIC_KEY_FILE
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("untrusted comment:"))
        .ok_or_else(|| "embedded market public key is missing".to_owned())?;
    minisign_verify::PublicKey::from_base64(encoded.trim())
        .map_err(|error| format!("invalid embedded market public key: {error}"))
}

fn cache_directory(storage_root: &Path, base: &Url) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(base.as_str().as_bytes()));
    storage_root.join("market-cache").join(digest)
}

fn write_cache(
    storage_root: &Path,
    base: &Url,
    index: &[u8],
    signature: &[u8],
) -> Result<(), String> {
    create_private_dir(storage_root)?;
    create_private_dir(&storage_root.join("market-cache"))?;
    let directory = cache_directory(storage_root, base);
    create_private_dir(&directory)?;
    write_atomic(&directory.join(INDEX_FILE), index)?;
    write_atomic(&directory.join(SIGNATURE_FILE), signature)
}

fn read_cache(storage_root: &Path, base: &Url) -> Result<(Vec<u8>, Vec<u8>), String> {
    reject_directory_symlink(storage_root)?;
    reject_directory_symlink(&storage_root.join("market-cache"))?;
    let directory = cache_directory(storage_root, base);
    reject_directory_symlink(&directory)?;
    Ok((
        read_bounded(&directory.join(INDEX_FILE), MAX_INDEX_BYTES)?,
        read_bounded(&directory.join(SIGNATURE_FILE), MAX_SIGNATURE_BYTES)?,
    ))
}

fn reject_directory_symlink(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect directory {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "path is not a regular directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("create market cache: {error}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("write market cache: {error}"))?;
        fs::rename(&temp, path).map_err(|error| format!("publish market cache: {error}"))?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn read_bounded(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect cache {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > cap as u64 {
        return Err(format!("market cache file is invalid: {}", path.display()));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(cap as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("read cache {}: {error}", path.display()))?;
    if bytes.len() > cap {
        return Err("market cache exceeds size cap".into());
    }
    Ok(bytes)
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "directory must not be a symbolic link: {}",
                path.display()
            ));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(format!("path is not a directory: {}", path.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect directory {}: {error}", path.display())),
    }
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

fn now_unix() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "system clock is before the Unix epoch".into())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("invalid {label} id `{value}`"));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<(), String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid SHA-256 digest".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemVersion {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<Vec<PreIdentifier>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PreIdentifier {
    Numeric(u64),
    Text(String),
}

impl SemVersion {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw.contains('+') {
            return Err(format!(
                "semantic build metadata is not supported in market versions: `{raw}`"
            ));
        }
        let (core, prerelease) = raw
            .split_once('-')
            .map_or((raw, None), |(core, pre)| (core, Some(pre)));
        let numbers = core
            .split('.')
            .map(|part| {
                if part.is_empty()
                    || !part.bytes().all(|byte| byte.is_ascii_digit())
                    || (part.len() > 1 && part.starts_with('0'))
                {
                    return Err(format!("invalid semantic version `{raw}`"));
                }
                part.parse::<u64>()
                    .map_err(|_| format!("semantic version component overflow in `{raw}`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if numbers.len() != 3 {
            return Err(format!(
                "semantic version must have three components: `{raw}`"
            ));
        }
        let prerelease = prerelease
            .map(|pre| {
                pre.split('.')
                    .map(|part| {
                        if part.is_empty()
                            || !part
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        {
                            return Err(format!("invalid semantic prerelease `{raw}`"));
                        }
                        if part.bytes().all(|byte| byte.is_ascii_digit()) {
                            if part.len() > 1 && part.starts_with('0') {
                                return Err(format!("invalid numeric prerelease `{raw}`"));
                            }
                            Ok(PreIdentifier::Numeric(part.parse().map_err(|_| {
                                format!("semantic prerelease overflow in `{raw}`")
                            })?))
                        } else {
                            Ok(PreIdentifier::Text(part.to_owned()))
                        }
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?;
        Ok(Self {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
            prerelease,
        })
    }
}

impl Ord for SemVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| compare_prerelease(&self.prerelease, &other.prerelease))
    }
}

impl PartialOrd for SemVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(
    left: &Option<Vec<PreIdentifier>>,
    right: &Option<Vec<PreIdentifier>>,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            for (left, right) in left.iter().zip(right) {
                let ordering = match (left, right) {
                    (PreIdentifier::Numeric(left), PreIdentifier::Numeric(right)) => {
                        left.cmp(right)
                    }
                    (PreIdentifier::Numeric(_), PreIdentifier::Text(_)) => Ordering::Less,
                    (PreIdentifier::Text(_), PreIdentifier::Numeric(_)) => Ordering::Greater,
                    (PreIdentifier::Text(left), PreIdentifier::Text(right)) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
    }
}

fn compare_versions(left: &str, right: &str) -> Result<Ordering, String> {
    Ok(SemVersion::parse(left)?.cmp(&SemVersion::parse(right)?))
}

fn validate_range(range: &str) -> Result<(), String> {
    version_matches("0.0.0", range).map(|_| ())
}

fn version_matches(version: &str, range: &str) -> Result<bool, String> {
    version_matches_parsed(&SemVersion::parse(version)?, range)
}

fn version_matches_parsed(version: &SemVersion, range: &str) -> Result<bool, String> {
    let range = range.trim();
    if range == "*" {
        return Ok(version.prerelease.is_none());
    }
    if range.is_empty() || range.contains("||") {
        return Err(format!("unsupported version range `{range}`"));
    }
    if version.prerelease.is_some() && !range.contains('-') {
        return Ok(false);
    }
    for token in range.split([',', ' ']).filter(|token| !token.is_empty()) {
        let matches = if let Some(raw) = token.strip_prefix('^') {
            let lower = SemVersion::parse(raw)?;
            let upper = if lower.major > 0 {
                SemVersion {
                    major: lower.major + 1,
                    minor: 0,
                    patch: 0,
                    prerelease: None,
                }
            } else if lower.minor > 0 {
                SemVersion {
                    major: 0,
                    minor: lower.minor + 1,
                    patch: 0,
                    prerelease: None,
                }
            } else {
                SemVersion {
                    major: 0,
                    minor: 0,
                    patch: lower.patch + 1,
                    prerelease: None,
                }
            };
            version >= &lower && version < &upper
        } else if let Some(raw) = token.strip_prefix('~') {
            let lower = SemVersion::parse(raw)?;
            let upper = SemVersion {
                major: lower.major,
                minor: lower.minor + 1,
                patch: 0,
                prerelease: None,
            };
            version >= &lower && version < &upper
        } else {
            let (operator, raw) = [">=", "<=", ">", "<", "="]
                .into_iter()
                .find_map(|operator| token.strip_prefix(operator).map(|raw| (operator, raw)))
                .unwrap_or(("=", token));
            let expected = SemVersion::parse(raw)?;
            match operator {
                ">=" => version >= &expected,
                "<=" => version <= &expected,
                ">" => version > &expected,
                "<" => version < &expected,
                "=" => version == &expected,
                _ => unreachable!(),
            }
        };
        if !matches {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::pack_directory;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    const TEST_KEY: &str = "RWTE0ea94HIhauKyGQMuQ1Sn3uYtzoskNWDsv+MEE66Y2lzpofS4v7p3";

    #[test]
    fn signed_index_is_verified_before_parsing_and_time_checks() {
        let index = include_bytes!("../../tests/fixtures/market-index.json");
        let signature = include_bytes!("../../tests/fixtures/market-index.json.minisig");
        let key_file = include_str!("../../tests/fixtures/market-index.pub");
        let encoded = key_file.lines().nth(1).unwrap();
        let key = minisign_verify::PublicKey::from_base64(encoded).unwrap();
        let verified = verify_signed_index_with_key(index, signature, 3, &key).unwrap();
        assert_eq!(verified.market.id, MARKET_ID);

        let mut tampered = index.to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        let error = verify_signed_index_with_key(&tampered, signature, 3, &key).unwrap_err();
        assert!(error.contains("signature"), "{error}");

        let error = verify_signed_index_with_key(index, signature, 101, &key).unwrap_err();
        assert!(error.contains("expired"), "{error}");
    }

    fn publisher() -> MarketPublisher {
        MarketPublisher {
            id: "artec".into(),
            name: "Artec".into(),
            status: PublisherStatus::Trusted,
            review_url: "https://pi.at.cn/publishers/artec".into(),
            keys: vec![MarketPublisherKey {
                id: "release-1".into(),
                public_key: TEST_KEY.into(),
                status: PublisherKeyStatus::Active,
                not_before_unix: 1,
                not_after_unix: u64::MAX,
            }],
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("clat-market-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_public_key() -> minisign_verify::PublicKey {
        let key_file = include_str!("../../tests/fixtures/market-install-index.pub");
        minisign_verify::PublicKey::from_base64(key_file.lines().nth(1).unwrap()).unwrap()
    }

    fn respond(mut stream: TcpStream, body: &[u8], content_type: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    fn market_server(artifact: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = std::str::from_utf8(&request[..read]).unwrap();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                match path {
                    "/index.json" => respond(
                        stream,
                        include_bytes!("../../tests/fixtures/market-install-index.json"),
                        "application/json",
                    ),
                    "/index.json.minisig" => respond(
                        stream,
                        include_bytes!("../../tests/fixtures/market-install-index.json.minisig"),
                        "application/octet-stream",
                    ),
                    "/packages/market-package.clatpkg" => {
                        respond(stream, &artifact, "application/octet-stream")
                    }
                    other => panic!("unexpected test market request {other}"),
                }
            }
        });
        (format!("http://{address}/"), handle)
    }

    #[test]
    fn signed_loopback_index_downloads_and_atomically_installs_a_publisher_package() {
        let root = temp_dir("install-e2e");
        let bundle = root.join("fixture.clatpkg");
        pack_directory(Path::new("tests/fixtures/market-package"), &bundle).unwrap();
        let artifact = fs::read(&bundle).unwrap();
        assert_eq!(
            artifact.len(),
            1285,
            "fixture changed; regenerate signed index"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&artifact)),
            "703e5725c5bdc682b4e1d2b19a4a7a7ab775840b192791baf03547af786acf1f",
            "fixture changed; regenerate signed index"
        );
        let (base, server) = market_server(artifact.clone());
        let mut market = Market::load_with_key(&base, &test_public_key(), 3).unwrap();
        let mutations = market
            .install(
                &root.join("storage"),
                MarketInstallOptions {
                    root_id: "dev.clat.market-fixture".into(),
                    version: "*".into(),
                    config: None,
                    accept_capabilities: true,
                    accept_vulnerabilities: false,
                    root_kind: InstallKind::Install,
                },
            )
            .unwrap();
        server.join().unwrap();
        assert_eq!(mutations.len(), 1);
        let installed = super::super::installed_packages(&root.join("storage")).unwrap();
        assert_eq!(installed[0].id, "dev.clat.market-fixture");
        assert_eq!(installed[0].trust, TrustLabel::PublisherVerified);
        assert_eq!(installed[0].publisher.as_deref(), Some("test.publisher"));
        market.index.revocations.push(MarketRevocation {
            package: "dev.clat.market-fixture".into(),
            version: Some("1.0.0".into()),
            artifact_sha256: None,
            effective_at_unix: 1,
            reason: "acceptance revocation".into(),
        });
        let findings = market.audit_installed(&root.join("storage")).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.id == "MARKET-REVOKED")
        );

        let mut tampered = artifact;
        *tampered.last_mut().unwrap() ^= 1;
        let (base, server) = market_server(tampered);
        let market = Market::load_with_key(&base, &test_public_key(), 3).unwrap();
        let tampered_storage = root.join("tampered-storage");
        let error = market
            .install(
                &tampered_storage,
                MarketInstallOptions {
                    root_id: "dev.clat.market-fixture".into(),
                    version: "*".into(),
                    config: None,
                    accept_capabilities: true,
                    accept_vulnerabilities: false,
                    root_kind: InstallKind::Install,
                },
            )
            .unwrap_err();
        server.join().unwrap();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
        assert!(
            super::super::installed_packages(&tampered_storage)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn version(number: &str, dependencies: BTreeMap<String, String>) -> MarketVersion {
        MarketVersion {
            version: number.into(),
            runtime: PluginRuntimeKind::WasmComponent,
            publisher: "artec".into(),
            publisher_key: "release-1".into(),
            published_at_unix: 2,
            capabilities: PluginCapabilities::default(),
            dependencies,
            compatibility: MarketCompatibility::default(),
            yanked: false,
            artifacts: vec![MarketArtifact {
                target: "any".into(),
                url: "packages/test.clatpkg".into(),
                sha256: "0".repeat(64),
                bytes: 1,
            }],
        }
    }

    fn package(id: &str, versions: Vec<MarketVersion>) -> MarketPackage {
        MarketPackage {
            id: id.into(),
            name: id.into(),
            summary: "test".into(),
            description: String::new(),
            homepage: String::new(),
            tags: Vec::new(),
            versions,
        }
    }

    fn market(packages: Vec<MarketPackage>) -> Market {
        Market::from_index(
            "http://127.0.0.1:1/",
            MarketIndex {
                schema_version: 1,
                market: MarketMetadata {
                    id: MARKET_ID.into(),
                    name: "test".into(),
                    generated_at_unix: 1,
                    expires_at_unix: u64::MAX,
                    homepage: "https://pi.at.cn".into(),
                },
                publishers: vec![publisher()],
                packages,
                revocations: Vec::new(),
                vulnerabilities: Vec::new(),
            },
        )
    }

    #[test]
    fn range_subset_has_semver_precedence() {
        assert!(version_matches("1.8.4", "^1.2.0").unwrap());
        assert!(!version_matches("2.0.0", "^1.2.0").unwrap());
        assert!(version_matches("0.2.8", "^0.2.3").unwrap());
        assert!(!version_matches("0.3.0", "^0.2.3").unwrap());
        assert!(version_matches("1.2.9", ">=1.2.0 <2.0.0").unwrap());
        assert_eq!(
            compare_versions("1.0.0", "1.0.0-rc.1").unwrap(),
            Ordering::Greater
        );
        assert!(!version_matches("1.1.0-beta.1", "^1.0.0").unwrap());
        assert!(SemVersion::parse("1.0.0+local").is_err());
        assert!(validate_range("1.x").is_err());
    }

    #[test]
    fn solver_chooses_highest_compatible_and_dependencies_first() {
        let market = market(vec![
            package(
                "dev.root",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([("dev.dep".into(), "^1.0.0".into())]),
                )],
            ),
            package(
                "dev.dep",
                vec![
                    version("1.0.0", BTreeMap::new()),
                    version("1.4.0", BTreeMap::new()),
                ],
            ),
        ]);
        let selected = market.solve("dev.root", "*").unwrap();
        assert_eq!(selected[0].package.id, "dev.dep");
        assert_eq!(selected[0].version.version, "1.4.0");
        assert_eq!(selected[1].package.id, "dev.root");
    }

    #[test]
    fn solver_rejects_cycles_and_conflicts() {
        let cycle = market(vec![
            package(
                "dev.a",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([("dev.b".into(), "*".into())]),
                )],
            ),
            package(
                "dev.b",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([("dev.a".into(), "*".into())]),
                )],
            ),
        ]);
        assert!(cycle.solve("dev.a", "*").unwrap_err().contains("cycle"));

        let conflict = market(vec![
            package(
                "dev.root",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([
                        ("dev.left".into(), "*".into()),
                        ("dev.right".into(), "*".into()),
                    ]),
                )],
            ),
            package(
                "dev.left",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([("dev.dep".into(), "^1.0.0".into())]),
                )],
            ),
            package(
                "dev.right",
                vec![version(
                    "1.0.0",
                    BTreeMap::from([("dev.dep".into(), "^2.0.0".into())]),
                )],
            ),
            package(
                "dev.dep",
                vec![
                    version("1.0.0", BTreeMap::new()),
                    version("2.0.0", BTreeMap::new()),
                ],
            ),
        ]);
        assert!(
            conflict
                .solve("dev.root", "*")
                .unwrap_err()
                .contains("conflict")
        );
    }

    #[test]
    fn revoked_key_and_artifact_are_not_installable() {
        let mut market = market(vec![package(
            "dev.root",
            vec![version("1.0.0", BTreeMap::new())],
        )]);
        market.index.revocations.push(MarketRevocation {
            package: "dev.root".into(),
            version: Some("1.0.0".into()),
            artifact_sha256: None,
            effective_at_unix: 1,
            reason: "compromised".into(),
        });
        assert!(market.solve("dev.root", "*").is_err());
        market.index.revocations.clear();
        market.index.publishers[0].keys[0].status = PublisherKeyStatus::Revoked;
        assert!(market.solve("dev.root", "*").is_err());
    }
}
