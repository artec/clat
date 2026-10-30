//! Native clients consume the authenticated host protocol, never session storage.
//! Client-owned clipboard sources use an isolated ephemeral draft directory.
//! Every mutation is sent once. A transport failure is an uncertain outcome,
//! not permission to retry or to open another local writer.
mod attachments;
pub(crate) mod browser;
pub(crate) mod build_identity;
mod commands;
mod plugins;
pub use plugins::PluginReviewTicket;
mod context;
mod credentials;
pub(crate) mod discovery;
mod error;
mod history;
mod info;
mod lifecycle;
pub use lifecycle::HostManagementArgs;
pub(crate) mod backend;
mod models;
mod sessions;
mod startup;
pub use backend::PackagedBackend;
mod desktop;
mod reconcile;
pub use reconcile::BackendConnection;
mod notification;
mod workspace;
pub use error::HostCallError;
pub use models::HostModelChoices;
pub use notification::HostNotification;
mod events;
mod transport;
pub use events::{HostEvent, decode_host_approval, decode_host_event};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub use transport::{HostEvents, HostEventsInterrupt};

pub const HOST_PROTOCOL_VERSION: u64 = 1;
/// Stable loopback port used by automatic background-host startup.
pub const DEFAULT_HOST_PORT: u16 = 2691;

#[derive(Clone)]
pub(crate) enum BuildMatchPolicy {
    RequireIfAdvertised,
    Ignore,
    Backend { fingerprint: String, official: bool },
}

#[derive(Clone)]
pub struct HostClient {
    port: u16,
    token: String,
    prefix: String,
    instance: String,
    root: PathBuf,
    clipboard: std::sync::Arc<crate::draft::DraftImageStore>,
}

impl HostClient {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn fixture_for_frontend_tests(root: &Path) -> Self {
        let root = root.canonicalize().expect("fixture storage root");
        Self {
            port: 1,
            token: "frontend-fixture".into(),
            prefix: String::new(),
            instance: "11111111-1111-1111-1111-111111111111".into(),
            clipboard: std::sync::Arc::new(crate::draft::DraftImageStore::new(&root)),
            root,
        }
    }

