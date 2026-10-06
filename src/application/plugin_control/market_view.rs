//! Verified read-only catalog projection for host clients. Package activation
//! remains prepare/review/commit; display metadata grants no capability.
use super::*;

impl HostApplication {
    pub fn plugin_market_listing(&self, generation: u32) -> Result<Value, ApplicationError> {
        let market = self.plugin_market(generation)?;
        let packages: Vec<Value> = market
            .search("")
            .into_iter()
            .map(|package| {
                let mut view = json!({"id":package.id,"name":package.name,"summary":package.summary,
                "manifest_version":generation,"health":"available"});
                match market.latest(&package.id) {
                    Ok(selected) => {
                        view["version"] = json!(selected.version.version);
                        view["runtime"] = json!(selected.version.runtime);
                        view["publisher"] = json!(selected.version.publisher);
                    }
                    Err(error) => view["health"] = json!(error),
                }
                view
            })
            .collect();
        Ok(json!({"packages":packages,"market_generation":generation}))
    }
}
