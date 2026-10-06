//! Plugin management transport. Review ownership survives UI cancellation and
//! late responses; configuration values never become display/error content.
use super::HostClient;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::thread::JoinHandle;
static CANCELLATIONS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

pub struct PluginReviewTicket {
    client: HostClient,
    ticket: Option<String>,
    pub review: Value,
}

impl PluginReviewTicket {
    pub fn commit(&mut self, accepted: bool, configs: &str) -> Result<Value, String> {
        if !accepted {
            return Err("Explicit capability approval is required".into());
        }
        if configs.len() > 64 * 1024 {
            return Err("Configuration exceeds 64 KiB".into());
        }
        let configs: BTreeMap<String, Value> = serde_json::from_str(configs)
            .map_err(|_| "Configuration must be a JSON object keyed by plugin ID")?;
        let ticket = self
            .ticket
            .take()
            .ok_or("Review already submitted; prepare again")?;
        // A lost acknowledgement is uncertain, never an automatic retry.
        self.client.call("plugin.commit", &json!({"ticket":ticket,
            "accept_capabilities":accepted,"configs":configs}))
            .map_err(|_| "Activation failed or acknowledgement lost; refresh installed plugins and prepare again".into())
    }
}

impl Drop for PluginReviewTicket {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            let client = self.client.clone();
            if let Ok(job) = std::thread::Builder::new()
                .name("plugin-review-cancel".into())
                .spawn(move || {
                    let _ = client.call("plugin.cancel", &json!({"ticket":ticket}));
                })
            {
                let mut jobs = CANCELLATIONS.lock().expect("plugin cleanup");
                jobs.retain(|job| !job.is_finished());
                jobs.push(job);
            }
        }
    }
}

impl HostClient {
    /// Graceful terminal exit drains cancellation work before process teardown.
    pub fn finish_plugin_review_cleanup() {
        let jobs = std::mem::take(&mut *CANCELLATIONS.lock().expect("plugin cleanup"));
        for job in jobs {
            let _ = job.join();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn plugin_review_fixture_for_tests(&self, review: Value) -> PluginReviewTicket {
        PluginReviewTicket {
            client: self.clone(),
            ticket: None,
            review,
        }
    }

    pub fn plugin_list(&self) -> Result<Value, String> {
        self.call("plugin.list", &json!({}))
    }
    pub fn plugin_market(&self, generation: u32) -> Result<Value, String> {
        self.call("plugin.market", &json!({"market_generation":generation}))
    }
    pub fn plugin_review(
        &self,
        id: &str,
        action: &str,
        generation: u32,
    ) -> Result<PluginReviewTicket, String> {
        // Establish cancellation identity before network I/O, including lost
        // prepare acknowledgements. Old callers may still omit this field.
        let ticket = uuid::Uuid::new_v4().to_string();
        let mut owned = PluginReviewTicket {
            client: self.clone(),
            ticket: Some(ticket.clone()),
            review: Value::Null,
        };
        let review = self.call(
            "plugin.prepare",
            &json!({"id":id,"action":action,"market_generation":generation,"ticket":ticket}),
        )?;
        owned.ticket = Some(
            review["ticket"]
                .as_str()
                .ok_or("Host returned no review ticket")?
                .to_owned(),
        );
        owned.review = review;
        Ok(owned)
    }

    pub fn plugin_remove(&self, id: &str, uninstall: bool) -> Result<Value, String> {
        self.call(
            "plugin.remove",
            &json!({"id":id,"action":if uninstall {"uninstall"} else {"disable"}}),
        )
    }
}

#[cfg(test)]
mod tests;
