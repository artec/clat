//! Windows UI Automation adapter. Shared code owns consent, snapshots and actions.
use super::*;
use crate::native_tools::ui_state::ApplicationKind;
use std::hash::{DefaultHasher, Hash, Hasher};

mod automation;
mod desktop;
mod elements;
mod worker;

use automation::Automation;
use desktop::Window;

pub(super) struct Windows;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ElementKey {
    window: Window,
    runtime: Vec<i32>,
    process: String,
}

struct Retained {
    key: ElementKey,
    slot: u64,
    root: bool,
}

enum Reference {
    Application(App, Vec<Window>),
    Element(Arc<Retained>),
}

impl Reference {
    fn of(handle: &Handle) -> &Self {
        handle.downcast_ref().expect("UIA handle")
    }

    fn retained(handle: &Handle) -> Result<Arc<Retained>> {
        match Self::of(handle) {
            Self::Element(element) => Ok(element.clone()),
            Self::Application(..) => Err(error("STALE_SNAPSHOT")),
        }
    }
}

impl Backend for Windows {
    fn ready(&self) -> Result<()> {
        desktop::interactive()
    }

    fn window_scoped(&self) -> bool {
        true
    }

    fn is_window_root(&self, node: &Node) -> bool {
        matches!(Reference::of(&node.handle), Reference::Element(element) if element.root)
    }

    fn app(&self, pid: i32) -> Result<App> {
        desktop::app(pid)
    }

    fn root(&self, app: &App, call: &CancelToken) -> Result<Handle> {
        active(call)?;
        let windows = desktop::visible_windows(call)?
            .into_iter()
            .filter(|window| window.pid == app.pid)
            .collect();
        Ok(Arc::new(Reference::Application(app.clone(), windows)))
    }

    fn same(&self, left: &Handle, right: &Handle) -> bool {
        match (Reference::of(left), Reference::of(right)) {
            (Reference::Application(left, _), Reference::Application(right, _)) => left == right,
            (Reference::Element(left), Reference::Element(right)) => left.key == right.key,
            _ => false,
        }
    }

    fn hash(&self, node: &Handle) -> u64 {
        let mut hash = DefaultHasher::new();
        match Reference::of(node) {
            Reference::Application(app, _) => app.pid.hash(&mut hash),
            Reference::Element(element) => element.key.hash(&mut hash),
        }
        hash.finish()
    }

    fn children(
        &self,
        node: &Handle,
        offset: usize,
        count: usize,
        windows: bool,
        call: &CancelToken,
    ) -> Result<Vec<Handle>> {
        match Reference::of(node) {
            Reference::Application(app, inventory) if windows => {
                let app = app.clone();
                let windows = inventory
                    .iter()
                    .copied()
                    .skip(offset)
                    .take(count)
                    .collect::<Vec<_>>();
                worker::run(call, move |uia, call, _| uia.windows(&app, &windows, call))
            }
            Reference::Application(..) => Ok(Vec::new()),
            Reference::Element(element) => {
                let element = element.clone();
                worker::run(call, move |uia, call, _| {
                    uia.children(&element, offset, count, call)
                })
            }
        }
    }

    fn menu_bar(&self, _: &Handle, _: &CancelToken) -> Result<Option<Handle>> {
        Ok(None)
    }

    fn describe(&self, node: &Handle, app: &App, call: &CancelToken) -> Result<Node> {
        if matches!(Reference::of(node), Reference::Application(root, _) if root == app) {
            return Ok(Node {
                handle: node.clone(),
                role: "UIAApplication".into(),
                title: app.label.clone(),
                subrole: None,
                description: None,
                actions: vec![],
                label_complete: true,
                enabled: None,
                focused: None,
                selected: None,
                expanded: None,
            });
        }
        let element = Reference::retained(node)?;
        let handle = node.clone();
        let app = app.clone();
        worker::run(call, move |uia, call, _| {
            if element.key.process != app.process_identity || element.key.window.pid != app.pid {
                return Err(error("STALE_APP"));
            }
            uia.describe(&element, handle, call)
        })
    }

    fn value(
        &self,
        node: &Node,
        purpose: ValuePurpose,
        call: &CancelToken,
    ) -> Result<Option<String>> {
        if node.secure() || matches!(Reference::of(&node.handle), Reference::Application(..)) {
            return Ok(None);
        }
        let element = Reference::retained(&node.handle)?;
        worker::run(call, move |uia, call, _| uia.value(&element, purpose, call))
    }

    fn window(&self, node: &Node, _: &App, call: &CancelToken) -> Result<Option<Handle>> {
        let element = Reference::retained(&node.handle)?;
        worker::run(call, move |uia, call, _| {
            uia.window(&element, call).map(Some)
        })
    }

    fn dispatch(&self, node: &Node, action: &str, call: &CancelToken) -> Result<()> {
        let expected = node.clone();
        let element = Reference::retained(&node.handle)?;
        let action = action.to_owned();
        worker::run(call, move |uia, call, dispatch| {
            uia.dispatch(&element, expected, &action, call, dispatch)
        })
    }

    fn target_fence(&self, standalone_terminal: bool) -> TargetFence {
        desktop::fence(standalone_terminal)
    }
}

pub(super) fn status() -> &'static str {
    if desktop::interactive().is_err() {
        return "locked";
    }
    let call = deadline(&CancelToken::new(), Duration::from_millis(750));
    if worker::run(&call, |_, _, _| Ok(())).is_ok() {
        "ready"
    } else {
        "unknown"
    }
}

pub(super) fn inventory(call: &CancelToken) -> Result<Value> {
    Windows.ready()?;
    let mut counts = std::collections::BTreeMap::new();
    for window in desktop::visible_windows(call)? {
        *counts.entry(window.pid).or_insert(0) += 1;
    }
    let focused = desktop::foreground_pid();
    let mut rows = Vec::new();
    for (pid, count) in counts {
        active(call)?;
        let Ok(app) = Windows.app(pid) else { continue };
        rows.push(
            json!({"pid":pid,"name":app.label,"app_user_model_id":app.bundle,
            "focused":focused == Some(pid),"window_count":count}),
        );
    }
    active(call)?;
    Ok(json!({"platform":"windows","applications":rows}))
}

#[cfg(test)]
mod live_tests;
