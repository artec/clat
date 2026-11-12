//! Public macOS Accessibility and AppKit backend. The native AX design uses
//! retained references, CFEqual, paged children and batched observations.
use super::*;
use ffi::{Cf, ax};
use std::ffi::CStr;

mod apps;
pub(in crate::native_tools) use apps::console_ready;
pub(super) use ffi::prompt_accessibility;
pub(in crate::native_tools) fn trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() != 0 }
}
pub(in crate::native_tools) fn window_count(pid: i32, cancel: &CancelToken) -> Option<usize> {
    let app = ffi::application(pid).ok()?;
    ax(&app).count(c"AXWindows", cancel).ok().flatten()
}
mod ffi;

pub(super) struct Mac;

const ACTIONS: &[(&str, &CStr)] = &[
    ("press", c"AXPress"),
    ("confirm", c"AXConfirm"),
    ("cancel", c"AXCancel"),
    ("pick", c"AXPick"),
    ("show_menu", c"AXShowMenu"),
    ("raise", c"AXRaise"),
];

impl Backend for Mac {
    fn ready(&self) -> Result<()> {
        if !trusted() {
            return Err(error("PERMISSION_DENIED"));
        }
        apps::console_ready()
    }

    fn app(&self, pid: i32) -> Result<App> {
        apps::read_app(pid)
    }

    fn root(&self, app: &App, call: &CancelToken) -> Result<Handle> {
        active(call)?;
        ffi::application(app.pid)
    }

    fn same(&self, left: &Handle, right: &Handle) -> bool {
        ax(left).value.same(&ax(right).value)
    }

    fn hash(&self, node: &Handle) -> u64 {
        ax(node).value.hash()
    }

    fn children(
        &self,
        node: &Handle,
        offset: usize,
        count: usize,
        windows: bool,
        call: &CancelToken,
    ) -> Result<Vec<Handle>> {
        ax(node).children(
            if windows { c"AXWindows" } else { c"AXChildren" },
            offset,
            count,
            call,
        )
    }

    fn menu_bar(&self, node: &Handle, call: &CancelToken) -> Result<Option<Handle>> {
        ax(node).element(c"AXMenuBar", call)
    }

    fn describe(&self, node: &Handle, app: &App, call: &CancelToken) -> Result<Node> {
        let reference = ax(node);
        if reference.pid()? != app.pid {
            return Err(error("FOREIGN_ELEMENT"));
        }
        let (role, subrole) = role(node, call)?;
        let protected = secure(&role, subrole.as_deref());
        let (title, description, label_complete) = if protected {
            (String::new(), None, true)
        } else {
            labels(node, call)?
        };
        let attrs = reference.attrs(
            [c"AXEnabled", c"AXFocused", c"AXSelected", c"AXExpanded"],
            call,
        )?;
        let available = reference.actions(call)?;
        Ok(Node {
            handle: node.clone(),
            role,
            title,
            subrole,
            description,
            label_complete,
            actions: ACTIONS
                .iter()
                .filter(|(_, native)| {
                    available
                        .iter()
                        .any(|item| item.as_bytes() == native.to_bytes())
                })
                .map(|(name, _)| (*name).to_owned())
                .collect(),
            enabled: boolean(&attrs[0]),
            focused: boolean(&attrs[1]),
            selected: boolean(&attrs[2]),
            expanded: boolean(&attrs[3]),
        })
    }

    fn value(&self, node: &Node, call: &CancelToken) -> Result<Option<String>> {
        if node.secure() || !verified_nonsecure_ancestry(&node.handle, call)? {
            return Ok(None);
        }
        Ok(ax(&node.handle)
            .attr(c"AXValue", call)?
            .as_ref()
            .and_then(Cf::scalar))
    }

    fn window(&self, node: &Node, app: &App, call: &CancelToken) -> Result<Option<Handle>> {
        let Some(window) = enclosing_window(node, call)? else {
            return Ok(None);
        };
        verify_window(app, &window, self, call)?;
        Ok(Some(window))
    }

