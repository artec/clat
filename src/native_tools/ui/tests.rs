use super::api::{Operation, UiTool};
use super::*;
use crate::Project;
use crate::control_storage::ui_consents::Decision;
use crate::interaction::{AskAnswer, AskQuestion};
use crate::permission::{
    InteractivePermissionPolicy, ModePolicy, PermissionDecision, PermissionMode, PermissionPolicy,
    PermissionRequest,
};
use crate::tool::{Tool, ToolCall, prepare_tool};
use std::sync::atomic::{AtomicUsize, Ordering};

const APP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

type ValueReadHook = Box<dyn FnOnce(&CancelToken) + Send>;

#[derive(Default)]
struct Fake {
    facts: Mutex<Facts>,
    values: AtomicUsize,
    dispatches: AtomicUsize,
    on_value_read: Mutex<Option<ValueReadHook>>,
}

struct Facts {
    pid: i32,
    start: u64,
    kind: ApplicationKind,
    title: String,
    value: String,
    value_unavailable: bool,
    count: usize,
    window_scoped: bool,
    current_window: usize,
    supported: bool,
    label_complete: bool,
    role: String,
    resolved_node: usize,
    unknown_dispatch: bool,
    cancel_on_root: Option<CancelToken>,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            pid: 42,
            start: 1,
            kind: ApplicationKind::Other,
            title: "Open folder".into(),
            value: "ordinary value".into(),
            value_unavailable: false,
            count: 2,
            window_scoped: false,
            current_window: 1,
            supported: true,
            label_complete: true,
            role: "AXButton".into(),
            resolved_node: 2,
            unknown_dispatch: false,
            cancel_on_root: None,
        }
    }
}

fn index(node: &Handle) -> usize {
    *node.downcast_ref::<usize>().unwrap()
}

impl Backend for Fake {
    fn window_scoped(&self) -> bool {
        self.facts.lock().unwrap().window_scoped
    }

