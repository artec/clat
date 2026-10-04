use super::*;
use crate::network_resources::component::{Host, HostTask, tests::host};
use wasmtime::component::Resource;
const ORIGIN: &str = "https://typed.example.com";
fn component(body: &str) -> String {
    format!(
        r#"(component
      (core module $m (func (export "probe") (result i32) {body}))
      (core instance $i (instantiate $m))
      (func (export "probe") (result u32) (canon lift (core func $i "probe"))))"#
    )
}
fn probe(
    store: &mut Store<HostState>,
    component: &Component,
    linker: &Linker<HostState>,
) -> wasmtime::Result<u32> {
    let instance = linker.instantiate(&mut *store, component)?;
    store
        .data_mut()
        .owner
        .check()
        .map_err(crate::network_resources::component::trap)?;
    let function = instance.get_typed_func::<(), (u32,)>(&mut *store, "probe")?;
    Ok(function.call(&mut *store, ())?.0)
}
#[test]
fn invocation_fresh_store_reuses_compiled_code_and_preserves_run_budget() {
    let mut lane = Lane::new(
        r#"(component
      (core module $m (global $g (mut i32) (i32.const 0))
        (func (export "probe") (result i32)
          global.get $g i32.const 1 i32.add global.set $g global.get $g))
      (core instance $i (instantiate $m))
      (func (export "probe") (result u32) (canon lift (core func $i "probe"))))"#,
    )
    .unwrap();
    let run = crate::dns_authority::Run::new(std::time::Duration::from_secs(5)).unwrap();
    for _ in 0..64 {
        let (host, _) =
            crate::network_resources::component::tests::host_for_run(ORIGIN, true, &run);
        let result = lane
            .invoke(host, |store, component, linker| {
                let _permit = store
                    .data()
                    .network
                    .scope()
                    .admit_http(std::time::Instant::now(), std::time::Duration::from_secs(1))
                    .unwrap();
                probe(store, component, linker)
            })
            .unwrap();
        assert_eq!(
            result, 1,
            "guest mutable state cannot survive an invocation"
        );
    }
    let (host, _) = crate::network_resources::component::tests::host_for_run(ORIGIN, true, &run);
    lane.invoke(host, |store, _, _| {
        assert!(
            matches!(
                store
                    .data()
                    .network
                    .scope()
                    .admit_http(std::time::Instant::now(), std::time::Duration::from_secs(1)),
                Err(crate::dns_authority::Failure::LimitExceeded)
            ),
            "fresh tool cannot refund run attempts"
        );
        Ok(())
    })
    .unwrap();
    let (host, _) = crate::network_resources::component::tests::host_for_run(ORIGIN, true, &run);
    assert!(
        host.network
            .scope()
            .check_active(std::time::Instant::now() + std::time::Duration::from_secs(1))
            .is_ok()
    );
}
#[test]
fn invocation_boundary_closes_lost_task_and_borrowed_scope_before_store_drop() {
    let engine = Engine::default();
    let (_run, mut host, _) = host(ORIGIN);
    let escaped = host.network.clone();
    let task = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    let _poll = HostTask::subscribe(&mut host, Resource::new_borrow(task.rep()))
        .unwrap()
        .unwrap();
    let mut store = Store::new(&engine, host);
    {
        let mut boundary = Boundary(&mut store);
        boundary.close().unwrap();
    }
    assert!(
        store.data().owner.table.is_empty(),
        "boundary must clear actual resources while Store lives"
    );
    assert_eq!(store.data().owner.len(), 0);
    assert!(
        escaped
            .scope()
            .check_active(std::time::Instant::now() + std::time::Duration::from_secs(1))
            .is_err(),
        "escaped network scope must be revoked"
    );
}
#[test]
fn invocation_unwind_clears_resources_before_store_drop() {
    let engine = Engine::default();
    let (_run, mut host, _) = host(ORIGIN);
    let _lost = Host::dns_start(&mut host, ORIGIN.into(), 1000)
        .unwrap()
        .unwrap();
    let mut store = Store::new(&engine, host);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _boundary = Boundary(&mut store);
        panic!("controlled host unwind");
    }));
    assert!(panic.is_err());
    assert!(
        store.data().owner.table.is_empty(),
        "unwind closes while Store lives"
    );
}
#[test]
fn invocation_run_revocation_interrupts_guest_cpu_without_host_import() {
    let mut lane = Lane::new(component("(loop $spin br $spin) unreachable")).unwrap();
    let (run, host, _) = host(ORIGIN);
    let revoke = run.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        revoke.invalidate();
    });
    let result = lane
        .invoke(host, |store, component, linker| {
            store.set_fuel(1_000_000_000)?;
            probe(store, component, linker)
        })
        .unwrap_err();
    thread.join().unwrap();
    assert!(
        format!("{result:#}").contains("Cancelled"),
        "epoch must check run authority: {result:#}"
    );
}
#[test]
fn invocation_return_cannot_deliver_result_after_run_revoked() {
    let mut lane = Lane::new(component("i32.const 7")).unwrap();
    let (run, host, _) = host(ORIGIN);
    let result = lane.invoke(host, |store, component, linker| {
        let result = probe(store, component, linker)?;
        run.invalidate();
        Ok(result)
    });
    assert!(format!("{:#}", result.unwrap_err()).contains("Cancelled"));
}

