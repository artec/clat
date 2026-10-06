//! Host-owned plugin installation control plane; never a journal producer.
use super::{ApplicationError, HostApplication, TrustedProjectApplication};
use crate::plugin::{
    DEFAULT_MARKET_URL, InstallKind, Market, MarketInstallOptions, PackageStore,
    PreparedMarketInstall,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
mod market_view;
#[cfg(test)]
mod tests;

pub(super) struct PluginReview {
    pub(super) action: String,
    id: String,
    revision: String,
    started: Instant,
    market: Option<PreparedMarketInstall>,
}

impl HostApplication {
    fn plugin_market(&self, generation: u32) -> Result<std::sync::Arc<Market>, ApplicationError> {
        #[cfg(test)]
        if let Some(source) = self
            .storage
            .plugin_market_fixture
            .lock()
            .expect("test market")
            .as_ref()
        {
            return Ok(source.clone());
        }
        let url = match generation {
            1 => DEFAULT_MARKET_URL,
            2 => "https://pi.at.cn/v2/",
            _ => {
                return Err(ApplicationError::new(
                    "unsupported plugin market generation",
                ));
            }
        };
        Market::load(self.storage.root(), url)
            .map(std::sync::Arc::new)
            .map_err(ApplicationError::new)
    }

    fn plugin_store(&self) -> Result<PackageStore, ApplicationError> {
        PackageStore::open_in_host(self.storage.root(), self.storage.lease.clone())
            .map_err(ApplicationError::new)
    }

    /// Safe projections deliberately omit all configuration values.
    pub fn plugin_list(&self) -> Result<Value, ApplicationError> {
        let packages = crate::plugin::installed_packages(self.storage.root())
            .map_err(ApplicationError::new)?;
        Ok(
            json!({"installed": packages.into_iter().map(|p| json!({"id": p.id, "name": p.name,
            "version": p.version, "manifest_version": p.manifest_version, "runtime": p.runtime, "enabled": p.enabled,
            "rollback_version": p.rollback_version, "trust": p.trust, "publisher": p.publisher,
            "health": p.health})).collect::<Vec<_>>()}),
        )
    }

    pub fn plugin_prepare(&mut self, id: &str, action: &str) -> Result<Value, ApplicationError> {
        self.plugin_prepare_generation(id, action, 1)
    }

    pub fn plugin_prepare_generation(
        &mut self,
        id: &str,
        action: &str,
        generation: u32,
    ) -> Result<Value, ApplicationError> {
        self.plugin_prepare_ticket(id, action, generation, None)
    }

    pub fn plugin_prepare_ticket(
        &mut self,
        id: &str,
        action: &str,
        generation: u32,
        requested: Option<&str>,
    ) -> Result<Value, ApplicationError> {
        let ticket = requested
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if uuid::Uuid::parse_str(&ticket).is_err() || self.plugin_reviews.contains_key(&ticket) {
            return Err(ApplicationError::new(
                "invalid or duplicate plugin review request",
            ));
        }
        self.reject_busy_plugin_projects()?;
        self.prune_plugin_reviews();
        if self.plugin_reviews.len() >= 8 {
            return Err(ApplicationError::new(
                "too many pending plugin reviews; cancel one",
            ));
        }
        let store = self.plugin_store()?;
        let (view, market) = self.plugin_review_view(&store, id, action, generation)?;
        self.plugin_reviews.insert(
            ticket.clone(),
            PluginReview {
                action: action.into(),
                id: id.into(),
                revision: store.revision(),
                started: Instant::now(),
                market,
            },
        );
        Ok(json!({"ticket": ticket, "action": action, "review": view, "expires_in_seconds": 900}))
    }

    fn plugin_review_view(
        &self,
        store: &PackageStore,
        id: &str,
        action: &str,
        generation: u32,
    ) -> Result<(Value, Option<PreparedMarketInstall>), ApplicationError> {
        let mut market = None;
        let view = match action {
            "install" | "update" => {
                let source = self.plugin_market(generation)?;
                let prepared = source
                    .prepare(
                        self.storage.root(),
                        store,
                        &MarketInstallOptions {
                            root_id: id.to_owned(),
                            version: "*".into(),
                            config: None,
                            accept_capabilities: false,
                            accept_vulnerabilities: false,
                            root_kind: if action == "install" {
                                InstallKind::Install
                            } else {
                                InstallKind::Update
                            },
                        },
                    )
                    .map_err(ApplicationError::new)?;
                let view = prepared.review();
                market = Some(prepared);
                view
            }
            "enable" | "rollback" | "configure" => {
                let manifest = store
                    .review_manifest(id, action == "rollback")
                    .map_err(ApplicationError::new)?;
                json!({"packages": [{"id": manifest.id, "name": manifest.name, "version": manifest.version,
                    "runtime": manifest.runtime.kind, "capabilities": manifest.capabilities,
                    "publisher": store.publisher(id),
                    "config_schema": if action == "configure" { manifest.config_schema } else { None }}]})
            }
            _ => return Err(ApplicationError::new("unknown plugin review action")),
        };
        Ok((view, market))
    }

    pub fn plugin_cancel(&mut self, ticket: &str) {
        self.plugin_reviews.remove(ticket);
        self.prune_plugin_reviews();
    }

    fn prune_plugin_reviews(&mut self) {
        self.plugin_reviews
            .retain(|_, review| review.started.elapsed() < Duration::from_secs(900));
    }

    fn reject_busy_plugin_projects(&self) -> Result<(), ApplicationError> {
        for app in self.projects.values() {
            reject_busy_plugin_project(&app.lock().expect("application lock"))?;
        }
        Ok(())
    }

    pub fn plugin_commit(
        &mut self,
        ticket: &str,
        accepted: bool,
        configs: BTreeMap<String, Value>,
    ) -> Result<Value, ApplicationError> {
        if !accepted {
            return Err(ApplicationError::new(
                "explicit plugin capability consent is required",
            ));
        }
        self.prune_plugin_reviews();
        let mut review = self
            .plugin_reviews
            .remove(ticket)
            .ok_or_else(|| ApplicationError::new("plugin review expired or already consumed"))?;
        let mut store = self.plugin_store()?;
        if review.revision != store.revision() {
            return Err(ApplicationError::new(
                "installed plugins changed; review again",
            ));
        }
        self.with_idle_plugin_runtimes(|| {
            let mutations = if let Some(prepared) = &mut review.market {
                prepared
                    .commit_configs(&mut store, configs, true)
                    .map_err(ApplicationError::new)?
            } else {
                let mutation = match review.action.as_str() {
                    "enable" => store.set_enabled(&review.id, true),
                    "rollback" => store.rollback(&review.id),
                    "configure" => store.configure(
                        &review.id,
                        configs
                            .get(&review.id)
                            .cloned()
                            .ok_or_else(|| ApplicationError::new("configuration is required"))?,
                    ),
                    _ => Err("invalid plugin action".into()),
                }
                .map_err(ApplicationError::new)?;
                vec![mutation]
            };
            Ok(
                json!({"committed": true, "packages": mutations.iter().map(|m| json!({"id": m.id,
                "version": m.version, "enabled": m.enabled, "note": m.note})).collect::<Vec<_>>()}),
            )
        })
    }

    pub fn plugin_remove(&mut self, id: &str, uninstall: bool) -> Result<Value, ApplicationError> {
        let mut store = self.plugin_store()?;
        self.with_idle_plugin_runtimes(|| {
            let mutation = if uninstall {
                store.uninstall(id)
            } else {
                store.set_enabled(id, false)
            }
            .map_err(ApplicationError::new)?;
            Ok(json!({"committed": true, "id": mutation.id, "note": mutation.note}))
        })
    }

    fn with_idle_plugin_runtimes(
        &self,
        mutate: impl FnOnce() -> Result<Value, ApplicationError>,
    ) -> Result<Value, ApplicationError> {
        // Sorted project locks are held across revoke, publication and remount.
        // A run admitted by any frontend cannot race this interval.
        let mut apps = self
            .projects
            .values()
            .map(|app| app.lock().expect("application lock"))
            .collect::<Vec<_>>();
        for app in &apps {
            reject_busy_plugin_project(app)?;
            if !app.mcp_status.is_settled() {
                return Err(ApplicationError::new(
                    "plugins are still starting; retry when startup settles",
                ));
            }
        }
        let stop_errors = apps
            .iter_mut()
            .filter_map(|app| {
                app.composition
                    .stop_external_plugins()
                    .err()
                    .map(|e| e.to_string())
            })
            .collect::<Vec<_>>();
        let result = if stop_errors.is_empty() {
            mutate()
        } else {
            Err(ApplicationError::new(stop_errors.join("; ")))
        };
        let mut failures = apps
            .iter_mut()
            .filter_map(|app| app.reload_external_plugins().err().map(|e| e.to_string()))
            .collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_secs(20);
        for app in &apps {
            if !app
                .mcp_status
                .wait_until_settled(deadline.saturating_duration_since(Instant::now()))
            {
                failures.push("plugin startup is still pending; inspect MCP status".into());
            }
            failures.extend(app.mcp_status.snapshot().failures);
        }
        let mut value = result?;
        value["runtime_warnings"] = json!(failures);
        Ok(value)
    }
}

fn reject_busy_plugin_project(app: &TrustedProjectApplication) -> Result<(), ApplicationError> {
    app.reject_session_switch_while_busy().map_err(|_| {
        ApplicationError::new("finish active runs or compaction before managing plugins")
    })
}

impl TrustedProjectApplication {
    #[cfg(test)]
    pub(crate) fn set_plugin_market_fixture(&self, market: Market) {
        *self
            .host_storage
            .plugin_market_fixture
            .lock()
            .expect("test market") = Some(std::sync::Arc::new(market));
    }

    fn reload_external_plugins(&mut self) -> Result<(), ApplicationError> {
        self.mcp_status = self.composition.restart_external_plugins(
            self.host_storage.root().to_owned(),
            &self.project,
            &self.control,
            &self.plugin_host,
            self.permission_modes_enabled
                .then(|| self.permission_mode.clone()),
        )?;
        let subscribers = self.subscribers.clone();
        self.mcp_status
            .set_notice_sink(std::sync::Arc::new(move |failures| {
                super::broadcast_to(
                    &subscribers,
                    super::ApplicationEvent::McpStartupNotice { failures },
                );
            }));
        Ok(())
    }
}
