use super::{HostClient, HostEvents, transport};
use crate::native_tools::ui_activity::{ComputerUseSnapshot, ComputerUseTarget};
use std::time::Duration;

impl HostClient {
    pub fn computer_use_events(&self) -> Result<HostEvents, String> {
        let mut root = self.clone();
        root.prefix.clear();
        let response = transport::request(&root, "GET", "/api/computer-use/events", &[])?;
        response
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|error| error.to_string())?;
        Ok(HostEvents::new(response))
    }

    pub fn stop_computer_use(&self, target: &ComputerUseTarget) -> Result<bool, String> {
        let mut root = self.clone();
        root.prefix.clear();
        let body = serde_json::to_vec(target).map_err(|error| error.to_string())?;
        let mut response = transport::request(&root, "POST", "/api/computer-use/stop", &body)?;
        let value: serde_json::Value =
            serde_json::from_slice(&transport::read_body(&mut response)?)
                .map_err(|error| error.to_string())?;
        value["stopped"]
            .as_bool()
            .ok_or_else(|| "invalid computer use stop receipt".into())
    }
}

impl HostEvents {
    pub fn next_computer_use(&mut self) -> Result<ComputerUseSnapshot, String> {
        let (kind, value) = self.next_frame()?;
        if kind != "computer-use" {
            return Err("invalid computer use event".into());
        }
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}
