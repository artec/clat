//! Per-run accessibility snapshots and action safety state.

use std::collections::VecDeque;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Element {
    pub app: String,
    pub pid: i32,
    pub process_identity: String,
    pub window: String,
    pub identity: String,
    pub role: String,
    pub title: String,
    pub actions: Vec<String>,
    pub state: String,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Failure {
    StaleApp,
    StaleSnapshot,
    UnsupportedAction,
    Unchanged,
    Limit,
    ProtectedTarget,
}

struct Snapshot<T> {
    id: String,
    content: T,
}

pub(super) struct UiState<T> {
    snapshots: VecDeque<Snapshot<T>>,
    dispatched: usize,
    last: Option<(Element, usize)>,
}

impl<T> Default for UiState<T> {
    fn default() -> Self {
        Self {
            snapshots: VecDeque::new(),
            dispatched: 0,
            last: None,
        }
    }
}

impl<T> UiState<T> {
    pub fn snapshot(&mut self, id: String, content: T) {
        self.snapshots.push_back(Snapshot { id, content });
        if self.snapshots.len() > 8 {
            self.snapshots.pop_front();
        }
    }

    pub fn get(&self, id: &str) -> Result<&T, Failure> {
        self.snapshots
            .iter()
            .find(|snapshot| snapshot.id == id)
            .map(|snapshot| &snapshot.content)
            .ok_or(Failure::StaleSnapshot)
    }

    pub fn get_mut(&mut self, id: &str) -> Result<&mut T, Failure> {
        self.snapshots
            .iter_mut()
            .find(|snapshot| snapshot.id == id)
            .map(|snapshot| &mut snapshot.content)
            .ok_or(Failure::StaleSnapshot)
    }

    /// Reserve before entering the OS; uncertain dispatches still consume budget.
    pub fn record_dispatch(&mut self, element: &Element) -> Result<(), Failure> {
        if self.dispatched >= 20 {
            return Err(Failure::Limit);
        }
        let repeats = self
            .last
            .as_ref()
            .filter(|(last, _)| {
                last.app == element.app
                    && last.process_identity == element.process_identity
                    && last.window == element.window
                    && last.identity == element.identity
                    && last.state == element.state
            })
            .map_or(0, |(_, count)| *count);
        if repeats >= 3 {
            return Err(Failure::Unchanged);
        }
        self.dispatched += 1;
        self.last = Some((element.clone(), repeats + 1));
        Ok(())
    }
}

pub(super) fn check_element(old: &Element, current: &Element, action: &str) -> Result<(), Failure> {
    if old.app.is_empty()
        || old.pid <= 0
        || old.process_identity.is_empty()
        || old.app != current.app
        || old.pid != current.pid
        || old.process_identity != current.process_identity
    {
        return Err(Failure::StaleApp);
    }
    if old.window.is_empty()
        || old.identity.is_empty()
        || old.role.is_empty()
        || old.window != current.window
        || old.identity != current.identity
        || old.role != current.role
        || old.title != current.title
    {
        return Err(Failure::StaleSnapshot);
    }
    if !["press", "confirm", "cancel", "pick", "show_menu", "raise"].contains(&action)
        || !old.actions.iter().any(|a| a == action)
        || !current.actions.iter().any(|a| a == action)
    {
        return Err(Failure::UnsupportedAction);
    }
    Ok(())
}

/// Host-supplied provenance; model arguments cannot set these fields.
pub(super) struct TargetFence {
    pub protected_pids: Vec<i32>,
    pub protected_apps: Vec<String>,
    pub independent_local_tui: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApplicationKind {
    Protected,
    Other,
    BrowserOrUnknown,
}

impl TargetFence {
    pub fn check_app(&self, pid: i32, app: &str, kind: ApplicationKind) -> Result<(), Failure> {
        if matches!(kind, ApplicationKind::Protected)
            || pid <= 0
            || app.is_empty()
            || self.protected_pids.contains(&pid)
            || self.protected_apps.iter().any(|protected| protected == app)
            || (matches!(kind, ApplicationKind::BrowserOrUnknown) && !self.independent_local_tui)
        {
            return Err(Failure::ProtectedTarget);
        }
        Ok(())
    }
}

pub(super) fn needs_approval(title: &str) -> bool {
    let lower = title.to_lowercase();
    let lower: String = lower.chars().filter(|ch| !ch.is_control()
        && !matches!(ch, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')).collect();
    [
        "发送", "提交", "删除", "清空", "发布", "支付", "购买", "注销", "同意", "傳送", "刪除",
        "清除", "發佈", "發送", "購買", "註銷", "send", "submit", "delete", "clear", "publish",
        "pay", "purchase", "buy", "sign out", "log out", "logout", "agree",
    ]
    .iter()
    .any(|term| lower.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> Element {
        Element {
            app: "application".into(),
            pid: 42,
            process_identity: "start-1".into(),
            window: "window-1".into(),
            identity: "element-1".into(),
            role: "AXButton".into(),
            title: "Open".into(),
            actions: vec!["press".into()],
            state: "enabled".into(),
        }
    }

    #[test]
    fn cua_snapshot_rejects_pid_reuse_and_stale_nodes() {
        let original = node();
        let mut changed = original.clone();
        changed.process_identity = "start-2".into();
        assert_eq!(
            check_element(&original, &changed, "press"),
            Err(Failure::StaleApp)
        );
        changed = original.clone();
        changed.title = "Delete".into();
        assert_eq!(
            check_element(&original, &changed, "press"),
            Err(Failure::StaleSnapshot)
        );
    }

    #[test]
    fn cua_snapshot_retains_only_eight() {
        let mut state = UiState::default();
        for index in 0..9 {
            state.snapshot(index.to_string(), index);
        }
        assert_eq!(state.get("0"), Err(Failure::StaleSnapshot));
        assert_eq!(state.get("1"), Ok(&1));
        assert_eq!(state.get("8"), Ok(&8));
    }

    #[test]
    fn cua_action_safety_rejects_unsupported_actions_and_fuses_repetition() {
        let mut state = UiState::<()>::default();
        let element = node();
        assert_eq!(
            check_element(&element, &element, "set_value"),
            Err(Failure::UnsupportedAction)
        );
        for _ in 0..3 {
            state.record_dispatch(&element).unwrap();
        }
        assert_eq!(state.record_dispatch(&element), Err(Failure::Unchanged));
        assert!(needs_approval("删除文件"));
        assert!(needs_approval("Submit order"));
        assert!(!needs_approval("Open folder"));
    }

    #[test]
    fn cua_action_budget_counts_only_dispatches_and_changed_state_resets_fuse() {
        let mut state = UiState::<()>::default();
        let mut element = node();
        for i in 0..20 {
            element.state = format!("state-{i}");
            assert_eq!(state.record_dispatch(&element), Ok(()));
        }
        element.state = "new state".into();
        assert_eq!(state.record_dispatch(&element), Err(Failure::Limit));
    }

    #[test]
    fn cua_action_requires_support_in_both_observations() {
        let original = node();
        let mut current = original.clone();
        current.actions.clear();
        assert_eq!(
            check_element(&original, &current, "press"),
            Err(Failure::UnsupportedAction)
        );
    }

    #[test]
    fn cua_action_fence_rejects_host_and_shared_browser_targets() {
        let element = node();
        let mut fence = TargetFence {
            protected_pids: vec![42],
            protected_apps: vec!["terminal".into()],
            independent_local_tui: false,
        };
        assert_eq!(
            fence.check_app(element.pid, &element.app, ApplicationKind::Other),
            Err(Failure::ProtectedTarget)
        );
        fence.protected_pids.clear();
        assert_eq!(
            fence.check_app(element.pid, &element.app, ApplicationKind::BrowserOrUnknown),
            Err(Failure::ProtectedTarget)
        );
        fence.independent_local_tui = true;
        assert_eq!(
            fence.check_app(element.pid, &element.app, ApplicationKind::BrowserOrUnknown),
            Ok(())
        );
        let mut terminal = element;
        terminal.app = "terminal".into();
        assert_eq!(
            fence.check_app(terminal.pid, &terminal.app, ApplicationKind::Other),
            Err(Failure::ProtectedTarget)
        );
    }

    #[test]
    fn cua_action_rejects_missing_identity_even_when_both_observations_match() {
        let mut element = node();
        element.process_identity.clear();
        assert_eq!(
            check_element(&element, &element, "press"),
            Err(Failure::StaleApp)
        );
        element.process_identity = "start".into();
        element.identity.clear();
        assert_eq!(
            check_element(&element, &element, "press"),
            Err(Failure::StaleSnapshot)
        );
    }
}
