//! Owns the commit and revocation of one console login slot per provider and storage root.
use super::*;
use serde_json::Value;
use std::collections::BTreeMap;
use tokenplan_quota::ConsoleCallbackCancel;

pub(super) static DEVICE: LoginFlows = LoginFlows::new("qianwen");
pub(super) static CALLBACK: LoginFlows = LoginFlows::new("bailian");

pub(super) enum Start {
    New(Uuid),
    Pending(Value),
}

pub(super) enum LoginFailure {
    Cancelled,
    Denied,
    Expired,
    Failed,
}

struct Flow {
    id: Uuid,
    stage: Stage,
}

enum Stage {
    Starting,
    Pending {
        view: Value,
        cancel: Option<ConsoleCallbackCancel>,
    },
    Complete,
    Failed(LoginFailure),
}

impl Stage {
    fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Pending { .. })
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Pending { .. } => "pending",
            Self::Complete => "complete",
            Self::Failed(LoginFailure::Cancelled | LoginFailure::Failed) => "failed",
            Self::Failed(LoginFailure::Denied) => "denied",
            Self::Failed(LoginFailure::Expired) => "expired",
        }
    }
}

pub(super) struct LoginFlows {
    provider: &'static str,
    slots: Mutex<BTreeMap<PathBuf, Flow>>,
}