    fn ready(&self) -> Result<()> {
        Ok(())
    }
    fn app(&self, pid: i32) -> Result<App> {
        let facts = self.facts.lock().unwrap();
        if facts.pid != pid {
            return Err(error("STALE_APP"));
        }
        Ok(App {
            pid,
            identity: APP.into(),
            process_identity: facts.start.to_string(),
            label: "Example".into(),
            bundle: Some("test.example".into()),
            kind: facts.kind,
        })
    }
    fn root(&self, _: &App, call: &CancelToken) -> Result<Handle> {
        if let Some(token) = &self.facts.lock().unwrap().cancel_on_root {
            token.cancel();
        }
        active(call)?;
        Ok(Arc::new(0_usize))
    }
    fn same(&self, left: &Handle, right: &Handle) -> bool {
        index(left) == index(right)
    }
    fn hash(&self, node: &Handle) -> u64 {
        index(node) as u64
    }
    fn children(
        &self,
        node: &Handle,
        offset: usize,
        count: usize,
        windows: bool,
        _: &CancelToken,
    ) -> Result<Vec<Handle>> {
        let range = match (index(node), windows) {
            (0, true) => 1..2,
            (1, false) => 2..self.facts.lock().unwrap().count + 2,
            _ => 0..0,
        };
        Ok(range
            .skip(offset)
            .take(count)
            .map(|index| Arc::new(index) as Handle)
            .collect())
    }
    fn menu_bar(&self, _: &Handle, _: &CancelToken) -> Result<Option<Handle>> {
        Ok(None)
    }
    fn describe(&self, node: &Handle, _: &App, _: &CancelToken) -> Result<Node> {
        let facts = self.facts.lock().unwrap();
        let index = index(node);
        let (role, title, subrole) = match index {
            0 if facts.window_scoped => ("UIAApplication", "Example".into(), None),
            0 => ("AXApplication", "Example".into(), None),
            1 => ("AXWindow", "Example window".into(), None),
            2 => (facts.role.as_str(), facts.title.clone(), None),
            3 => (
                "AXTextField",
                String::new(),
                Some("AXSecureTextField".into()),
            ),
            _ => ("AXGroup", String::new(), None),
        };
        Ok(Node {
            handle: if index == 2 {
                Arc::new(facts.resolved_node)
            } else {
                node.clone()
            },
            role: role.into(),
            title,
            subrole,
            description: None,
            actions: if index == 2 && facts.supported {
                vec!["press".into()]
            } else {
                Vec::new()
            },
            label_complete: index != 2 || facts.label_complete,
            enabled: Some(true),
            focused: Some(index == 2),
            selected: None,
            expanded: None,
        })
    }
    fn value(
        &self,
        node: &Node,
        purpose: ValuePurpose,
        cancel: &CancelToken,
    ) -> Result<Option<String>> {
        assert!(
            !node.secure(),
            "a secure field reached the platform value reader"
        );
        self.values.fetch_add(1, Ordering::Relaxed);
        let hook = self.on_value_read.lock().unwrap().take();
        if let Some(hook) = hook {
            hook(cancel);
        }
        let facts = self.facts.lock().unwrap();
        if index(&node.handle) == 2 && facts.value_unavailable {
            return match purpose {
                ValuePurpose::Read => Err(error("NATIVE_ERROR")),
                ValuePurpose::RepeatGuard => Ok(None),
            };
        }
        Ok((index(&node.handle) == 2).then(|| facts.value.clone()))
    }
    fn window(&self, node: &Node, _: &App, _: &CancelToken) -> Result<Option<Handle>> {
        Ok((index(&node.handle) != 0)
            .then(|| Arc::new(self.facts.lock().unwrap().current_window) as Handle))
    }
    fn dispatch(&self, _: &Node, _: &str, cancel: &CancelToken) -> Result<()> {
        active(cancel)?;
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        if self.facts.lock().unwrap().unknown_dispatch {
            Err(error("DISPATCH_UNKNOWN"))
        } else {
            Ok(())
        }
    }
    fn target_fence(&self, standalone: bool) -> TargetFence {
        TargetFence {
            protected_pids: vec![99],
            protected_apps: Vec::new(),
            independent_local_tui: standalone,
        }
    }
}

type Answer = dyn Fn(&CancelToken) -> AskAnswer + Send + Sync;

struct Asker {
    calls: AtomicUsize,
    answer: Mutex<Arc<Answer>>,
    standalone: bool,
}

impl UserAsker for Asker {
    fn ask(&self, question: AskQuestion, cancel: &CancelToken) -> AskAnswer {
        assert!(!question.allow_custom);
        assert_eq!(
            question
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            ["session", "always", "deny"]
        );
        self.calls.fetch_add(1, Ordering::Relaxed);
        let answer = self.answer.lock().unwrap().clone();
        answer(cancel)
    }
    fn is_standalone_terminal(&self) -> bool {
        self.standalone
    }
}

struct Fixture {
    root: std::path::PathBuf,
    fake: Arc<Fake>,
    service: Arc<Service>,
    asker: Arc<Asker>,
    cancel: CancelToken,
}