    fn dispatch(&self, node: &Node, action: &str, call: &CancelToken) -> Result<()> {
        let native = ACTIONS
            .iter()
            .find(|(name, _)| *name == action)
            .map(|(_, native)| *native)
            .ok_or_else(|| error("UNSUPPORTED_ACTION"))?;
        ax(&node.handle).perform(native, call)
    }

    fn target_fence(&self, standalone_terminal: bool) -> TargetFence {
        apps::fence(standalone_terminal)
    }
}

fn role(node: &Handle, call: &CancelToken) -> Result<(String, Option<String>)> {
    let attrs = ax(node).attrs([c"AXRole", c"AXSubrole"], call)?;
    let role = text::<129>(&attrs[0]).ok_or_else(|| error("STALE_SNAPSHOT"))?;
    let subrole = text::<129>(&attrs[1]);
    if attrs[1].is_some() && subrole.is_none() {
        return Err(error("NATIVE_ERROR"));
    }
    Ok((role, subrole))
}

fn secure(role: &str, subrole: Option<&str>) -> bool {
    role == "AXSecureTextField" || subrole == Some("AXSecureTextField")
}

fn labels(node: &Handle, call: &CancelToken) -> Result<(String, Option<String>, bool)> {
    let attrs = ax(node).attrs([c"AXTitle", c"AXDescription"], call)?;
    let title = text::<385>(&attrs[0]);
    let description = text::<385>(&attrs[1]);
    let complete =
        (attrs[0].is_none() || title.is_some()) && (attrs[1].is_none() || description.is_some());
    Ok((title.unwrap_or_default(), description, complete))
}

fn text<const N: usize>(value: &Option<Cf>) -> Option<String> {
    value.as_ref().and_then(Cf::string::<N>)
}

fn boolean(value: &Option<Cf>) -> Option<bool> {
    value.as_ref().and_then(Cf::boolean)
}

fn verified_nonsecure_ancestry(node: &Handle, call: &CancelToken) -> Result<bool> {
    let mut current = node.clone();
    let owner = ax(node).pid()?;
    for _ in 0..=64 {
        let (role, subrole) = role(&current, call)?;
        if ax(&current).pid()? != owner || secure(&role, subrole.as_deref()) {
            return Ok(false);
        }
        if role == "AXApplication" {
            return Ok(true);
        }
        let Some(parent) = ax(&current).element(c"AXParent", call)? else {
            return Ok(false);
        };
        current = parent;
    }
    Ok(false)
}

fn enclosing_window(node: &Node, call: &CancelToken) -> Result<Option<Handle>> {
    if matches!(node.role.as_str(), "AXWindow" | "AXMenuBar") {
        return Ok(Some(node.handle.clone()));
    }
    let owner = ax(&node.handle).pid()?;
    let mut current = node.handle.clone();
    for _ in 0..=64 {
        let Some(parent) = ax(&current).element(c"AXParent", call)? else {
            return Ok(None);
        };
        if ax(&parent).pid()? != owner {
            return Err(error("STALE_SNAPSHOT"));
        }
        let (role, _) = role(&parent, call)?;
        if matches!(role.as_str(), "AXWindow" | "AXMenuBar") {
            return Ok(Some(parent));
        }
        if role == "AXApplication" {
            return Ok(None);
        }
        current = parent;
    }
    Err(error("STALE_SNAPSHOT"))
}

fn verify_window(app: &App, window: &Handle, backend: &Mac, call: &CancelToken) -> Result<()> {
    let root = ffi::application(app.pid)?;
    if role(window, call)?.0 == "AXMenuBar" {
        return backend
            .menu_bar(&root, call)?
            .filter(|menu| backend.same(menu, window))
            .map(|_| ())
            .ok_or_else(|| error("STALE_SNAPSHOT"));
    }
    for offset in (0..1024).step_by(16) {
        let windows = backend.children(&root, offset, 16, true, call)?;
        if windows.iter().any(|current| backend.same(current, window)) {
            return Ok(());
        }
        if windows.len() < 16 {
            break;
        }
    }
    Err(error("STALE_SNAPSHOT"))
}