#[test]
fn invocation_idle_response_physically_closes_on_finish_drop_and_unwind() {
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let engine = Engine::default();
    for exit in ["finish", "drop", "unwind"] {
        let (origin, server) = crate::network_resources::component::tests::partial_server(b"", 100);
        let (_run, mut host, _) = host(&origin);
        let resolution = crate::dns_authority::test_resolution(
            host.network.scope(),
            crate::dns_authority::Origin::parse(&origin).unwrap(),
            vec!["127.0.0.1".parse().unwrap()],
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        );
        let target = host.insert(resolution).unwrap();
        let task = Host::http_start(
            &mut host,
            target,
            crate::network_resources::component::Request {
                url: origin,
                verb: crate::network_resources::component::Method::Get,
                headers: vec![],
                body: vec![],
                timeout_ms: 1500,
                max_response_bytes: 100,
            },
        )
        .unwrap()
        .unwrap();
        crate::network_resources::component::tests::ready(&mut host, &task);
        let outcome = HostTask::get(&mut host, Resource::new_borrow(task.rep()))
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome,
            crate::network_resources::component::Outcome::Headers(_)
        ));
        // Lose the guest-owned response instead of asking guest dispose to save us.
        let mut store = Store::new(&engine, host);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut boundary = Boundary(&mut store);
            match exit {
                "finish" => boundary.close().unwrap(),
                "unwind" => panic!("controlled idle unwind"),
                _ => {}
            }
        }));
        assert_eq!(unwind.is_err(), exit == "unwind");
        assert!(
            store.data().owner.table.is_empty(),
            "{exit}: actual table must clear before Store Drop"
        );
        server.join().unwrap(); // Store still lives: entity EOF is not masked by its Drop.
    }
}
#[test]
fn invocation_deadline_interrupts_guest_cpu_without_host_import() {
    let mut lane = Lane::new(component("(loop $spin br $spin) unreachable")).unwrap();
    let run = crate::dns_authority::Run::new(std::time::Duration::from_millis(30)).unwrap();
    let (host, _) = crate::network_resources::component::tests::host_for_run(ORIGIN, true, &run);
    let result = lane
        .invoke(host, |store, component, linker| {
            store.set_fuel(1_000_000_000)?;
            probe(store, component, linker)
        })
        .unwrap_err();
    assert!(
        format!("{result:#}").contains("DeadlineExceeded"),
        "epoch must enforce absolute deadline: {result:#}"
    );
}
#[test]
fn invocation_rejects_revoked_entry_without_executing_callback() {
    let mut lane = Lane::new(component("i32.const 7")).unwrap();
    let (run, host, _) = host(ORIGIN);
    run.invalidate();
    let result: wasmtime::Result<()> =
        lane.invoke(host, |_, _, _| panic!("revoked entry cannot execute"));
    assert!(format!("{:#}", result.unwrap_err()).contains("Cancelled"));
}

