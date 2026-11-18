//! Host-local activity projection. No tool arguments or durable events enter it.
use crate::CancelToken;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "macos", windows, test))]
const RECENT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseKind {
    Apps,
    Read,
    Wait,
    Act,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ComputerUseTarget {
    pub run_id: String,
    pub project_root: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ComputerUseActivity {
    pub target: ComputerUseTarget,
    pub kind: ComputerUseKind,
    pub app: Option<String>,
    pub recent: bool,
}

#[derive(Deserialize, Serialize)]
pub struct ComputerUseSnapshot {
    pub revision: u64,
    pub activities: Vec<ComputerUseActivity>,
}

#[derive(Default)]
pub(crate) struct Activity {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    revision: u64,
    runs: BTreeMap<String, RunActivity>,
}

struct RunActivity {
    target: ComputerUseTarget,
    lifetime: CancelToken,
    active: BTreeMap<u64, Operation>,
    recent: Option<(u64, Operation, Instant)>,
}

struct Operation {
    kind: ComputerUseKind,
    app: Option<String>,
    lease: CancelToken,
}

impl State {
    fn prune(&mut self, now: Instant) -> bool {
        let mut changed = false;
        self.runs.retain(|_, run| {
            if run.lifetime.is_cancelled() {
                changed = true;
                return false;
            }
            let active = run.active.len();
            run.active.retain(|_, op| !op.lease.is_cancelled());
            changed |= active != run.active.len();
            if run
                .recent
                .as_ref()
                .is_some_and(|(_, op, expires)| now >= *expires || op.lease.is_cancelled())
            {
                run.recent = None;
                changed = true;
            }
            true
        });
        changed
    }

    fn snapshot(&self) -> ComputerUseSnapshot {
        let mut rows = Vec::new();
        for run in self.runs.values() {
            let activity = |op: &Operation, recent| ComputerUseActivity {
                target: run.target.clone(),
                kind: op.kind,
                app: op.app.clone(),
                recent,
            };
            if let Some((&order, op)) = run.active.last_key_value() {
                rows.push((false, std::cmp::Reverse(order), activity(op, false)));
            } else if let Some((order, op, _)) = &run.recent {
                rows.push((true, std::cmp::Reverse(*order), activity(op, true)));
            }
        }
        rows.sort_unstable_by_key(|(recent, order, _)| (*recent, *order));
        ComputerUseSnapshot {
            revision: self.revision,
            activities: rows.into_iter().map(|(_, _, row)| row).collect(),
        }
    }
}

impl Activity {
    fn changed(&self, state: &mut State) -> u64 {
        state.revision += 1;
        self.changed.notify_all();
        state.revision
    }

    pub(crate) fn next(
        &self,
        after: Option<u64>,
        heartbeat: Duration,
    ) -> Option<ComputerUseSnapshot> {
        let until = Instant::now() + heartbeat;
        let mut state = self.state.lock().expect("computer use activity");
        loop {
            let now = Instant::now();
            if state.prune(now) {
                self.changed(&mut state);
            }
            if Some(state.revision) != after {
                return Some(state.snapshot());
            }
            if now >= until {
                return None;
            }
            let wake = state
                .runs
                .values()
                .filter_map(|run| run.recent.as_ref().map(|(_, _, expires)| *expires))
                .min()
                .unwrap_or(until)
                .min(until);
            state = self
                .changed
                .wait_timeout(state, wake - now)
                .expect("computer use activity")
                .0;
        }
    }

    pub(crate) fn stop(&self, target: &ComputerUseTarget) -> bool {
        let mut state = self.state.lock().expect("computer use activity");
        if !state
            .runs
            .get(&target.run_id)
            .is_some_and(|run| run.target == *target)
        {
            return false;
        }
        let run = state.runs.remove(&target.run_id).expect("matched activity");
        let live = !run.lifetime.is_cancelled();
        run.lifetime.cancel();
        self.changed(&mut state);
        live
    }
}

#[cfg(any(target_os = "macos", windows, test))]
impl Activity {
    pub(crate) fn start(
        &self,
        target: &ComputerUseTarget,
        lifetime: &CancelToken,
        lease: &CancelToken,
        kind: ComputerUseKind,
        app: Option<&str>,
    ) -> Option<Use<'_>> {
        let mut state = self.state.lock().expect("computer use activity");
        if lifetime.is_cancelled() || lease.is_cancelled() {
            return None;
        }
        let order = self.changed(&mut state);
        let run = state
            .runs
            .entry(target.run_id.clone())
            .or_insert_with(|| RunActivity {
                target: target.clone(),
                lifetime: lifetime.clone(),
                active: BTreeMap::new(),
                recent: None,
            });
        let app = app.map(|label| label.chars().take(100).collect());
        run.active.insert(
            order,
            Operation {
                kind,
                app,
                lease: lease.clone(),
            },
        );
        run.recent = None;
        Some(Use {
            registry: self,
            run_id: target.run_id.clone(),
            order,
        })
    }

    pub(crate) fn end(&self, target: &ComputerUseTarget, lifetime: &CancelToken) {
        let mut state = self.state.lock().expect("computer use activity");
        lifetime.cancel();
        if state.runs.remove(&target.run_id).is_some() {
            self.changed(&mut state);
        }
    }

    pub(crate) fn permissions_changed(&self) {
        let mut state = self.state.lock().expect("computer use activity");
        if state.prune(Instant::now()) {
            self.changed(&mut state);
        }
    }
}

#[cfg(any(target_os = "macos", windows, test))]
pub(crate) struct Use<'a> {
    registry: &'a Activity,
    run_id: String,
    order: u64,
}

