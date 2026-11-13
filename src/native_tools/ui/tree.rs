//! Bounded, scoped AX traversal. Continuations keep opaque native handles in
//! the run; indices refer only to the snapshot that issued them.
use super::*;
use serde::Deserialize;
use std::collections::HashMap;

const PAGE: usize = 100;
const BATCH: usize = 16;
const MAX_NODES: usize = 8192;
const MAX_DEPTH: usize = 64;
pub(super) const READ_TIME: Duration = Duration::from_secs(10);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TreeRequest {
    pid: Option<i32>,
    window_index: Option<usize>,
    snapshot_id: Option<String>,
    element_index: Option<usize>,
    cursor: Option<usize>,
    include_values: Option<bool>,
    max_depth: Option<usize>,
    page_size: Option<usize>,
}

#[derive(Clone)]
pub(super) struct Observed {
    pub(super) node: Node,
    pub(super) window: Option<Handle>,
    parent: Option<usize>,
    depth: usize,
}

pub(super) struct Snapshot {
    pub(super) app: App,
    pub(super) lease: Arc<Lease>,
    pub(super) nodes: Vec<Observed>,
    walk: Walk,
    values: bool,
}

#[derive(Clone)]
pub(super) struct Walk {
    pending: Vec<Work>,
    seen: HashMap<u64, Vec<Handle>>,
    visited: usize,
    lookahead: Option<Observed>,
    depth_limit: Option<usize>,
}

#[derive(Clone)]
enum Work {
    Visit {
        handle: Handle,
        parent: Option<usize>,
        window: Option<Handle>,
        depth: usize,
    },
    Children {
        node: Observed,
        parent: usize,
        offset: usize,
        source: Children,
    },
}

#[derive(Clone, Copy)]
enum Children {
    Ordinary,
    Windows,
    MenuBar,
}

impl Walk {
    pub(super) fn new(root: Handle, window: Option<Handle>) -> Self {
        Self {
            pending: vec![Work::Visit {
                handle: root,
                parent: None,
                window,
                depth: 0,
            }],
            seen: HashMap::new(),
            visited: 0,
            lookahead: None,
            depth_limit: None,
        }
    }

