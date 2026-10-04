use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};
fn instantiate(
    path: &str,
) -> (
    crate::dns_authority::Run,
    Store<HostState>,
    TypedClient,
    Arc<AtomicUsize>,
) {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_opt_level(wasmtime::OptLevel::None);
    let engine = Engine::new(&config).unwrap();
    let component = Component::from_file(&engine, path).unwrap();
    let mut linker = Linker::new(&engine);
    HostState::link_scheduler(&mut linker).unwrap();
    let (run, mut host, _) = super::tests::host("https://typed.example.com");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    host.dns = SystemDns::fake(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        Ok(vec!["8.8.8.8".parse().unwrap()])
    });
    let mut store = Store::new(&engine, host);
    store.set_fuel(100_000_000_000).unwrap();
    let instance = TypedClient::instantiate(&mut store, &component, &linker).unwrap();
    (run, store, instance, calls)
}
#[test]
#[ignore = "requires isolated native typed engine component; no real DNS"]
fn typed_native_dns_abort_orders_and_same_instance_release_before_store_drop() {
    let path =
        std::env::var("PLG4_NATIVE_TYPED_COMPONENT").expect("build-native-typed-task output");
    let (_run, mut store, instance, calls) = instantiate(&path);
    for (scenario, submit, kind) in [
        ("before", 0, "error"),
        ("microtask", 1, "error"),
        ("during", 1, "error"),
        ("none", 1, "success"),
        ("after", 1, "success"),
        ("parallel", 2, "success"),
        ("denied", 1, "success"),
        ("validation", 1, "success"),
        ("prototype", 1, "success"),
        ("quota", 15, "success"),
        ("microtask", 1, "error"),
        ("none", 1, "success"),
        ("dns64", 1, "success"),
    ] {
        if scenario == "dns64" {
            let seen = calls.clone();
            store.data_mut().dns = SystemDns::fake(move |name| {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(if name == "ipv4only.arpa" {
                    vec![
                        "2001:4860:1234::c000:aa".parse().unwrap(),
                        "2001:4860:1234::c000:ab".parse().unwrap(),
                    ]
                } else {
                    vec!["2001:4860:1234::808:808".parse().unwrap()]
                })
            });
        }
        let previous = store.data().dns_submitted;
        let returned = instance.call_run(&mut store, scenario);
        assert_eq!(
            store.data().dns_submitted - previous,
            submit,
            "pre-abort must not submit"
        );
        let value = returned.unwrap();
        println!("TYPED_NATIVE {value}");
        let result: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert_eq!(result["kind"], kind);
        if kind == "error" {
            assert_eq!(result["name"], "AbortError");
        }
        assert_eq!(
            store.data().owner.len(),
            0,
            "physical cleanup before Store Drop"
        );
        assert!(
            store.data().owner.table.is_empty(),
            "actual table empty, not just ledger"
        );
        assert!(
            store.data_mut().owner.check().is_ok(),
            "owner was not closed to manufacture cleanup"
        );
    }
    assert!(calls.load(Ordering::SeqCst) >= 4);
}
