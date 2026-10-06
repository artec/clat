//! Presentation and input ownership only; all mutations use host client ports.
use super::*;
use crate::host::PluginReviewTicket;
use serde_json::Value;
mod input;
mod render;
#[cfg(all(test, feature = "runtime-tests"))]
mod style_tests;
#[cfg(test)]
mod tests;
mod transport;

#[derive(Default)]
pub(super) struct PluginUi {
    pub(super) review_terminal_area: Option<Rect>,
    pub(super) dialog: Option<PluginDialog>,
    pub(super) request_serial: u64,
    pub(super) jobs: Vec<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Installed,
    Market,
    Review,
    Config,
    Remove,
}

pub(super) struct PluginDialog {
    view: View,
    installed: Vec<Value>,
    market: Vec<Value>,
    query: InputBuffer,
    searching: bool,
    selected: usize,
    market_generation: u32,
    review: Option<PluginReviewTicket>,
    accepted: bool,
    config: InputBuffer,
    scroll: usize,
    reviewed: usize,
    total: usize,
    review_width: usize,
    page: usize,
    pending: bool,
    status: String,
    request: u64,
    remove: Option<(String, bool)>,
    expires: Option<Instant>,
}

impl PluginDialog {
    fn new() -> Self {
        Self {
            view: View::Installed,
            installed: vec![],
            market: vec![],
            query: InputBuffer::default(),
            searching: false,
            selected: 0,
            market_generation: 1,
            review: None,
            accepted: false,
            config: InputBuffer::default(),
            scroll: 0,
            reviewed: 0,
            total: 0,
            review_width: 0,
            page: 1,
            pending: false,
            status: "Loading installed plugins…".into(),
            request: 0,
            remove: None,
            expires: None,
        }
    }
    fn rows(&self) -> Vec<&Value> {
        let query = self.query.text().to_lowercase();
        let rows = if self.view == View::Market {
            &self.market
        } else {
            &self.installed
        };
        rows.iter()
            .filter(|row| {
                ["name", "id", "summary"]
                    .iter()
                    .any(|key| label(row, key).to_lowercase().contains(&query))
            })
            .collect()
    }
    fn selected(&self) -> Option<Value> {
        self.rows().get(self.selected).map(|v| (*v).clone())
    }
    fn clear_review(&mut self) {
        self.review = None;
        self.config.clear();
        self.accepted = false;
        self.scroll = 0;
        self.reviewed = 0;
        self.total = 0;
        self.review_width = 0;
        self.expires = None;
    }
    fn expire(&mut self) {
        if self
            .expires
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.clear_review();
            self.view = View::Installed;
            self.status = "Review expired; prepare again".into();
        }
    }
    fn can_commit(&self) -> bool {
        // Consent gate is exercised independently of the host's second gate.
        self.review.is_some()
            && !self.pending
            && self.accepted
            && self.total > 0
            && self.reviewed >= self.total
            && self
                .expires
                .is_none_or(|deadline| Instant::now() < deadline)
    }
}

fn label(value: &Value, key: &str) -> String {
    value[key]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect()
}

pub(super) enum PluginReply {
    List(Result<Value, String>),
    Market(Result<Value, String>),
    Review(Result<PluginReviewTicket, String>),
    Mutation(Result<Value, String>),
}

impl App {
    pub(super) fn open_plugin_dialog(&mut self) {
        if self.native.is_none() {
            self.flash_status("Plugin management uses the shared CLAT host; restart with clat (not standalone/DSH)");
            return;
        }
        self.close_plugin_dialog();
        self.plugins.dialog = Some(PluginDialog::new());
        self.request_plugin_list(false);
    }
    pub(super) fn plugin_repaint_deadline(&self) -> Option<Instant> {
        self.plugins
            .dialog
            .as_ref()
            .and_then(|d| d.expires)
            .map(|at| at.max(Instant::now() + Duration::from_millis(1)))
    }
    pub(super) fn close_plugin_dialog(&mut self) {
        self.plugins.dialog = None;
    }
}