#[cfg(any(target_os = "macos", windows, test))]
impl Use<'_> {
    fn finish(&self, now: Instant) {
        let mut state = self.registry.state.lock().expect("computer use activity");
        if let Some(run) = state.runs.get_mut(&self.run_id)
            && let Some(op) = run.active.remove(&self.order)
        {
            if !op.lease.is_cancelled() && !run.lifetime.is_cancelled() {
                run.recent = Some((self.order, op, now + RECENT));
            }
            self.registry.changed(&mut state);
        }
    }
}

#[cfg(any(target_os = "macos", windows, test))]
impl Drop for Use<'_> {
    fn drop(&mut self) {
        self.finish(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ComputerUseKind::{Act, Apps, Read, Wait};

    fn target(id: &str, project: &str) -> ComputerUseTarget {
        ComputerUseTarget {
            run_id: id.into(),
            session_id: "a session".into(),
            project_root: project.into(),
        }
    }

    #[test]
    fn snapshots_choose_live_activity_and_expire_recent_activity() {
        let registry = Activity::default();
        let (one, two) = (target("one", "/one"), target("two", "/two"));
        let (one_run, two_run) = (CancelToken::new(), CancelToken::new());
        let read = registry
            .start(&one, &one_run, &one_run, Read, Some("Editor"))
            .unwrap();
        let wait = registry
            .start(&two, &two_run, &two_run, Wait, Some("Viewer"))
            .unwrap();
        let kinds = |state: &State| {
            state
                .snapshot()
                .activities
                .iter()
                .map(|row| (row.kind, row.recent))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&registry.state.lock().unwrap()),
            [(Wait, false), (Read, false)]
        );
        let now = Instant::now();
        wait.finish(now);
        let mut state = registry.state.lock().unwrap();
        assert!(!state.prune(now + Duration::from_millis(1999)));
        assert_eq!(kinds(&state), [(Read, false), (Wait, true)]);
        assert!(state.prune(now + RECENT));
        assert_eq!(kinds(&state), [(Read, false)]);
        drop(state);
        drop(read);
        let apps = registry
            .start(&one, &one_run, &one_run, Apps, None)
            .unwrap();
        let act = registry
            .start(&one, &one_run, &one_run, Act, Some("Editor"))
            .unwrap();
        assert_eq!(kinds(&registry.state.lock().unwrap()), [(Act, false)]);
        drop(act);
        assert_eq!(kinds(&registry.state.lock().unwrap()), [(Apps, false)]);
        drop(apps);
        registry.end(&one, &one_run);
        assert!(
            registry
                .next(None, Duration::ZERO)
                .unwrap()
                .activities
                .is_empty()
        );
    }

    #[test]
    fn revocation_stop_and_old_targets_do_not_cross_run_boundaries() {
        let registry = Activity::default();
        let run_source = CancelToken::new();
        let run = run_source.child();
        let lease = run.child();
        let first = target("first", "/one");
        let second = target("second", "/two");
        let second_run = CancelToken::new();
        let first_call = registry
            .start(&first, &run, &lease, Wait, Some("Editor"))
            .unwrap();
        let second_call = registry
            .start(&second, &second_run, &second_run, Apps, None)
            .unwrap();
        lease.cancel();
        registry.permissions_changed();
        drop(first_call);
        let rows = registry.next(None, Duration::ZERO).unwrap().activities;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, second);
        assert!(!registry.stop(&target("second", "/one")));
        assert!(registry.stop(&first));
        assert!(!registry.stop(&first));
        assert!(run.is_cancelled());
        assert!(!run_source.is_cancelled());
        assert!(!second_run.is_cancelled());
        assert!(registry.start(&first, &run, &run, Apps, None).is_none());
        let next_turn = run_source.child();
        let third = target("third", "/one");
        let resumed = registry
            .start(&third, &next_turn, &next_turn, Read, Some("Editor"))
            .unwrap();
        assert!(!registry.stop(&first));
        drop(resumed);
        drop(second_call);
        assert!(!next_turn.is_cancelled());
    }
}
