//! Interactive native acceptance fixture. Its window never edits external state.
use super::*;
use ::windows::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    System::StationsAndDesktops::*,
    UI::WindowsAndMessaging::*,
};
use ::windows::core::{PCWSTR, w};
use std::sync::{atomic::AtomicUsize, mpsc};

static PRESSES: AtomicUsize = AtomicUsize::new(0);
const CLOSE_FIXTURE: u32 = WM_APP + 1;

struct Fixture {
    raw: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Fixture {
    fn open() -> Self {
        let (reply, ready) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let root = create_window();
            reply.send(root.0 as isize).unwrap();
            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
                unsafe { DispatchMessageW(&message) };
            }
        });
        Self {
            raw: ready.recv_timeout(Duration::from_secs(5)).unwrap(),
            thread: Some(thread),
        }
    }

    fn close(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe {
                PostMessageW(
                    Some(HWND(self.raw as *mut _)),
                    CLOSE_FIXTURE,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .unwrap();
            thread.join().unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe extern "system" fn message(hwnd: HWND, id: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match id {
        WM_COMMAND if wparam.0 & 0xffff == 1 => {
            PRESSES.fetch_add(1, Ordering::Release);
            LRESULT(0)
        }
        CLOSE_FIXTURE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, id, wparam, lparam) },
    }
}

fn create_window() -> HWND {
    let name = w!("ClatUiaAcceptance");
    let instance = unsafe { ::windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
        .unwrap()
        .into();
    let class = WNDCLASSW {
        lpszClassName: name,
        hInstance: instance,
        lpfnWndProc: Some(message),
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };
    let root = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            name,
            w!("CLAT UIA test fixture"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            440,
            280,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .unwrap();
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Fixture button"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            15,
            15,
            180,
            30,
            Some(root),
            Some(HMENU(1 as *mut _)),
            Some(instance),
            None,
        )
    }
    .unwrap();
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!("Fixture password"),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_PASSWORD as u32),
            15,
            60,
            180,
            30,
            Some(root),
            None,
            Some(instance),
            None,
        )
    }
    .unwrap();
    add_choice_controls(root, instance);
    root
}

fn add_choice_controls(root: HWND, instance: HINSTANCE) {
    let add = |class: PCWSTR, style, id, y, message, name: PCWSTR| {
        let control = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(style),
                220,
                y,
                160,
                100,
                Some(root),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
        }
        .unwrap();
        let inserted =
            unsafe { SendMessageW(control, message, None, Some(LPARAM(name.as_ptr() as isize))) };
        assert_eq!(inserted.0, 0);
    };
    add(
        w!("LISTBOX"),
        LBS_NOTIFY as u32,
        2usize,
        12,
        LB_ADDSTRING,
        w!("Fixture list item"),
    );
    add(
        w!("COMBOBOX"),
        CBS_DROPDOWNLIST as u32,
        3usize,
        125,
        CB_ADDSTRING,
        w!("Fixture combo option"),
    );
}

