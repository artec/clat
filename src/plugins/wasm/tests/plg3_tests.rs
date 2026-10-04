//! Opt-in author experiment; no experimental engine enters the CLAT binary.
use super::*;

fn runtime() -> (Engine, Linker<PluginState>) {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    let engine = Engine::new(&config).expect("engine");
    let mut linker = Linker::new(&engine);
    add_wasi_to_linker_bounded_clocks(&mut linker).expect("existing WASI linker");
    Plugin::add_to_linker::<PluginState, wasmtime::component::HasSelf<PluginState>>(
        &mut linker,
        |state| state,
    )
    .expect("existing WIT linker");
    (engine, linker)
}

fn instantiate(
    engine: &Engine,
    linker: &Linker<PluginState>,
    component: &wasmtime::component::Component,
    config: Option<&str>,
) -> (Store<PluginState>, Plugin) {
    let state = PluginState {
        bridge: PluginHostBridge::shared(),
        source: PluginSource::Wasm("plg3-engine-probe".into()),
        limits: StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).build(),
        wasi: WasiCtxBuilder::new().build(),
        table: ResourceTable::new(),
        config: config.map(str::to_owned),
        capabilities: Some(PluginCapabilities::default()),
        clock: ClockShared::default(),
    };
    let mut store = Store::new(engine, state);
    store.limiter(|state| &mut state.limits);
    store.set_fuel(LIST_FUEL).expect("list fuel");
    store.set_epoch_deadline(u64::MAX / 2);
    let instance = Plugin::instantiate(&mut store, component, linker).expect("instantiate probe");
    assert_eq!(
        instance
            .clat_plugin_tools()
            .call_list_tools(&mut store)
            .unwrap()
            .len(),
        7
    );
    (store, instance)
}

fn invoke(store: &mut Store<PluginState>, plugin: &Plugin, name: &str) -> Value {
    store.set_fuel(CALL_FUEL).unwrap();
    let text = plugin
        .clat_plugin_tools()
        .call_call(store, name, "{}")
        .unwrap()
        .unwrap();
    serde_json::from_str(&text).expect("probe JSON")
}

fn check_engine(store: &mut Store<PluginState>, plugin: &Plugin) {
    assert_eq!(
        invoke(store, plugin, "async_semantics"),
        serde_json::json!({
            "result": 10,
            "trace": ["outer-before", "inner-before", "leaf", "inner-after", "outer-after", "cleanup-second", "cleanup-first"],
            "cancellation": "aborted"
        })
    );
}

fn check_regex(store: &mut Store<PluginState>, plugin: &Plugin) {
    assert_eq!(
        invoke(store, plugin, "regex"),
        serde_json::json!([
            {"identifier": false, "starts": false, "parts": [""]},
            {"identifier": true, "starts": false, "parts": ["", ""]},
            {"identifier": true, "starts": true, "parts": ["工具"]},
            {"identifier": true, "starts": true, "parts": ["工具", "2"]},
            {"identifier": true, "starts": true, "parts": ["𐐀abc"]},
            {"identifier": false, "starts": false, "parts": ["2bad"]},
            {"identifier": true, "starts": true, "parts": ["e\u{0301}"]},
            {"identifier": false, "starts": false, "parts": ["", ""]},
            {"identifier": false, "starts": true, "parts": ["bad", "name"]},
            {"identifier": false, "starts": true, "parts": ["a", ""]},
            {"identifier": false, "starts": true, "parts": ["a", "b", "工具"]},
        ])
    );
}

fn check_isolation(
    engine: &Engine,
    linker: &Linker<PluginState>,
    component: &wasmtime::component::Component,
) {
    // Disabled StarlingMonkey HTTP has a trap stub: JavaScript cannot catch it.
    // Use a fresh instance, since a canonical ABI trap can invalidate reentry.
    let (mut store, instance) = instantiate(engine, linker, component, None);
    let error = instance
        .clat_plugin_tools()
        .call_call(&mut store, "isolation", "{}")
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("unreachable"),
        "HTTP must fail closed: {error:?}"
    );
}

fn check_fuel(
    engine: &Engine,
    linker: &Linker<PluginState>,
    component: &wasmtime::component::Component,
) {
    let (mut control, control_instance) = instantiate(engine, linker, component, None);
    assert_eq!(
        invoke(&mut control, &control_instance, "burn"),
        serde_json::json!(4_999_950_000u64)
    );
    let (mut store, instance) = instantiate(engine, linker, component, None);
    store.set_fuel(10_000_000).unwrap();
    let error = instance
        .clat_plugin_tools()
        .call_call(&mut store, "burn", "{}")
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("fuel"),
        "expected fuel trap: {error:?}"
    );
}