    pub(super) fn next(
        &mut self,
        backend: &dyn Backend,
        app: &App,
        call: &CancelToken,
    ) -> Result<Option<Observed>> {
        active(call)?;
        if let Some(observed) = self.lookahead.take() {
            return Ok(Some(observed));
        }
        while let Some(work) = self.pending.pop() {
            active(call)?;
            match work {
                Work::Visit {
                    handle,
                    parent,
                    window,
                    depth,
                } => {
                    let seen = self.seen.entry(backend.hash(&handle)).or_default();
                    if seen.iter().any(|prior| backend.same(prior, &handle)) {
                        continue;
                    }
                    if self.visited >= MAX_NODES || depth > MAX_DEPTH {
                        return Err(error("TREE_LIMIT"));
                    }
                    seen.push(handle.clone());
                    let node = match backend.describe(&handle, app, call) {
                        Ok(node) => node,
                        Err(error)
                            if error
                                .recovery()
                                .is_some_and(|details| details["code"] == "FOREIGN_ELEMENT") =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let window = if matches!(node.role.as_str(), "AXWindow" | "AXMenuBar") {
                        Some(handle)
                    } else {
                        window
                    };
                    let observed = Observed {
                        node,
                        window,
                        parent,
                        depth,
                    };
                    self.enqueue_children(&observed);
                    self.visited += 1;
                    return Ok(Some(observed));
                }
                Work::Children {
                    node,
                    parent,
                    offset,
                    source,
                } => {
                    self.enqueue_batch(backend, node, parent, offset, source, call)?;
                }
            }
        }
        Ok(None)
    }

    fn enqueue_children(&mut self, observed: &Observed) {
        if observed.node.secure() || self.depth_limit == Some(observed.depth) {
            return;
        }
        let work = |source| Work::Children {
            node: observed.clone(),
            parent: self.visited,
            offset: 0,
            source,
        };
        self.pending.push(work(Children::Ordinary));
        if observed.node.role == "AXApplication" {
            self.pending.push(work(Children::MenuBar));
            self.pending.push(work(Children::Windows));
        }
    }

    fn enqueue_batch(
        &mut self,
        backend: &dyn Backend,
        node: Observed,
        parent: usize,
        offset: usize,
        source: Children,
        call: &CancelToken,
    ) -> Result<()> {
        let children = match source {
            Children::MenuBar => backend
                .menu_bar(&node.node.handle, call)?
                .into_iter()
                .collect(),
            Children::Windows | Children::Ordinary => backend.children(
                &node.node.handle,
                offset,
                BATCH,
                matches!(source, Children::Windows),
                call,
            )?,
        };
        if children.len() == BATCH {
            self.pending.push(Work::Children {
                node: node.clone(),
                parent,
                offset: offset + BATCH,
                source,
            });
        }
        self.pending
            .extend(children.into_iter().rev().map(|handle| Work::Visit {
                handle,
                parent: Some(parent),
                window: node.window.clone(),
                depth: node.depth + 1,
            }));
        Ok(())
    }
}

impl Node {
    pub(super) fn secure(&self) -> bool {
        self.role == "AXSecureTextField" || self.subrole.as_deref() == Some("AXSecureTextField")
    }
}

impl Service {
    pub(super) fn tree(&self, run: &Run, request: TreeRequest) -> Result<Value> {
        let TreeRequest {
            pid,
            window_index,
            snapshot_id,
            element_index,
            cursor,
            include_values,
            max_depth,
            page_size,
        } = request;
        let page_size = page_size.unwrap_or(PAGE);
        if !(1..=PAGE).contains(&page_size)
            || max_depth.is_some_and(|depth| depth > MAX_DEPTH || cursor.is_some())
        {
            return Err(error("INVALID_ARGUMENTS"));
        }
        if let (None, None, Some(id), None, Some(offset)) =
            (&pid, &window_index, &snapshot_id, &element_index, cursor)
        {
            return self.next_page(run, id, offset, include_values, page_size);
        }
        let (app, lease, selected) = match (pid, window_index, snapshot_id, element_index, cursor) {
            (Some(pid), window, None, None, None) => {
                let app = self.app(pid)?;
                let lease = self.control.ui_authority.lease(&app, run)?;
                (app, lease, Scope::Application(window))
            }
            (None, None, Some(id), Some(index), None) => {
                let (app, lease, observed) = get_observation(run, &id, index)?;
                (app, lease, Scope::Subtree(observed))
            }
            _ => return Err(error("INVALID_ARGUMENTS")),
        };
        let values = include_values.unwrap_or(false);
        if values {
            self.consent(run, &app, &lease)?;
        }
        let call = deadline(&lease.cancel, READ_TIME);
        self.recheck_app(&app)?;
        let (root, window) = self.scope(&app, selected, &call)?;
        let id = run.snapshot_id();
        let mut walk = Walk::new(root, window);
        walk.depth_limit = max_depth;
        let mut snapshot = Snapshot {
            app,
            lease,
            nodes: Vec::new(),
            walk,
            values,
        };
        let result = self.page(&id, &mut snapshot, page_size, &call)?;
        active(&snapshot.lease.cancel)?;
        run.data
            .try_lock()
            .map_err(|_| error("NATIVE_BUSY"))?
            .snapshot(id, snapshot);
        Ok(result)
    }

    fn scope(
        &self,
        app: &App,
        scope: Scope,
        call: &CancelToken,
    ) -> Result<(Handle, Option<Handle>)> {
        match scope {
            Scope::Application(window_index) => {
                let root = self.backend.root(app, call)?;
                if let Some(index) = window_index {
                    let window = self
                        .backend
                        .children(&root, index, 1, true, call)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| error("STALE_SNAPSHOT"))?;
                    return Ok((window.clone(), Some(window)));
                }
                Ok((root, None))
            }
            Scope::Subtree(observed) => {
                let node = self.backend.describe(&observed.node.handle, app, call)?;
                let window = self.backend.window(&node, app, call)?;
                if !same_window(self.backend.as_ref(), &observed.window, &window) {
                    return Err(error("STALE_SNAPSHOT"));
                }
                Ok((node.handle, window))
            }
        }
    }