#[test]
#[ignore = "requires freshly generated native component; private numeric transport fixture only"]
fn invocation_native_lost_wrappers_fresh_guest_and_trap_cleanup() {
    use crate::network_resources::component::{
        TypedClient,
        tests::{host_for_run, partial_server},
    };
    let _lock = crate::http_authority::transport::tests::LOCK
        .lock()
        .unwrap();
    let path = std::env::var("PLG4_NATIVE_TYPED_COMPONENT").expect("build native component");
    let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
    let run = crate::dns_authority::Run::new(std::time::Duration::from_secs(20)).unwrap();
    for _ in 0..2 {
        for order in ["resolution", "return", "throw"] {
            let (origin, server) = if order == "resolution" {
                (ORIGIN.to_string(), None)
            } else {
                let (origin, server) = partial_server(b"", 100);
                (origin, Some(server))
            };
            let (mut host, calls) = host_for_run(&origin, true, &run);
            host.transport_fixture = true;
            let scenario =
                serde_json::json!({"phase":"lifetime", "origin":origin, "order":order}).to_string();
            host.close_probe = Some(Box::new(move |host| {
                assert!(
                    host.owner.table.is_empty(),
                    "native cleanup must precede Store Drop"
                );
                if let Some(server) = server {
                    server.join().unwrap();
                }
            }));
            let result = lane.invoke(host, |store, component, linker| {
                let instance = TypedClient::instantiate(&mut *store, component, linker)?;
                store
                    .data_mut()
                    .owner
                    .check()
                    .map_err(crate::network_resources::component::trap)?;
                let result = instance.call_run(&mut *store, &scenario);
                assert_eq!(
                    store.data().owner.len(),
                    1,
                    "guest really abandoned an owned wrapper"
                );
                result
            });
            if order == "throw" {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result.unwrap(),
                    if order == "resolution" {
                        "lost-resolution"
                    } else {
                        "lost-response"
                    }
                );
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "fixture never invokes system DNS"
            );
            println!("NATIVE_INVOCATION {order}");
        }
    }
}

#[test]
fn invocation_guest_error_and_host_unwind_close_without_store_drop_masking() {
    let mut lane = Lane::new(component("i32.const 7")).unwrap();
    for unwind in [false, true] {
        let (_run, mut host, _) = host(ORIGIN);
        let observed = Arc::new(AtomicBool::new(false));
        let seen = observed.clone();
        host.close_probe = Some(Box::new(move |host| {
            assert!(
                host.owner.table.is_empty(),
                "error/unwind cleanup before Store Drop"
            );
            seen.store(true, Ordering::SeqCst);
        }));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lane.invoke(host, |store, _, _| -> wasmtime::Result<()> {
                let _lost = Host::dns_start(store.data_mut(), ORIGIN.into(), 1000)?.unwrap();
                if unwind {
                    panic!("controlled invoke unwind");
                }
                wasmtime::bail!("controlled guest failure")
            })
        }));
        assert_eq!(result.is_err(), unwind);
        if let Ok(result) = result {
            assert!(result.is_err());
        }
        assert!(observed.load(Ordering::SeqCst));
    }
}
#[test]
fn invocation_memory_limit_applies_before_component_initialization() {
    let bytes = r#"(component (core module $m
      (memory 4112) (func (export "probe") (result i32) i32.const 7))
      (core instance $i (instantiate $m))
      (func (export "probe") (result u32) (canon lift (core func $i "probe"))))"#;
    let mut lane = Lane::new(bytes).unwrap();
    let (_run, mut host, calls) = host(ORIGIN);
    let observed = Arc::new(AtomicBool::new(false));
    let seen = observed.clone();
    host.close_probe = Some(Box::new(move |host| {
        assert!(host.owner.table.is_empty());
        seen.store(true, Ordering::SeqCst);
    }));
    let error = lane.invoke(host, probe).unwrap_err();
    assert!(format!("{error:#}").contains("memory"));
    assert!(
        observed.load(Ordering::SeqCst),
        "initialization failure must close the same boundary"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn invocation_second_memory_cannot_multiply_guest_memory_budget() {
    let bytes = r#"(component (core module $m
      (memory 1) (memory 1) (func (export "probe") (result i32) i32.const 7))
      (core instance $i (instantiate $m))
      (func (export "probe") (result u32) (canon lift (core func $i "probe"))))"#;
    let mut lane = Lane::new(bytes).unwrap();
    let (_run, host, _) = host(ORIGIN);
    let error = lane.invoke(host, probe).unwrap_err();
    assert!(
        format!("{error:#}").contains("memor"),
        "second memory must be rejected: {error:#}"
    );
}