#[test]
#[ignore = "requires an unlocked interactive Windows desktop and a non-elevated test process"]
fn cua_live_windows_uia_retains_window_identity_and_dispatches_semantic_actions() {
    Windows.ready().unwrap();
    let mut fixture = Fixture::open();
    let app = Windows.app(std::process::id() as i32).unwrap();
    let call = deadline(&CancelToken::new(), Duration::from_secs(10));
    let fixture_root = std::env::temp_dir().join(format!("clat-uia-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&fixture_root).unwrap();
    crate::control_storage::sentinel::initialize(&fixture_root).unwrap();
    let storage = Arc::new(ControlStorage::open_ready(&fixture_root).unwrap());
    let service = Service::new(storage, AskUserSlot::shared(), Arc::new(Windows));
    service.begin("live-windows".into(), &call, ToolAccessPolicy::all(), None);
    let run = service.run(&call).unwrap();
    let (window, tree) = observe_fixture(&service, &run, &app, &call);
    perform_fixture_action(
        &run,
        &tree,
        "press",
        |node| node["title"] == "Fixture button",
        &call,
    );
    verify_press();
    perform_fixture_action(
        &run,
        &tree,
        "pick",
        |node| node["title"] == "Fixture list item",
        &call,
    );
    assert_eq!(fixture_state(fixture.raw, 2, LB_GETCURSEL), 0);
    perform_fixture_action(
        &run,
        &tree,
        "show_menu",
        |node| node["role"] == "ComboBox",
        &call,
    );
    assert_ne!(fixture_state(fixture.raw, 3, CB_GETDROPPEDSTATE), 0);
    fixture.close();
    let reopened = Fixture::open();
    assert!(Windows.describe(&window, &app, &call).is_err());
    drop(reopened);
    service.end();
    drop(service);
    crate::test_support::cleanup_tree(&fixture_root);
    check_desktop_switch();
}

fn observe_fixture(service: &Service, run: &Run, app: &App, call: &CancelToken) -> (Handle, Value) {
    let applications = inventory(call).unwrap();
    assert!(
        applications["applications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["pid"] == app.pid && entry["window_count"].as_u64().unwrap_or(0) > 0
            })
    );
    let overview = service
        .tree(run, api::decode(&json!({"pid":app.pid})).unwrap())
        .unwrap();
    let node = &overview["nodes"][1];
    let id = overview["snapshot_id"].as_str().unwrap();
    let index = node["index"].as_u64().unwrap() as usize;
    let window = tree::get_observation(run, id, index).unwrap().2.node.handle;
    let wait = json!({"snapshot_id":id,"element_index":index,"condition":"appear","title":"Fixture button"});
    assert_eq!(
        service
            .wait_for_condition(run, api::decode(&wait).unwrap())
            .unwrap()["matched"],
        true
    );
    let tree = service
        .tree(
            run,
            api::decode(&json!({"snapshot_id":id,"element_index":index})).unwrap(),
        )
        .unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    let password = nodes.iter().find(|node| node["role"] == "Edit").unwrap();
    assert_eq!(password["value_state"], "secure");
    assert_eq!(password["title"], "");
    assert!(password.get("value").is_none());
    (window, tree)
}

fn perform_fixture_action(
    run: &Run,
    tree: &Value,
    action: &'static str,
    matches: impl Fn(&Value) -> bool,
    call: &CancelToken,
) {
    let node = tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| matches(node))
        .unwrap_or_else(|| panic!("fixture target missing for {action}: {tree}"));
    assert!(
        node["actions"].as_array().unwrap().contains(&json!(action)),
        "{node}"
    );
    let index = node["index"].as_u64().unwrap() as usize;
    let snapshot = tree["snapshot_id"].as_str().unwrap();
    let observed = tree::get_observation(run, snapshot, index).unwrap().2;
    let handle = Reference::retained(&observed.node.handle).unwrap();
    // The production action fence protects this process. Exercise the native
    // operation on the fixture below that fence.
    worker::run(call, move |uia, call, dispatch| {
        uia.perform_fixture(&handle, action, call, dispatch)
    })
    .unwrap();
}

fn verify_press() {
    let until = Instant::now() + Duration::from_secs(2);
    while PRESSES.load(Ordering::Acquire) == 0 && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(PRESSES.load(Ordering::Acquire), 1);
}

fn fixture_state(window: isize, id: i32, message: u32) -> isize {
    let control = unsafe { GetDlgItem(Some(HWND(window as *mut _)), id) }.unwrap();
    unsafe { SendMessageW(control, message, None, None) }.0
}

struct TestDesktop {
    original: HDESK,
    temporary: HDESK,
}

impl Drop for TestDesktop {
    fn drop(&mut self) {
        unsafe { SetThreadDesktop(self.original) }.expect("restore the test thread desktop");
        let _ = unsafe { CloseDesktop(self.temporary) };
    }
}

fn check_desktop_switch() {
    // Put just the test thread on a desktop that cannot receive input.
    // The VM user's active desktop and sign-in state are untouched.
    let flags = DESKTOP_CONTROL_FLAGS(0);
    let original =
        unsafe { GetThreadDesktop(::windows::Win32::System::Threading::GetCurrentThreadId()) }
            .unwrap();
    let name = format!("CLAT-UIA-{}\0", uuid::Uuid::new_v4())
        .encode_utf16()
        .collect::<Vec<_>>();
    let temporary = unsafe {
        CreateDesktopW(
            ::windows::core::PCWSTR(name.as_ptr()),
            None,
            None,
            flags,
            DESKTOP_CREATEWINDOW.0 | DESKTOP_READOBJECTS.0,
            None,
        )
    }
    .unwrap();
    let desktop = TestDesktop {
        original,
        temporary,
    };
    unsafe { SetThreadDesktop(desktop.temporary) }.unwrap();
    let error = Windows.ready().unwrap_err();
    assert_eq!(
        error.recovery().unwrap()["code"],
        "WINDOWS_DESKTOP_UNAVAILABLE"
    );
    drop(desktop);
    Windows.ready().unwrap();
}