impl Fixture {
    fn new(standalone: bool) -> Self {
        let root = std::env::temp_dir().join(format!("clat-cua-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        crate::control_storage::sentinel::initialize(&root).unwrap();
        let storage = Arc::new(ControlStorage::open_ready(&root).unwrap());
        let asker = Arc::new(Asker {
            calls: 0.into(),
            answer: Mutex::new(Arc::new(|_| AskAnswer::Selected("session".into()))),
            standalone,
        });
        let slot = AskUserSlot::shared();
        slot.install(Some(asker.clone()));
        let fake = Arc::new(Fake::default());
        let service = Arc::new(Service::new(storage, slot, fake.clone()));
        let fixture = Self {
            root,
            fake,
            service,
            asker,
            cancel: CancelToken::new(),
        };
        fixture.begin("first", true, ToolAccessPolicy::all());
        fixture
    }

    fn begin(&self, session: &str, interactive: bool, access: ToolAccessPolicy) {
        self.service.begin(
            session.into(),
            &self.cancel,
            access,
            interactive.then_some(self.asker.as_ref() as &dyn UserAsker),
        );
    }

    #[cfg(target_os = "macos")]
    fn live_read_only() -> Self {
        let mut f = Self::new(false);
        f.service = Arc::new(Service::new(
            f.service.control.clone(),
            f.service.asker.clone(),
            Arc::new(mac::Mac),
        ));
        f.begin("live-read", false, ToolAccessPolicy::all());
        f
    }

    fn read(&self, operation: Operation, arguments: Value) -> Result<Value> {
        self.tool(operation)
            .invoke(&arguments, &Project::new("."), &self.cancel)
    }

    fn tool(&self, operation: Operation) -> UiTool {
        UiTool {
            service: self.service.clone(),
            operation,
        }
    }

    fn target(&self) -> Value {
        let tree = self.read(Operation::Tree, json!({"pid":42})).unwrap();
        json!({"snapshot_id":tree["snapshot_id"],"element_index":2,"action":"press"})
    }

    fn action(
        &self,
        arguments: Value,
        mode: PermissionMode,
        approve: impl Fn(PermissionRequest, &CancelToken) -> PermissionDecision + Send + Sync + 'static,
    ) -> Result<Value> {
        let tool = self.tool(Operation::Act);
        let call = ToolCall {
            id: "cua".into(),
            name: tool.definition().name,
            arguments,
        };
        let definition = tool.definition();
        let (execution, prepared) = prepare_tool(Arc::new(tool), &call.arguments, &self.cancel)?;
        let project = Project::new(".");
        let policy = InteractivePermissionPolicy::with_approver(
            ModePolicy::new(Arc::new(std::sync::RwLock::new(mode))),
            self.cancel.clone(),
            Arc::new(approve),
        );
        if policy.check_prepared(&project, &definition, &call, prepared.as_ref())
            != PermissionDecision::Allow
        {
            return Err(error("ACTION_NOT_APPROVED"));
        }
        execution.invoke(&call.arguments, &project, &self.cancel)
    }

    fn dispatches(&self) -> usize {
        self.fake.dispatches.load(Ordering::Relaxed)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.service.end();
        crate::test_support::cleanup_tree(&self.root);
    }
}

fn code(result: Result<Value>, expected: &str) {
    let error = result.unwrap_err();
    assert_eq!(error.recovery().unwrap()["code"], expected, "{error}");
}

fn allow(_: PermissionRequest, _: &CancelToken) -> PermissionDecision {
    PermissionDecision::Allow
}

#[test]
fn cua_tree_pages_preserve_indices_and_allow_scoped_reads() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().count = 249;
    let mut page = f.read(Operation::Tree, json!({"pid":42})).unwrap();
    let id = page["snapshot_id"].clone();
    for offset in [0, 100, 200] {
        let rows = page["nodes"].as_array().unwrap();
        assert_eq!(rows.len(), (251_usize - offset).min(100));
        assert_eq!(rows[0]["index"], offset);
        if offset < 200 {
            assert_eq!(page["has_more"], true);
            page = f
                .read(
                    Operation::Tree,
                    json!({"snapshot_id":id,"cursor":page["next_cursor"]}),
                )
                .unwrap();
        }
    }
    assert_eq!(page["has_more"], false);
    assert_eq!(f.asker.calls.load(Ordering::Relaxed), 0);
    assert_eq!(f.fake.values.load(Ordering::Relaxed), 0);
    let window = f
        .read(Operation::Tree, json!({"pid":42,"window_index":0}))
        .unwrap();
    assert_eq!(window["nodes"][0]["role"], "AXWindow");
    let subtree = f
        .read(Operation::Tree, json!({"snapshot_id":id,"element_index":2}))
        .unwrap();
    assert_eq!(subtree["nodes"][0]["title"], "Open folder");
    f.begin("second", true, ToolAccessPolicy::all());
    code(
        f.read(Operation::Tree, json!({"snapshot_id":id,"element_index":2})),
        "STALE_SNAPSHOT",
    );
}

#[test]
fn cua_tree_overview_pages_keep_scope_and_actionable_indices() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().count = 249;
    for request in [
        json!({"pid":42,"page_size":0}),
        json!({"pid":42,"page_size":101}),
        json!({"pid":42,"max_depth":65}),
    ] {
        code(f.read(Operation::Tree, request), "INVALID_ARGUMENTS");
    }

