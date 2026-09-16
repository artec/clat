//! Authenticated project routing. Each project has its own run, drafts,
//! approval arbitration and subscriber stream; the root control plane is shared.
use super::protocol::RpcError;
use super::state::ServeShared;
use crate::application::HostApplication;
use crate::{Project, ProjectAuthorization, TrustedProjectApplication};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_MOUNTED_PROJECTS: usize = 16;
#[cfg(test)]
mod lifecycle_tests;
mod reclaim;
pub(crate) const HOST_METHODS: &[&str] = &[
    "host.describe",
    "host.stop",
    "host.takeover",
    "workspace.list",
    "workspace.open",
];

struct Projects {
    application: HostApplication,
    routes: BTreeMap<String, Arc<ServeShared>>,
}

pub(crate) struct WorkspaceHost {
    projects: Mutex<Projects>,
    instance: String,
    build_fingerprint: String,
    shutdown: Arc<AtomicBool>,
}

impl Drop for WorkspaceHost {
    fn drop(&mut self) {
        // Includes startup failures before the accept loop takes ownership.
        let _ = self.close();
    }
}

impl WorkspaceHost {
    pub(crate) fn publish_endpoint(&self, root: &Path, port: u16) -> Result<(), String> {
        crate::host_client::discovery::publish(root, port, &self.instance)
    }
    pub(crate) fn matches_instance(&self, expected: &str) -> bool {
        self.instance == expected
    }
    pub(crate) fn new(
        application: TrustedProjectApplication,
        token: String,
        port: u16,
        queue_frames: usize,
        build_fingerprint: String,
        shutdown: Arc<AtomicBool>,
    ) -> Arc<Self> {
        let project = application.project().clone();
        let mut application = HostApplication::new(application);
        let app = application
            .attach(project)
            .expect("bootstrap project is mounted");
        let mut shared = ServeShared::new(app, token, port);
        shared.queue_frames = queue_frames;
        let shared = Arc::new(shared);
        let host = Arc::new(Self {
            projects: Mutex::new(Projects {
                application,
                routes: BTreeMap::from([("default".into(), shared.clone())]),
            }),
            instance: uuid::Uuid::new_v4().to_string(),
            build_fingerprint,
            shutdown,
        });
        *shared.host.lock().expect("host link") = Arc::downgrade(&host);
        shared.spawn_notice_forwarder();
        host
    }

    pub(crate) fn route(&self, id: &str) -> Result<Arc<ServeShared>, RpcError> {
        self.projects
            .lock()
            .expect("host projects")
            .routes
            .get(id)
            .map(|shared| {
                shared.touch();
                shared.clone()
            })
            .ok_or_else(|| RpcError::not_found("workspace is not mounted"))
    }