    fn next_page(
        &self,
        run: &Run,
        id: &str,
        offset: usize,
        values: Option<bool>,
        page_size: usize,
    ) -> Result<Value> {
        let mut state = run.data.try_lock().map_err(|_| error("NATIVE_BUSY"))?;
        let snapshot = state.get_mut(id).map_err(safety_error)?;
        if offset != snapshot.nodes.len()
            || snapshot.walk.lookahead.is_none()
            || values.is_some_and(|values| values != snapshot.values)
        {
            return Err(error("STALE_SNAPSHOT"));
        }
        // The Plan Mode guard is checked even when a continuation omits include_values.
        if snapshot.values {
            super::api::validate_plan(
                run,
                super::api::Operation::Tree,
                &json!({"include_values":true}),
            )?;
        }
        self.recheck_app(&snapshot.app)?;
        let call = deadline(&snapshot.lease.cancel, READ_TIME);
        self.page(id, snapshot, page_size, &call)
    }

    fn page(
        &self,
        id: &str,
        snapshot: &mut Snapshot,
        page_size: usize,
        call: &CancelToken,
    ) -> Result<Value> {
        // Failed pages leave the old cursor intact.
        let mut walk = snapshot.walk.clone();
        let mut nodes = Vec::new();
        let mut rows = Vec::new();
        for _ in 0..page_size {
            let Some(observed) = walk.next(self.backend.as_ref(), &snapshot.app, call)? else {
                break;
            };
            let index = snapshot.nodes.len() + nodes.len();
            rows.push(self.row(index, &observed, snapshot, call)?);
            nodes.push(observed);
        }
        if nodes.len() == page_size {
            walk.lookahead = walk.next(self.backend.as_ref(), &snapshot.app, call)?;
        }
        active(call)?;
        let has_more = walk.lookahead.is_some();
        snapshot.walk = walk;
        snapshot.nodes.extend(nodes);
        Ok(
            json!({"snapshot_id":id,"pid":snapshot.app.pid,"nodes":rows,"has_more":has_more,
            "next_cursor":has_more.then_some(snapshot.nodes.len())}),
        )
    }

    fn row(
        &self,
        index: usize,
        observed: &Observed,
        snapshot: &Snapshot,
        call: &CancelToken,
    ) -> Result<Value> {
        let node = &observed.node;
        let mut value_state = if node.secure() { "secure" } else { "redacted" };
        let mut row = json!({"index":index,"parent_index":observed.parent,"depth":observed.depth,
            "role":node.role,"title":node.title,"subrole":node.subrole,"description":node.description,
            "enabled":node.enabled,"focused":node.focused,"selected":node.selected,
            "expanded":node.expanded,"actions":node.actions});
        if snapshot.values && !node.secure() {
            self.control
                .ui_authority
                .authorized(&self.control, &snapshot.lease, || Ok(()))?;
            let value = self.backend.value(node, ValuePurpose::Read, call)?;
            value_state = if value.is_some() {
                "visible"
            } else {
                "unavailable"
            };
            if let Some(value) = value {
                row["value"] = json!(value);
            }
        }
        row["value_state"] = json!(value_state);
        Ok(row)
    }
}

enum Scope {
    Application(Option<usize>),
    Subtree(Observed),
}

pub(super) fn same_window(
    backend: &dyn Backend,
    left: &Option<Handle>,
    right: &Option<Handle>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => backend.same(left, right),
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn get_observation(
    run: &Run,
    id: &str,
    index: usize,
) -> Result<(App, Arc<Lease>, Observed)> {
    let state = run.data.try_lock().map_err(|_| error("NATIVE_BUSY"))?;
    let snapshot = state.get(id).map_err(safety_error)?;
    if snapshot.lease.cancel.is_cancelled() {
        return Err(error("STALE_SNAPSHOT"));
    }
    let observed = snapshot
        .nodes
        .get(index)
        .cloned()
        .ok_or_else(|| error("STALE_SNAPSHOT"))?;
    Ok((snapshot.app.clone(), snapshot.lease.clone(), observed))
}
