//! Downloaded, verified proposals. No executable starts before commit.
use super::*;
use std::time::Instant;

pub(crate) struct PreparedMarketInstall {
    transaction: PathBuf,
    pub(super) index: MarketIndex,
    selections: Vec<MarketSelection>,
    requests: Vec<PackageInstallRequest>,
    pub(super) inspections: Vec<crate::plugin::PackageInspection>,
    revision: String,
    root_id: String,
    started: Instant,
}

impl Drop for PreparedMarketInstall {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.transaction);
    }
}

impl PreparedMarketInstall {
    pub(crate) fn review(&self) -> serde_json::Value {
        serde_json::json!({"packages": self.inspections.iter().map(|inspection| {
            let manifest = &inspection.manifest;
            serde_json::json!({"id": manifest.id, "name": manifest.name,
                "version": manifest.version, "runtime": manifest.runtime.kind,
                "capabilities": manifest.capabilities, "config_schema": manifest.config_schema,
                "tree_sha256": inspection.tree_sha256, "publisher": inspection.publisher.as_ref().map(|p| &p.publisher)})
        }).collect::<Vec<_>>()})
    }

    pub(crate) fn expired(&self) -> bool {
        self.started.elapsed() > MAX_INSTALL_WALL_TIME
    }

    pub(crate) fn commit(
        &mut self,
        store: &mut PackageStore,
        config: Option<serde_json::Value>,
        accepted: bool,
    ) -> Result<Vec<PackageMutation>, String> {
        let configs = config
            .map(|value| BTreeMap::from([(self.root_id.clone(), value)]))
            .unwrap_or_default();
        self.commit_configs(store, configs, accepted)
    }

    pub(crate) fn commit_configs(
        &mut self,
        store: &mut PackageStore,
        configs: BTreeMap<String, serde_json::Value>,
        accepted: bool,
    ) -> Result<Vec<PackageMutation>, String> {
        if self.expired() {
            return Err("installation review expired; prepare again".into());
        }
        validate_index(&self.index, now_unix()?)?;
        if self.revision != store.revision() {
            return Err("installed plugins changed; prepare and review again".into());
        }
        for request in &self.requests {
            let inspection = PackageStore::inspect(&request.path)?;
            let reviewed = self
                .inspections
                .iter()
                .find(|i| i.manifest.id == inspection.manifest.id)
                .ok_or("package was not reviewed")?;
            if inspection.tree_sha256 != reviewed.tree_sha256 {
                return Err("prepared artifact changed; review again".into());
            }
            let selection = self
                .selections
                .iter()
                .find(|s| s.package.id == inspection.manifest.id)
                .ok_or("prepared package is not in the reviewed solution")?;
            validate_downloaded_package(&self.index, selection, &inspection, now_unix()?)?;
        }
        let requests = self
            .requests
            .iter()
            .map(|request| PackageInstallRequest {
                path: request.path.clone(),
                kind: request.kind,
                accept_capabilities: accepted,
                config: request
                    .path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .and_then(|id| {
                        configs.get(id).cloned().or_else(|| {
                            if request.kind == InstallKind::Update {
                                store.configuration(id)
                            } else {
                                None
                            }
                        })
                    }),
            })
            .collect();
        store.install_batch(requests)
    }
}

impl Market {
    pub(crate) fn prepare(
        &self,
        root: &Path,
        store: &PackageStore,
        options: &MarketInstallOptions,
    ) -> Result<PreparedMarketInstall, String> {
        validate_index(&self.index, now_unix()?)?;
        let selections = self.solve(&options.root_id, &options.version)?;
        let installed = store
            .list()
            .into_iter()
            .map(|p| (p.id, p.version))
            .collect::<BTreeMap<_, _>>();
        match (options.root_kind, installed.contains_key(&options.root_id)) {
            (InstallKind::Install, true) => {
                return Err("plugin is already installed; use update".into());
            }
            (InstallKind::Update, false) => {
                return Err("plugin is not installed; use install".into());
            }
            _ => {}
        }
        self.check_advisories(&selections, options.accept_vulnerabilities)?;
        let parent = root.join("plugin-market-staging");
        create_private_dir(&parent)?;
        let transaction = parent.join(uuid::Uuid::new_v4().to_string());
        create_private_dir(&transaction)?;
        let mut prepared = PreparedMarketInstall {
            transaction,
            index: self.index.clone(),
            selections,
            requests: Vec::new(),
            inspections: Vec::new(),
            revision: store.revision(),
            root_id: options.root_id.clone(),
            started: Instant::now(),
        };
        for selection in &prepared.selections {
            if prepared.expired() {
                return Err("market download exceeded transaction limit".into());
            }
            if installed.get(&selection.package.id) == Some(&selection.version.version) {
                continue;
            }
            let bundle = prepared
                .transaction
                .join(format!("{}.clatpkg", selection.package.id));
            self.download_artifact(&selection.artifact, &bundle)?;
            let path = prepared.transaction.join(&selection.package.id);
            unpack_bundle(&bundle, &path)?;
            let inspection = PackageStore::inspect(&path)?;
            validate_downloaded_package(&self.index, selection, &inspection, now_unix()?)?;
            prepared.inspections.push(inspection);
            prepared.requests.push(PackageInstallRequest {
                path,
                config: None,
                accept_capabilities: false,
                kind: if installed.contains_key(&selection.package.id) {
                    InstallKind::Update
                } else {
                    InstallKind::Install
                },
            });
        }
        if prepared.requests.is_empty() {
            return Err("selected versions are already installed".into());
        }
        Ok(prepared)
    }

    fn check_advisories(&self, selections: &[MarketSelection], accept: bool) -> Result<(), String> {
        if accept {
            return Ok(());
        }
        for selection in selections {
            for advisory in &self.index.vulnerabilities {
                if advisory.package == selection.package.id
                    && version_matches(&selection.version.version, &advisory.affected)?
                {
                    return Err(format!(
                        "known vulnerability {} blocks installation; review with the CLI",
                        advisory.id
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_update_preserves_private_config_when_blank_and_replaces_when_supplied() {
        let (root, mut store, mut prepared) =
            super::super::tests::prepared_fixture("update-config-retention");
        let id = "dev.clat.market-fixture";
        let old = serde_json::json!({"credential":"old-private-value", "setting":"keep"});
        store
            .install(
                &prepared.requests[0].path,
                Some(old.clone()),
                true,
                InstallKind::Install,
            )
            .unwrap();
        prepared.requests[0].kind = InstallKind::Update;
        prepared.revision = store.revision();
        prepared
            .commit_configs(&mut store, BTreeMap::new(), true)
            .unwrap();
        assert_eq!(
            store.configuration(id),
            Some(old),
            "blank update must retain secrets"
        );
        prepared.revision = store.revision();
        let new = serde_json::json!({"credential":"new-private-value"});
        prepared
            .commit_configs(&mut store, BTreeMap::from([(id.into(), new.clone())]), true)
            .unwrap();
        assert_eq!(
            store.configuration(id),
            Some(new),
            "entered config replaces the complete object"
        );
        drop(prepared);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
