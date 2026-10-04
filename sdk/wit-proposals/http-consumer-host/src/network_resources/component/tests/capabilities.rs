use super::*;
#[test]
fn capability_clock_real_component_import_does_not_grant_permission() {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Config, Engine, Store};
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let component = Component::new(
        &engine,
        r#"
(component
  (type $clock (instance (export "now" (func (result u64)))))
  (import "wasi:clocks/monotonic-clock@0.2.10" (instance $clock (type $clock)))
  (core func $now (canon lower (func $clock "now")))
  (core module $m
    (import "clock" "now" (func $now (result i64)))
    (func (export "probe") (result i64) call $now))
  (core instance $i (instantiate $m
    (with "clock" (instance (export "now" (func $now))))))
  (func (export "probe") (result u64) (canon lift (core func $i "probe"))))
"#,
    )
    .unwrap();
    let mut linker = Linker::new(&engine);
    HostState::link_scheduler(&mut linker).unwrap();
    for declared in [false, true] {
        let (_run, host, calls) = host_with_clock(ORIGIN, declared);
        let mut store = Store::new(&engine, host);
        store.set_fuel(100_000).unwrap();
        let instance = linker.instantiate(&mut store, &component).unwrap();
        let probe = instance
            .get_typed_func::<(), (u64,)>(&mut store, "probe")
            .unwrap();
        let result = probe.call(&mut store, ());
        assert_eq!(
            result.is_ok(),
            declared,
            "an import cannot grant clock authority"
        );
        assert!(store.data().owner.table.is_empty());
        assert_eq!(store.data().owner.len(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