fn check_write_fence(store: &mut Store<PluginState>, instance: &Plugin) {
    let root = unique_root("plg3-outside-write");
    let target = root.join("outside.txt");
    std::fs::write(&target, "outside marker").unwrap();
    let arguments = serde_json::json!({"path": target, "content": "escape"}).to_string();
    let text = instance
        .clat_plugin_tools()
        .call_call(store, "write_fence", &arguments)
        .unwrap()
        .unwrap();
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["denied"], true);
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("hostTools.write_file")
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), "outside marker");
    std::fs::remove_dir_all(root).unwrap();
}

fn check_cancellation(
    engine: &Engine,
    linker: &Linker<PluginState>,
    component: &wasmtime::component::Component,
) {
    let (mut store, instance) = instantiate(engine, linker, component, None);
    store.set_fuel(CALL_FUEL).unwrap();
    store.set_epoch_deadline(1);
    let cancel = CancelToken::new();
    // Exercise the actual production watcher, not a manually incremented epoch.
    let watcher = CancelWatcher::start(engine, &cancel);
    let worker = std::thread::spawn(move || {
        instance
            .clat_plugin_tools()
            .call_call(&mut store, "spin", "{}")
            .unwrap_err()
    });
    std::thread::sleep(Duration::from_millis(30));
    let begin = Instant::now();
    cancel.cancel();
    let error = worker.join().expect("worker");
    drop(watcher);
    assert!(
        format!("{error:?}").contains("interrupt"),
        "expected epoch trap: {error:?}"
    );
    assert!(
        begin.elapsed() < Duration::from_secs(2),
        "epoch must interrupt promptly"
    );
}

fn measurements(bytes: &[u8]) -> Value {
    let mut samples = Vec::new();
    for _ in 0..3 {
        let begin = Instant::now();
        let (engine, linker) = runtime();
        let component = wasmtime::component::Component::from_binary(&engine, bytes).unwrap();
        let compiled_ms = begin.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let (mut store, instance) = instantiate(&engine, &linker, &component, None);
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        store.set_fuel(CALL_FUEL).unwrap();
        let start = Instant::now();
        instance
            .clat_plugin_tools()
            .call_call(&mut store, "async_semantics", "{}")
            .unwrap()
            .unwrap();
        samples.push(serde_json::json!({
            "compile_and_engine_ms": compiled_ms, "instantiate_and_list_ms": cold_ms,
            "async_call_ms": start.elapsed().as_secs_f64() * 1000.0,
            "async_call_fuel": CALL_FUEL - store.get_fuel().unwrap(),
        }));
    }
    serde_json::json!({"engine_probe_bytes": bytes.len(), "samples": samples,
        "official_search": null, "scope": "engine probe only; no accepted performance claim"})
}

#[test]
#[ignore = "author-generated JS component; armed by sdk/dsh-wasm-spike/test.mjs"]
fn plg3_component_engine_probe() {
    let Some(path) = std::env::var_os("CLAT_PLG3_COMPONENT") else {
        return;
    };
    let bytes = std::fs::read(path).expect("probe component");
    let (engine, linker) = runtime();
    let component =
        wasmtime::component::Component::from_binary(&engine, &bytes).expect("component");
    let (mut store, instance) = instantiate(&engine, &linker, &component, None);
    assert_eq!(
        invoke(&mut store, &instance, "config_states")["state"],
        "missing"
    );
    eprintln!("PLG3: async / config / isolation / fuel");
    check_engine(&mut store, &instance);
    check_regex(&mut store, &instance);
    check_write_fence(&mut store, &instance);
    check_isolation(&engine, &linker, &component);
    check_fuel(&engine, &linker, &component);
    eprintln!("PLG3: production cancel watcher");
    check_cancellation(&engine, &linker, &component);
    for (config, state) in [
        ("{}", "omitted"),
        (r#"{"apiKey":""}"#, "empty"),
        (r#"{"apiKey":"private-plg3-fixture"}"#, "present"),
    ] {
        let (mut store, instance) = instantiate(&engine, &linker, &component, Some(config));
        let result = invoke(&mut store, &instance, "config_states");
        assert_eq!(result["state"], state);
        assert!(!result.to_string().contains("private-plg3-fixture"));
    }
    if let Some(path) = std::env::var_os("CLAT_PLG3_MEASUREMENTS") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&measurements(&bytes)).unwrap(),
        )
        .unwrap();
    }
}