#[test]
#[ignore = "requires freshly generated second-flavor scope component; no DNS"]
fn invocation_explicit_scope_component_preserves_original_lifecycle() {
    use crate::network_resources::component::TypedClient;
    let path = std::env::var("PLG4_SCOPE_COMPONENT").expect("build-scope-component.mjs output");
    let mut lane = Lane::new(std::fs::read(path).unwrap()).unwrap();
    for _ in 0..2 {
        let (_run, host, calls) = host(ORIGIN);
        let result = lane
            .invoke(host, |store, component, linker| {
                let instance = TypedClient::instantiate(&mut *store, component, linker)?;
                instance.call_run(&mut *store, "scope")
            })
            .unwrap();
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            result["trace"],
            serde_json::json!([
                "a-last",
                "a-failure",
                "a-second",
                "a-first",
                "event-b",
                "b-last",
                "b-failure",
                "b-second",
                "b-first"
            ])
        );
        assert_eq!(result["failures"], 2);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "scope proof never performs DNS"
        );
    }
}

pub(super) fn original_quartet_lists_and_denies_forged_origin(lane: &mut Lane) {
    use crate::network_resources::component::TypedClient;
    for phase in ["list", "scope", "denied"] {
        let denied = phase == "denied";
        let (_run, host, calls) = host(ORIGIN);
        let scenario = if denied {
            serde_json::json!({"origins":["https://undeclared.invalid"],"name":"web_fetch",
                "arguments":{"url":"https://undeclared.invalid/path"}})
        } else {
            serde_json::json!({"phase":phase})
        }
        .to_string();
        let result = lane.invoke(host, |store, component, linker| {
            let instance = TypedClient::instantiate(&mut *store, component, linker)?;
            let result = instance.call_run(&mut *store, &scenario);
            if result.is_err() {
                println!(
                    "OFFICIAL_DIAGNOSTIC {}",
                    String::from_utf8_lossy(&store.data().owner.diagnostics.contents())
                );
            }
            assert_eq!(store.data().dns_submitted, u32::from(denied));
            assert_eq!(store.data().http_submitted, 0);
            result
        });
        if denied {
            let packet: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
            println!("OFFICIAL_DENIAL {packet}");
            let failure = packet["failure"].to_string();
            assert!(failure.contains("capability-denied"), "{failure}");
        } else {
            let expected = if phase == "list" {
                serde_json::json!(["web_search", "web_fetch"])
            } else {
                serde_json::json!({"owned":true,"rootProviderRetained":true})
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&result.unwrap()).unwrap(),
                expected
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "forged guest origins never authorize DNS"
        );
    }
}