    pub(crate) fn dispatch(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
    ) -> Result<Value, RpcError> {
        let params = params
            .as_object()
            .ok_or_else(|| RpcError::bad_request("params must be an object"))?;
        if method == "host.stop" {
            self.shutdown.store(true, Ordering::SeqCst);
            return Ok(json!({"stopping": true}));
        }
        if method == "host.takeover" {
            return self.takeover(params);
        }
        let mut projects = self.projects.lock().expect("host projects");
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(RpcError::busy("host is shutting down"));
        }
        match method {
            "host.describe" => Ok(json!({"protocol_version": 1, "instance_id": self.instance,
                "storage_root": projects.application.storage_root(), "methods": HOST_METHODS,
                "wire_version": crate::wire::WIRE_VERSION,
                "journal_version": crate::session::compat::SESSION_FORMAT_VERSION,
                "product_version": crate::host_client::build_identity::product_version(),
                "build_fingerprint": self.build_fingerprint})),
            "workspace.list" => Ok(
                json!({"workspaces": projects.routes.iter().map(|(id, shared)| {
                let app = shared.app.lock().expect("application lock");
                json!({"id": id, "root": app.project().root(), "api_prefix": format!("/workspace/{id}")})
            }).collect::<Vec<_>>()}),
            ),
            "workspace.open" => self.open(&mut projects, params),
            _ => Err(RpcError::bad_request("unknown host method")),
        }
    }

    fn takeover(&self, params: &serde_json::Map<String, Value>) -> Result<Value, RpcError> {
        let expected = params
            .get("expected_instance_id")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::bad_request("expected_instance_id is required"))?;
        if expected != self.instance {
            return Err(RpcError::busy(
                "host instance changed; refresh before requesting takeover",
            ));
        }
        let replacement = params
            .get("replacement_product_version")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::bad_request("replacement_product_version is required"))?;
        let order = crate::host_client::build_identity::compare_product_versions(
            replacement,
            crate::host_client::build_identity::product_version(),
        )
        .map_err(|_| RpcError::bad_request("replacement_product_version is invalid"))?;
        if order != std::cmp::Ordering::Greater {
            return Err(RpcError::bad_request(
                "replacement_product_version must be newer than the running host",
            ));
        }

        let projects = self.projects.lock().expect("host projects");
        if self.shutdown.load(Ordering::SeqCst) {
            return Ok(json!({"stopping": true}));
        }
        let routes = projects.routes.values().cloned().collect::<Vec<_>>();
        let _mutations = routes
            .iter()
            .map(|shared| shared.rpc_mutations.lock().expect("project RPC mutation"))
            .collect::<Vec<_>>();
        if routes.iter().any(|shared| !shared.is_idle_for_takeover()) {
            return Err(RpcError::busy(
                "host has active work; finish or cancel it before upgrading",
            ));
        }
        for shared in &routes {
            shared.mark_shutting_down();
        }
        self.shutdown.store(true, Ordering::SeqCst);
        Ok(json!({"stopping": true}))
    }

    fn open(
        self: &Arc<Self>,
        projects: &mut Projects,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, RpcError> {
        let path = params
            .get("root")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::bad_request("root is required"))?;
        let path = std::path::Path::new(path);
        if !path.is_absolute() {
            return Err(RpcError::bad_request("root must be absolute"));
        }
        let root = path
            .canonicalize()
            .map_err(|_| RpcError::not_found("project directory is unavailable"))?;
        for (id, shared) in &projects.routes {
            if shared
                .app
                .lock()
                .expect("application lock")
                .project()
                .root()
                .canonicalize()
                .ok()
                .as_ref()
                == Some(&root)
            {
                shared.touch();
                return Ok(json!({"id": id, "api_prefix": format!("/workspace/{id}")}));
            }
        }
        // A transport credential alone is not project trust. Consent is explicit.
        let authorize = match params.get("trust") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            _ => return Err(RpcError::bad_request("trust must be a boolean")),
        };
        if !root.is_dir() {
            return Err(RpcError::bad_request("project must be a directory"));
        }
        if !authorize
            && !projects
                .application
                .is_project_trusted(&Project::new(&root))
        {
            return Err(RpcError::bad_request(
                "project is not trusted; explicit consent is required",
            ));
        }
        if projects.routes.len() >= MAX_MOUNTED_PROJECTS {
            self.reclaim_one(projects)?;
        }
        let app = if authorize {
            projects
                .application
                .authorize_and_attach(Project::new(root), ProjectAuthorization::grant())
        } else {
            projects.application.attach(Project::new(root))
        }
        .map_err(|error| RpcError::bad_request(error.to_string()))?;
        let shared = self.shared_for(projects, app);
        let id = uuid::Uuid::new_v4().to_string();
        projects.routes.insert(id.clone(), shared);
        Ok(json!({"id": id, "api_prefix": format!("/workspace/{id}")}))
    }

    fn shared_for(
        self: &Arc<Self>,
        projects: &Projects,
        app: Arc<Mutex<TrustedProjectApplication>>,
    ) -> Arc<ServeShared> {
        let default = projects.routes.get("default").expect("default route");
        let mut shared = ServeShared::new(app, default.token.clone(), default.port);
        shared.queue_frames = default.queue_frames;
        let shared = Arc::new(shared);
        *shared.host.lock().expect("host link") = Arc::downgrade(self);
        shared.spawn_notice_forwarder();
        shared
    }

    pub(crate) fn close(&self) -> Result<(), String> {
        self.shutdown.store(true, Ordering::SeqCst);
        let routes = self
            .projects
            .lock()
            .expect("host projects")
            .routes
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for shared in &routes {
            shared.mark_shutting_down();
            shared.cancel_active_run();
            shared.clear_subscribers();
        }
        for shared in &routes {
            shared.drain_connections();
            shared.drain_workers();
        }
        drop(routes);
        let mut projects = self.projects.lock().expect("host projects");
        projects.routes.clear();
        projects
            .application
            .close()
            .map_err(|error| error.to_string())
    }
}