    pub fn submit_text(
        &self,
        text: &str,
        running: bool,
        selection: u64,
    ) -> Result<Value, HostCallError> {
        let call = |method: &str, mut params: Value| {
            params["expected_selection_generation"] = json!(selection);
            self.call_with_receipt(method, &params)
        };
        match text.trim() {
            "/new" | "/clear" => call("session.new", json!({})),
            "/cancel" => call("run.cancel", json!({})),
            "/compact" => call("session.compact", json!({"action":"start"})),
            "/compact cancel" => call("session.compact", json!({"action":"cancel"})),
            "/model" => Err("Use Workbench settings → Models in the PWA; terminal profile editor is not attached yet".into()),
            text if text.starts_with("/resume ") => call("session.switch", json!({"id":text[8..].trim()})),
            "/resume" => self.call_with_receipt("session.list", &json!({})),
            text if text.starts_with('/') => call("command.run", json!({"command":text})),
            text => call(if running { "steer.send" } else { "prompt.send" },
                json!({"text":text, "clientMessageId":uuid::Uuid::new_v4().to_string()})),
        }
    }
    pub fn connect_local(port: u16) -> Result<Self, String> {
        Self::connect_local_presented(port).map_err(|e| e.raw)
    }
    pub fn connect_local_presented(port: u16) -> Result<Self, crate::i18n::PresentedText> {
        let root = crate::control_storage::sentinel::default_storage_root()?;
        let token = credentials::read_token(&root)?;
        Self::connect_checked(
            port,
            token,
            &root,
            None,
            BuildMatchPolicy::RequireIfAdvertised,
        )
        .map_err(discovery::DiscoveryFailure::presented)
    }
    /// Read-only CLI queries must select an already mounted project, never the
    /// Host's bootstrap project or an implicitly trusted/newly mounted project.
    pub fn connect_current_project(
        port: u16,
        locale: crate::i18n::LocaleContext,
    ) -> Result<Self, String> {
        let project = crate::Project::current().map_err(|e| e.to_string())?;
        Self::connect_local_presented(port)
            .map_err(|e| e.render(locale))?
            .for_mounted_project(project.root(), locale)
    }
    pub fn for_mounted_project(
        &self,
        root: &Path,
        locale: crate::i18n::LocaleContext,
    ) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let projects = self.call("workspace.list", &json!({}))?;
        let id = projects["workspaces"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|r| r["root"].as_str().is_some_and(|p| Path::new(p) == root))
            })
            .and_then(|r| r["id"].as_str())
            .ok_or_else(|| locale.text("host.project_not_mounted").to_owned())?;
        self.with_project_route(id)
    }
    pub fn connect(port: u16, token: String, expected_root: &Path) -> Result<Self, String> {
        Self::connect_checked(
            port,
            token,
            expected_root,
            None,
            BuildMatchPolicy::RequireIfAdvertised,
        )
        .map_err(discovery::DiscoveryFailure::message)
    }

    fn connect_for_management(
        port: u16,
        token: String,
        expected_root: &Path,
    ) -> Result<Self, crate::i18n::PresentedText> {
        Self::connect_checked(port, token, expected_root, None, BuildMatchPolicy::Ignore)
            .map_err(discovery::DiscoveryFailure::presented)
    }

    fn connect_checked(
        port: u16,
        token: String,
        expected_root: &Path,
        expected_instance: Option<&str>,
        build_policy: BuildMatchPolicy,
    ) -> Result<Self, discovery::DiscoveryFailure> {
        if port == 0
            || token.is_empty()
            || token.len() > 256
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        {
            return Err("invalid host connection parameters".into());
        }
        let root = expected_root
            .canonicalize()
            .map_err(|_| "storage root is unavailable")?;
        let mut client = Self {
            port,
            token,
            prefix: String::new(),
            instance: String::new(),
            clipboard: std::sync::Arc::new(crate::draft::DraftImageStore::new(&root)),
            root,
        };
        let description = client
            .call_with_receipt("host.describe", &json!({}))
            .map_err(|e| discovery::DiscoveryFailure::from(e.presented()))?;
        client.instance = description["instance_id"]
            .as_str()
            .unwrap_or_default()
            .into();
        if uuid::Uuid::parse_str(&client.instance).is_err() {
            return Err("host did not identify its instance".into());
        }
        if expected_instance.is_some_and(|instance| instance != client.instance) {
            return Err("host endpoint instance changed; reconnect explicitly".into());
        }
        client.verify_description(&description)?;
        match build_policy {
            BuildMatchPolicy::RequireIfAdvertised => {
                build_identity::verify_advertised(&description)?
            }
            BuildMatchPolicy::Backend {
                fingerprint,
                official,
            } => build_identity::verify_for(&description, &fingerprint, official)?,
            BuildMatchPolicy::Ignore => {}
        }
        Ok(client)
    }

    fn verify_description(&self, description: &Value) -> Result<(), discovery::DiscoveryFailure> {
        // Compatibility is authoritative only after storage and instance identity.
        let root = description["storage_root"]
            .as_str()
            .ok_or("host storage identity is missing")?;
        if Path::new(root).canonicalize().ok().as_ref() != Some(&self.root) {
            return Err("host belongs to a different storage root".into());
        }
        if description["protocol_version"] != HOST_PROTOCOL_VERSION
            || description["wire_version"] != crate::wire::WIRE_VERSION
            || description["journal_version"] != crate::session::compat::SESSION_FORMAT_VERSION
        {
            return Err(discovery::DiscoveryFailure::Incompatible(
                crate::i18n::UiMessage::new("host.incompatible_protocol").into(),
            ));
        }
        Ok(())
    }

    pub fn instance_id(&self) -> &str {
        &self.instance
    }

    pub fn storage_root(&self) -> &Path {
        &self.root
    }

    /// Ephemeral client-owned sources only; never opens a session writer.
    /// Worker clones share ownership until their last reference is released.
    pub fn clipboard_drafts(&self) -> std::sync::Arc<crate::draft::DraftImageStore> {
        self.clipboard.clone()
    }

    pub fn open_project(&self, root: &Path, trust: bool) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|_| "project directory is unavailable")?;
        let opened = self.call("workspace.open", &json!({"root":root, "trust":trust}))?;
        let id = opened["id"].as_str().ok_or("host omitted project route")?;
        self.with_project_route(id)
    }
    fn with_project_route(&self, id: &str) -> Result<Self, String> {
        if id != "default"
            && (id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
        {
            return Err("host returned an invalid project route".into());
        }
        let mut client = self.clone();
        client.prefix = format!("/workspace/{id}");
        Ok(client)
    }

    /// Scope only to a page belonging to this verified Host; no workspace is mounted.
    pub fn for_browser_url(&self, address: &str) -> Result<Self, crate::i18n::PresentedText> {
        let url = url::Url::parse(address)
            .map_err(|_| crate::i18n::UiMessage::new("desktop.host_unverified"))?;
        let own = url::Url::parse(&self.browser_url())
            .map_err(|_| crate::i18n::UiMessage::new("desktop.host_unverified"))?;
        if url.origin() != own.origin()
            || url.path() != "/"
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(crate::i18n::UiMessage::new("desktop.host_unverified").into());
        }
        let routes: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "workspace")
            .collect();
        match routes.as_slice() {
            [] => {
                let mut client = self.clone();
                client.prefix.clear();
                Ok(client)
            }
            [(_, id)] => self
                .with_project_route(id)
                .map_err(|raw| crate::i18n::PresentedText {
                    raw,
                    presentation: Some(Box::new(crate::i18n::UiMessage::new(
                        "desktop.restore_invalid",
                    ))),
                }),
            _ => Err(crate::i18n::UiMessage::new("desktop.restore_invalid").into()),
        }
    }

    pub fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        self.call_with_receipt(method, params)
            .map_err(|error| error.to_string())
    }

    pub fn call_with_receipt(&self, method: &str, params: &Value) -> Result<Value, HostCallError> {
        if method.is_empty()
            || method.len() > 96
            || !method
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
        {
            return Err("invalid host method".into());
        }
        let body = serde_json::to_vec(params).map_err(|_| "cannot encode host request")?;
        let mut response =
            transport::request_presented(self, "POST", &format!("/api/{method}"), &body)?;
        let bytes = transport::read_body(&mut response)?;
        let result: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid host response")?;
        error::decode_result(result)
    }

    pub fn events(&self) -> Result<HostEvents, String> {
        Ok(HostEvents::new(transport::request(
            self,
            "GET",
            "/api/events",
            &[],
        )?))
    }
}