    let app = f
        .read(
            Operation::Tree,
            json!({"pid":42,"max_depth":1,"page_size":1}),
        )
        .unwrap();
    assert_eq!(app["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(app["next_cursor"], 1);
    let continuation = json!({"snapshot_id":app["snapshot_id"],"cursor":app["next_cursor"]});
    let mut changed_scope = continuation.clone();
    changed_scope["max_depth"] = json!(2);
    code(f.read(Operation::Tree, changed_scope), "INVALID_ARGUMENTS");
    let window = f.read(Operation::Tree, continuation).unwrap();
    assert_eq!(window["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(window["nodes"][0]["role"], "AXWindow");
    assert_eq!(window["nodes"][0]["parent_index"], 0);
    assert_eq!(window["has_more"], false);

    let subtree = f
        .read(
            Operation::Tree,
            json!({
                "snapshot_id":app["snapshot_id"],"element_index":window["nodes"][0]["index"],
                "max_depth":1,"page_size":2
            }),
        )
        .unwrap();
    let control = &subtree["nodes"][1];
    assert_eq!(control["role"], "AXButton");
    assert_eq!(control["title"], "Open folder");
    assert_eq!(control["parent_index"], 0);
    assert_eq!(subtree["next_cursor"], 2);
    assert_eq!(f.fake.values.load(Ordering::Relaxed), 0);
    assert_eq!(f.asker.calls.load(Ordering::Relaxed), 0);
    let dispatched = f
        .action(
            json!({"snapshot_id":subtree["snapshot_id"],"element_index":control["index"],"action":"press"}),
            PermissionMode::ReadOnly,
            allow,
        )
        .unwrap();
    assert_eq!(dispatched["dispatched"], true);
}

#[test]
fn cua_tree_limits_return_the_last_valid_page_and_are_inherited() {
    let f = Fixture::new(false);
    for max_nodes in [0, 8193] {
        code(
            f.read(Operation::Tree, json!({"pid":42,"max_nodes":max_nodes})),
            "INVALID_ARGUMENTS",
        );
    }
    let first = f
        .read(
            Operation::Tree,
            json!({"pid":42,"max_nodes":2,"page_size":1}),
        )
        .unwrap();
    assert_eq!(first["next_cursor"], 1);
    assert_eq!(first["limits_reached"], json!([]));
    let id = &first["snapshot_id"];
    code(
        f.read(
            Operation::Tree,
            json!({"snapshot_id":id,"cursor":1,"max_nodes":3}),
        ),
        "INVALID_ARGUMENTS",
    );
    let last = f
        .read(Operation::Tree, json!({"snapshot_id":id,"cursor":1}))
        .unwrap();
    assert_eq!(last["nodes"][0]["role"], "AXWindow");
    assert_eq!(last["has_more"], false);
    assert_eq!(last["limits_reached"], json!(["max_nodes"]));
    let shallow = f
        .read(Operation::Tree, json!({"pid":42,"max_depth":1}))
        .unwrap();
    assert_eq!(shallow["limits_reached"], json!(["max_depth"]));
    let exact = f
        .read(Operation::Tree, json!({"pid":42,"max_nodes":4}))
        .unwrap();
    assert_eq!(exact["limits_reached"], json!([]));
}

#[test]
fn cua_wait_cannot_infer_disappearance_from_a_truncated_tree() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().count = 8192;
    code(
        f.read(
            Operation::Wait,
            json!({"pid":42,"condition":"disappear","title":"Absent"}),
        ),
        "TREE_LIMIT",
    );
    let page = f
        .read(
            Operation::Tree,
            json!({"pid":42,"page_size":5,"max_nodes":5}),
        )
        .unwrap();
    assert_eq!(page["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(page["limits_reached"], json!(["max_nodes"]));
}

#[test]
fn cua_window_scoped_backend_pins_a_window_from_the_inventory() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().window_scoped = true;
    let overview = f.read(Operation::Tree, json!({"pid":42})).unwrap();
    assert_eq!(overview["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(overview["nodes"][1]["role"], "AXWindow");
    code(
        f.read(
            Operation::Wait,
            json!({"pid":42,"condition":"appear","title":"Open folder"}),
        ),
        "INVALID_ARGUMENTS",
    );
    let id = overview["snapshot_id"].clone();
    let request =
        json!({"snapshot_id":id,"element_index":1,"condition":"appear","title":"Open folder"});
    assert_eq!(
        f.read(Operation::Wait, request.clone()).unwrap()["matched"],
        true
    );
    let page = f
        .read(
            Operation::Tree,
            json!({"snapshot_id":id,"element_index":1,"page_size":1}),
        )
        .unwrap();
    f.fake.facts.lock().unwrap().current_window = 99;
    code(f.read(Operation::Wait, request), "STALE_SNAPSHOT");
    code(
        f.read(
            Operation::Tree,
            json!({"snapshot_id":page["snapshot_id"],"cursor":page["next_cursor"]}),
        ),
        "STALE_SNAPSHOT",
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires Accessibility access and CLAT_CUA_TEST_PIDS for open windows with splitters"]
fn cua_live_macos_splitter_does_not_abort_tree_read() {
    let f = Fixture::live_read_only();
    let input = std::env::var("CLAT_CUA_TEST_PIDS").unwrap();
    let pids: Vec<i32> = input
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert!(!pids.is_empty());
    for pid in pids {
        for arguments in [json!({"pid":pid}), json!({"pid":pid,"window_index":0})] {
            let page = f
                .read(Operation::Tree, arguments.clone())
                .unwrap_or_else(|err| panic!("{arguments}: {err}"));
            let nodes = page["nodes"].as_array().unwrap();
            assert!(
                nodes.iter().any(|node| node["role"] == "AXSplitter"),
                "{arguments}: no splitter in the first page"
            );
            if page["has_more"] == true {
                let next = f
                    .read(
                        Operation::Tree,
                        json!({
                            "snapshot_id":page["snapshot_id"], "cursor":page["next_cursor"]
                        }),
                    )
                    .unwrap_or_else(|err| panic!("{arguments} continuation: {err}"));
                assert_eq!(next["nodes"][0]["index"], page["next_cursor"]);
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires Accessibility access and CLAT_CUA_FINDER_PID with a Finder window open"]
fn cua_live_macos_finder_action_value_is_optional() {
    let f = Fixture::live_read_only();
    let pid: i32 = std::env::var("CLAT_CUA_FINDER_PID")
        .unwrap()
        .parse()
        .unwrap();
    let page = f
        .read(Operation::Tree, json!({"pid":pid,"window_index":0}))
        .unwrap();
    let run = f.service.run(&f.cancel).unwrap();
    let snapshot = page["snapshot_id"].as_str().unwrap();
    let backend = f.service.backend.as_ref();
    for (role, action) in [("AXWindow", "raise"), ("AXOutline", "show_menu")] {
        let found = page["nodes"].as_array().unwrap().iter().any(|row| {
            if row["role"] != role
                || !row["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a == action)
            {
                return false;
            }
            let index = row["index"].as_u64().unwrap() as usize;
            let (app, lease, observed) = tree::get_observation(&run, snapshot, index).unwrap();
            let call = deadline(&lease.cancel, Duration::from_secs(2));
            let Err(err) = backend.value(&observed.node, ValuePurpose::Read, &call) else {
                return false;
            };
            assert_eq!(err.recovery().unwrap()["code"], "NATIVE_ERROR");
            assert!(
                backend
                    .value(&observed.node, ValuePurpose::RepeatGuard, &call)
                    .unwrap()
                    .is_none()
            );
            let live = backend
                .describe(&observed.node.handle, &app, &call)
                .unwrap();
            assert!(live.actions.iter().any(|item| item == action));
            let window = backend.window(&live, &app, &call).unwrap().unwrap();
            assert!(backend.same(observed.window.as_ref().unwrap(), &window));
            true
        });
        assert!(found, "no {role} with {action} and unavailable AXValue");
    }
}

#[test]
fn cua_value_consent_is_session_scoped_and_secure_values_never_reach_the_backend() {
    let f = Fixture::new(false);
    *f.asker.answer.lock().unwrap() = Arc::new(|_| AskAnswer::Selected("deny".into()));
    let request = json!({"pid":42,"include_values":true});
    for _ in 0..2 {
        code(f.read(Operation::Tree, request.clone()), "APP_DENIED");
    }
    assert_eq!(f.asker.calls.load(Ordering::Relaxed), 1);
    assert_eq!(f.fake.values.load(Ordering::Relaxed), 0);

    f.begin("second", true, ToolAccessPolicy::all());
    *f.asker.answer.lock().unwrap() = Arc::new(|_| AskAnswer::Selected("always".into()));
    let result = f.read(Operation::Tree, request.clone()).unwrap();
    assert_eq!(result["nodes"][2]["value"], "ordinary value");
    assert_eq!(result["nodes"][3]["value_state"], "secure");
    assert!(result["nodes"][3].get("value").is_none());
    let projected = f.tool(Operation::Tree).journal_output(&result);
    assert!(!projected.to_string().contains("ordinary value"));
    f.begin("first", true, ToolAccessPolicy::all());
    code(f.read(Operation::Tree, request.clone()), "APP_DENIED");
    f.begin("third", true, ToolAccessPolicy::all());
    assert!(f.read(Operation::Tree, request).is_ok());
    assert_eq!(f.asker.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn cua_plan_and_headless_preserve_structure_and_reject_sensitive_paths() {
    let f = Fixture::new(false);
    let values = json!({"pid":42,"include_values":true});
    f.read(Operation::Tree, values.clone()).unwrap();
    for (interactive, access, expected) in [
        (true, ToolAccessPolicy::plan_mode(), "PLAN_MODE"),
        (false, ToolAccessPolicy::all(), "APP_APPROVAL_UNAVAILABLE"),
    ] {
        f.begin("first", interactive, access);
        let target = f.target();
        code(f.read(Operation::Tree, values.clone()), expected);
        code(
            f.read(
                Operation::Wait,
                json!({"pid":42,"role":"AXButton","condition":"contains","value":"ordinary"}),
            ),
            expected,
        );
        code(
            f.action(target, PermissionMode::FullAccess, allow),
            expected,
        );
        assert!(
            f.read(
                Operation::Wait,
                json!({"pid":42,"role":"AXButton","condition":"appear"})
            )
            .is_ok()
        );
    }
    assert_eq!(f.dispatches(), 0);
}

#[test]
fn cua_action_policy_and_keyword_floor_use_the_actual_target() {
    for (mode, title, approval_required) in [
        (PermissionMode::ReadOnly, "Open folder", true),
        (PermissionMode::ProjectWrite, "Open folder", true),
        (PermissionMode::FullAccess, "Open folder", false),
        (PermissionMode::FullAccess, "刪除檔案", true),
        (PermissionMode::FullAccess, "Submit payment", true),
    ] {
        let f = Fixture::new(false);
        f.fake.facts.lock().unwrap().title = title.into();
        let result = f.action(f.target(), mode, move |request, _| {
            assert_eq!(request.arguments["target"]["title"], title);
            PermissionDecision::Deny {
                reason: "no".into(),
            }
        });
        if approval_required {
            code(result, "ACTION_NOT_APPROVED");
            assert_eq!(f.dispatches(), 0, "{title} under {mode:?}");
        } else {
            let result = result.unwrap();
            assert_eq!(result["dispatched"], true);
            assert_eq!(result["verified"], false);
            assert_eq!(f.dispatches(), 1);
        }
        assert_eq!(f.asker.calls.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn cua_protected_and_shared_browser_targets_are_fenced_before_asking() {
    for (kind, standalone, expected) in [
        (ApplicationKind::Protected, true, false),
        (ApplicationKind::BrowserOrUnknown, false, false),
        (ApplicationKind::BrowserOrUnknown, true, true),
        (ApplicationKind::Other, false, true),
    ] {
        let f = Fixture::new(standalone);
        f.fake.facts.lock().unwrap().kind = kind;
        let result = f.action(f.target(), PermissionMode::FullAccess, allow);
        if expected {
            assert_eq!(result.unwrap()["dispatched"], true);
        } else {
            code(result, "PROTECTED_TARGET");
            assert_eq!(f.asker.calls.load(Ordering::Relaxed), 0);
            assert_eq!(f.dispatches(), 0);
        }
    }
}

#[test]
fn cua_action_revalidates_pid_window_role_title_and_action_after_approval() {
    type Mutation = (fn(&mut Facts), &'static str);
    let mutations: [Mutation; 7] = [
        (|facts| facts.start += 1, "STALE_APP"),
        (|facts| facts.current_window += 1, "STALE_SNAPSHOT"),
        (|facts| facts.title = "Delete".into(), "STALE_SNAPSHOT"),
        (|facts| facts.role = "AXLink".into(), "STALE_SNAPSHOT"),
        (|facts| facts.resolved_node += 1, "STALE_SNAPSHOT"),
        (|facts| facts.supported = false, "UNSUPPORTED_ACTION"),
        (|facts| facts.label_complete = false, "STALE_SNAPSHOT"),
    ];
    for (mutate, expected) in mutations {
        let f = Fixture::new(false);
        let fake = f.fake.clone();
        code(
            f.action(f.target(), PermissionMode::ReadOnly, move |_, _| {
                mutate(&mut fake.facts.lock().unwrap());
                PermissionDecision::Allow
            }),
            expected,
        );
        assert_eq!(f.dispatches(), 0);
    }
}

#[test]
fn cua_revoking_during_app_consent_cancels_the_prompt_and_discards_a_late_answer() {
    let f = Fixture::new(false);
    let control = f.service.control.clone();
    *f.asker.answer.lock().unwrap() = Arc::new(move |cancel| {
        control.ui_authority.revoke(&control, APP, None).unwrap();
        assert!(cancel.is_cancelled());
        AskAnswer::Selected("always".into())
    });
    let target = f.target();
    code(
        f.action(target.clone(), PermissionMode::FullAccess, allow),
        "APP_ACCESS_REVOKED",
    );
    assert!(f.service.control.ui_consents().unwrap().is_empty());
    code(
        f.action(target, PermissionMode::FullAccess, allow),
        "STALE_SNAPSHOT",
    );
    assert_eq!(f.dispatches(), 0);
}

#[test]
fn cua_revoking_during_action_approval_blocks_even_a_late_allow() {
    let f = Fixture::new(false);
    let control = f.service.control.clone();
    let target = f.target();
    code(
        f.action(
            target.clone(),
            PermissionMode::ProjectWrite,
            move |_, cancel| {
                control
                    .ui_authority
                    .revoke(&control, APP, Some("first"))
                    .unwrap();
                assert!(cancel.is_cancelled());
                PermissionDecision::Allow
            },
        ),
        "ACTION_NOT_APPROVED",
    );
    code(
        f.action(target, PermissionMode::FullAccess, allow),
        "STALE_SNAPSHOT",
    );
    assert_eq!(f.dispatches(), 0);
}

#[test]
fn cua_dispatch_rechecks_stored_consent_even_with_a_live_lease() {
    let f = Fixture::new(false);
    let control = f.service.control.clone();
    // The value read occurs after the initial consent check and before the
    // final dispatch check. Change only storage, leaving the lease live.
    *f.fake.on_value_read.lock().unwrap() = Some(Box::new(move |cancel| {
        assert_eq!(
            control.ui_consent(APP, "first").unwrap(),
            Some(Decision::Allow)
        );
        control
            .set_ui_consent(APP, Some("first"), Decision::Deny)
            .unwrap();
        assert!(!cancel.is_cancelled());
    }));

    code(
        f.action(f.target(), PermissionMode::ReadOnly, allow),
        "APP_ACCESS_REVOKED",
    );
    assert_eq!(f.fake.values.load(Ordering::Relaxed), 1);
    assert_eq!(f.dispatches(), 0);
}

#[test]
fn cua_wait_supports_conditions_timeout_and_run_cancellation() {
    let f = Fixture::new(false);
    for (condition, title, value) in [
        ("appear", "Open folder", None),
        ("disappear", "Missing", None),
        ("focus", "Open folder", None),
        ("value_equals", "Open folder", Some("ordinary value")),
        ("contains", "Open folder", Some("ordinary")),
    ] {
        let mut request = json!({"pid":42,"title":title,"condition":condition});
        if let Some(value) = value {
            request["value"] = json!(value);
        }
        assert_eq!(f.read(Operation::Wait, request).unwrap()["matched"], true);
    }
    code(
        f.read(
            Operation::Wait,
            json!({"pid":42,"title":"Missing","condition":"appear","timeout_ms":20}),
        ),
        "TIMEOUT",
    );
    f.fake.facts.lock().unwrap().cancel_on_root = Some(f.cancel.clone());
    code(
        f.read(
            Operation::Wait,
            json!({"pid":42,"title":"Missing","condition":"appear"}),
        ),
        "CANCELLED",
    );
}

#[test]
fn cua_action_fuse_and_dispatch_budget_are_per_run() {
    let f = Fixture::new(false);
    let target = f.target();
    for _ in 0..3 {
        f.action(target.clone(), PermissionMode::FullAccess, allow)
            .unwrap();
    }
    code(
        f.action(target.clone(), PermissionMode::FullAccess, allow),
        "UNCHANGED",
    );
    for index in 3..20 {
        f.fake.facts.lock().unwrap().value = index.to_string();
        f.action(target.clone(), PermissionMode::FullAccess, allow)
            .unwrap();
    }
    code(f.action(target, PermissionMode::FullAccess, allow), "LIMIT");
    assert_eq!(f.dispatches(), 20);
    f.begin("second", true, ToolAccessPolicy::all());
    f.action(f.target(), PermissionMode::FullAccess, allow)
        .unwrap();
    assert_eq!(f.dispatches(), 21);
}

#[test]
fn cua_action_without_value_still_dispatches_and_trips_repeat_guard() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().value_unavailable = true;
    let target = f.target();
    for _ in 0..3 {
        let result = f
            .action(target.clone(), PermissionMode::ReadOnly, allow)
            .unwrap();
        assert_eq!(result["dispatched"], true);
        assert_eq!(result["verified"], false);
    }
    code(
        f.action(target, PermissionMode::ReadOnly, allow),
        "UNCHANGED",
    );
    assert_eq!(f.dispatches(), 3);
    code(
        f.read(
            Operation::Wait,
            json!({"pid":42,"role":"AXButton",
            "condition":"value_equals","value":"ordinary value"}),
        ),
        "NATIVE_ERROR",
    );
}

#[test]
fn cua_uncertain_dispatch_is_one_shot_and_reports_an_unknown_outcome() {
    let f = Fixture::new(false);
    f.fake.facts.lock().unwrap().unknown_dispatch = true;
    let target = f.target();
    let err = f
        .action(target, PermissionMode::FullAccess, allow)
        .unwrap_err();
    assert_eq!(err.recovery().unwrap()["code"], "DISPATCH_UNKNOWN");
    assert_eq!(err.recovery().unwrap()["retry_safe"], false);
    assert_eq!(err.recovery().unwrap()["dispatched"], Value::Null);
    assert_eq!(f.dispatches(), 1);
}