impl LoginFlows {
    const fn new(provider: &'static str) -> Self {
        Self {
            provider,
            slots: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn start(&self, control: &ControlStorage) -> Result<Start, ApplicationError> {
        let root = control.root_path();
        let mut slots = self.slots.lock().expect("console login flows");
        match slots.get(root).map(|flow| &flow.stage) {
            Some(Stage::Pending { view, .. }) => return Ok(Start::Pending(view.clone())),
            Some(Stage::Starting) => {
                return Err(ApplicationError::new("console login is starting"));
            }
            _ => {}
        }
        let id = Uuid::new_v4();
        slots.insert(
            root.to_owned(),
            Flow {
                id,
                stage: Stage::Starting,
            },
        );
        Ok(Start::New(id))
    }

    pub(super) fn publish(
        &self,
        control: &ControlStorage,
        id: Uuid,
        view: Value,
        cancel: Option<ConsoleCallbackCancel>,
    ) -> Result<(), ApplicationError> {
        let mut slots = self.slots.lock().expect("console login flows");
        let flow = slots
            .get_mut(control.root_path())
            .filter(|flow| flow.id == id && matches!(flow.stage, Stage::Starting))
            .ok_or_else(|| ApplicationError::new("console login was cancelled"))?;
        flow.stage = Stage::Pending { view, cancel };
        Ok(())
    }

    pub(super) fn status(&self, control: &ControlStorage) -> &'static str {
        self.slots
            .lock()
            .expect("console login flows")
            .get(control.root_path())
            .map_or("none", |flow| flow.stage.status())
    }

    pub(super) fn is_pending(&self, control: &ControlStorage, id: Uuid) -> bool {
        self.slots
            .lock()
            .expect("console login flows")
            .get(control.root_path())
            .is_some_and(|flow| flow.id == id && matches!(flow.stage, Stage::Pending { .. }))
    }

    pub(super) fn cancel(&self, control: &ControlStorage) {
        let mut slots = self.slots.lock().expect("console login flows");
        let root = control.root_path();
        if slots.get(root).is_some_and(|flow| flow.stage.is_active()) {
            cancel_flow(slots.remove(root));
        }
    }

    pub(super) fn remove(&self, control: &ControlStorage) -> Result<(), ApplicationError> {
        let mut slots = self.slots.lock().expect("console login flows");
        remove_credential(control, self.provider)?;
        cancel_flow(slots.remove(control.root_path()));
        Ok(())
    }

    pub(super) fn settle(
        &self,
        control: &ControlStorage,
        id: Uuid,
        result: Result<ConsoleCredentialRow, LoginFailure>,
    ) -> bool {
        let mut slots = self.slots.lock().expect("console login flows");
        let Some(flow) = slots
            .get_mut(control.root_path())
            .filter(|flow| flow.id == id && flow.stage.is_active())
        else {
            return false;
        };
        // Keep the identity check and write in the same critical section as revocation.
        flow.stage = match result {
            Ok(row)
                if control
                    .write_console_credential(self.provider, &row)
                    .is_ok() =>
            {
                invalidate_cache(control, self.provider);
                Stage::Complete
            }
            Ok(_) => Stage::Failed(LoginFailure::Failed),
            Err(error) => Stage::Failed(error),
        };
        true
    }
}

fn cancel_flow(flow: Option<Flow>) {
    if let Some(Flow {
        stage: Stage::Pending {
            cancel: Some(cancel),
            ..
        },
        ..
    }) = flow
    {
        cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(label: &str) -> (PathBuf, ControlStorage) {
        let (root, _) = crate::test_support::roots(label);
        std::fs::create_dir_all(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let control = ControlStorage::open_ready(&root).unwrap();
        (root, control)
    }

    fn start(flows: &LoginFlows, control: &ControlStorage) -> Uuid {
        let Start::New(id) = flows.start(control).unwrap() else {
            panic!("expected new login");
        };
        id
    }

    fn credential() -> ConsoleCredentialRow {
        ConsoleCredentialRow {
            blob: seal("test credential").unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn settle_stores_the_credential_only_for_the_current_flow() {
        let (root, control) = store("console-callback-settlement");
        let cancelled = start(&CALLBACK, &control);
        CALLBACK
            .publish(&control, cancelled, json!({}), None)
            .unwrap();
        CALLBACK.cancel(&control);
        assert!(!CALLBACK.settle(&control, cancelled, Ok(credential())));
        assert_eq!(control.read_console_credential("bailian").unwrap(), None);

        let current = start(&CALLBACK, &control);
        let view = json!({"url": "fixture"});
        CALLBACK
            .publish(&control, current, view.clone(), None)
            .unwrap();
        assert!(
            matches!(CALLBACK.start(&control).unwrap(), Start::Pending(existing) if existing == view)
        );
        assert!(
            CALLBACK
                .publish(&control, cancelled, json!({}), None)
                .is_err()
        );
        assert!(!CALLBACK.settle(&control, cancelled, Ok(credential())));
        assert_eq!(CALLBACK.status(&control), "pending");
        assert_eq!(control.read_console_credential("bailian").unwrap(), None);

        assert!(CALLBACK.settle(&control, current, Ok(credential())));
        assert_eq!(CALLBACK.status(&control), "complete");
        assert!(
            control
                .read_console_credential("bailian")
                .unwrap()
                .is_some()
        );
        assert!(!CALLBACK.settle(&control, current, Err(LoginFailure::Expired)));
        CALLBACK.remove(&control).unwrap();
        drop(control);
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }

    #[test]
    fn console_login_flows_are_isolated_by_provider_and_storage_root() {
        let (first, one) = store("console-first-root");
        let (second, two) = store("console-second-root");
        let device = start(&DEVICE, &one);
        let other_device = start(&DEVICE, &two);
        let callback = start(&CALLBACK, &one);
        DEVICE.publish(&one, device, json!({}), None).unwrap();
        DEVICE.publish(&two, other_device, json!({}), None).unwrap();
        CALLBACK.publish(&one, callback, json!({}), None).unwrap();
        assert!(!DEVICE.settle(&two, device, Ok(credential())));

        DEVICE.remove(&one).unwrap();
        assert!(!DEVICE.settle(&one, device, Ok(credential())));
        assert!(DEVICE.settle(&two, other_device, Ok(credential())));
        assert!(CALLBACK.settle(&one, callback, Ok(credential())));
        assert_eq!(one.read_console_credential("qianwen").unwrap(), None);
        assert!(one.read_console_credential("bailian").unwrap().is_some());
        assert!(two.read_console_credential("qianwen").unwrap().is_some());
        CALLBACK.remove(&one).unwrap();
        DEVICE.remove(&two).unwrap();
        drop((one, two));
        crate::test_support::cleanup_tree(first.parent().unwrap());
        crate::test_support::cleanup_tree(second.parent().unwrap());
    }

    #[test]
    fn console_login_reports_failed_storage_and_can_start_again() {
        let (root, control) = store("console-storage-failure");
        let id = start(&CALLBACK, &control);
        CALLBACK.publish(&control, id, json!({}), None).unwrap();
        let oversized = ConsoleCredentialRow {
            blob: "x".repeat(256 * 1024 + 1),
            ..Default::default()
        };
        assert!(CALLBACK.settle(&control, id, Ok(oversized)));
        assert_eq!(CALLBACK.status(&control), "failed");
        assert_eq!(control.read_console_credential("bailian").unwrap(), None);
        assert!(!CALLBACK.settle(&control, id, Ok(credential())));
        let next = start(&CALLBACK, &control);
        assert!(CALLBACK.settle(&control, next, Ok(credential())));
        CALLBACK.remove(&control).unwrap();
        drop(control);
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }

    #[test]
    fn console_login_removal_failure_preserves_the_flow_for_retry() {
        let (root, control) = store("console-remove-failure");
        let id = start(&CALLBACK, &control);
        CALLBACK.publish(&control, id, json!({}), None).unwrap();
        let file = control.root_path().join("console-credentials.json");
        std::fs::write(&file, "invalid credential file").unwrap();
        assert!(CALLBACK.remove(&control).is_err());
        assert!(CALLBACK.is_pending(&control, id));
        std::fs::remove_file(file).unwrap();
        CALLBACK.remove(&control).unwrap();
        assert!(!CALLBACK.settle(&control, id, Ok(credential())));
        drop(control);
        crate::test_support::cleanup_tree(root.parent().unwrap());
    }
}
