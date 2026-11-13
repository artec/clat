//! Waits poll the same scoped, bounded observation path as ui_tree.
use super::tree::Walk;
use super::*;
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Condition {
    Appear,
    Disappear,
    Focus,
    ValueEquals,
    Contains,
}

impl Condition {
    fn reads_value(self) -> bool {
        matches!(self, Self::ValueEquals | Self::Contains)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaitRequest {
    pid: i32,
    window_index: Option<usize>,
    condition: Condition,
    role: Option<String>,
    title: Option<String>,
    value: Option<String>,
    timeout_ms: Option<u64>,
}

impl WaitRequest {
    fn validate(&self) -> Result<Duration> {
        let timeout = self.timeout_ms.unwrap_or(5000);
        if !(1..=10000).contains(&timeout)
            || (self.role.is_none() && self.title.is_none())
            || self.role.as_ref().is_some_and(String::is_empty)
            || self.title.as_ref().is_some_and(String::is_empty)
            || self.condition.reads_value() != self.value.is_some()
            || self.value.as_ref().is_some_and(|value| value.len() > 1024)
        {
            return Err(error("INVALID_ARGUMENTS"));
        }
        Ok(Duration::from_millis(timeout))
    }

    fn matches(&self, node: &Node) -> bool {
        self.role.as_ref().is_none_or(|role| role == &node.role)
            && self.title.as_ref().is_none_or(|title| title == &node.title)
    }
}

impl Service {
    pub(super) fn wait_for_condition(&self, run: &Run, request: WaitRequest) -> Result<Value> {
        let timeout = request.validate()?;
        let app = self.app(request.pid)?;
        let lease = self.control.ui_authority.lease(&app, run)?;
        if request.condition.reads_value() {
            self.consent(run, &app, &lease)?;
        }
        let call = deadline(&lease.cancel, timeout);
        let mut window = None;
        loop {
            active(&call)?;
            self.recheck_app(&app)?;
            match self.observe_wait(&app, &lease, &request, &mut window, &call) {
                Ok(true) => {
                    active(&call)?;
                    return Ok(json!({"matched":true,"pid":app.pid}));
                }
                Ok(false) => {}
                Err(error)
                    if error
                        .recovery()
                        .is_some_and(|details| details["code"] == "NATIVE_BUSY") => {}
                Err(error) => return Err(error),
            }
            if let Some(remaining) = call.remaining() {
                std::thread::sleep(remaining.min(Duration::from_millis(60)));
            }
        }
    }

    fn observe_wait(
        &self,
        app: &App,
        lease: &Lease,
        request: &WaitRequest,
        window: &mut Option<Handle>,
        call: &CancelToken,
    ) -> Result<bool> {
        let Some(root) = self.wait_root(app, request.window_index, window, call)? else {
            return Ok(matches!(request.condition, Condition::Disappear));
        };
        let mut walk = Walk::new(root, None);
        while let Some(observed) = walk.next(self.backend.as_ref(), app, call)? {
            if request.matches(&observed.node)
                && self.wait_match(lease, request, &observed.node, call)?
            {
                return Ok(!matches!(request.condition, Condition::Disappear));
            }
        }
        Ok(matches!(request.condition, Condition::Disappear))
    }

    fn wait_root(
        &self,
        app: &App,
        index: Option<usize>,
        selected: &mut Option<Handle>,
        call: &CancelToken,
    ) -> Result<Option<Handle>> {
        if let Some(window) = selected {
            let node = self.backend.describe(window, app, call);
            let current = node.and_then(|node| self.backend.window(&node, app, call));
            return match current {
                Ok(Some(live)) if self.backend.same(window, &live) => Ok(Some(live)),
                Ok(_) => Ok(None),
                Err(error)
                    if error
                        .recovery()
                        .is_some_and(|details| details["code"] == "STALE_SNAPSHOT") =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            };
        }
        let root = self.backend.root(app, call)?;
        let Some(index) = index else {
            return Ok(Some(root));
        };
        *selected = self
            .backend
            .children(&root, index, 1, true, call)?
            .into_iter()
            .next();
        Ok(selected.clone())
    }

    fn wait_match(
        &self,
        lease: &Lease,
        request: &WaitRequest,
        node: &Node,
        call: &CancelToken,
    ) -> Result<bool> {
        Ok(match request.condition {
            Condition::Appear | Condition::Disappear => true,
            Condition::Focus => node.focused == Some(true),
            Condition::ValueEquals | Condition::Contains if node.secure() => false,
            Condition::ValueEquals | Condition::Contains => {
                self.control
                    .ui_authority
                    .authorized(&self.control, lease, || Ok(()))?;
                let value = self.backend.value(node, ValuePurpose::Read, call)?;
                let needle = request
                    .value
                    .as_deref()
                    .ok_or_else(|| error("INVALID_ARGUMENTS"))?;
                value.is_some_and(|value| match request.condition {
                    Condition::ValueEquals => value == needle,
                    _ => value.contains(needle),
                })
            }
        })
    }
}
